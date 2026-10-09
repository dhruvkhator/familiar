//! The Familiar MCP server: tools a bot calls to reach its owner and the rest of Familiar.
//! Served on 127.0.0.1 (random port, streamable HTTP). Each run gets its own bearer token, so a call is always
//! attributed to the run that made it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::crm::{self, Actor, teammate};
use crate::daemon::{Ctx, Signal};
use crate::db::{Bot, Run};
use crate::runner::{self, Ask, Decision, Events, Offer};
use crate::drafts;
use crate::storage;

/// The longest a Familiar tool call may block: an `ask_user` question (30 minutes) plus slack. Drafts don't block (see
/// [`crate::drafts`]). This is the CLIs' MCP tool timeout, so it bounds a hung connector call too.
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(35 * 60);

pub const DRAFT_KINDS: [&str; 6] = ["post", "reply", "email", "dm", "comment", "other"];

/// The draft fields the owner may edit before approving.
pub const DRAFT_EDITABLE: [&str; 3] = ["body", "subject", "to"];

/// What a token is allowed to act as.
#[derive(Clone)]
pub struct Scope {
    pub run: Run,
    pub bot: Bot,
    pub workspace: PathBuf,
    pub events: Events,
    pub cancel: CancellationToken,
}

#[derive(Clone, Default)]
pub struct Registry(Arc<Mutex<HashMap<String, Scope>>>);

impl Registry {
    /// Register a run; the returned guard unregisters it when dropped.
    pub fn register(&self, mut scope: Scope) -> (String, ScopeGuard) {
        // Tool calls still in flight (ask_user, handoff wait) stop when the run ends.
        scope.cancel = scope.cancel.child_token();
        let cancel = scope.cancel.clone();
        let token = Uuid::new_v4().simple().to_string() + &Uuid::new_v4().simple().to_string();
        self.0.lock().unwrap().insert(token.clone(), scope);
        (token.clone(), ScopeGuard { registry: self.clone(), token, cancel })
    }

    fn get(&self, token: &str) -> Option<Scope> {
        self.0.lock().unwrap().get(token).cloned()
    }
}

pub struct ScopeGuard {
    registry: Registry,
    token: String,
    cancel: CancellationToken,
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        self.registry.0.lock().unwrap().remove(&self.token);
        self.cancel.cancel();
    }
}

/// Bind the server's port first (the URL goes into `Ctx`), then [`serve`] it.
pub async fn bind() -> Result<(tokio::net::TcpListener, String)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/mcp", listener.local_addr()?);
    Ok((listener, url))
}

pub fn serve(listener: tokio::net::TcpListener, ctx: Ctx, shutdown: CancellationToken) {
    let mut config = StreamableHttpServerConfig::default();
    config.cancellation_token = shutdown.child_token();
    let registry = ctx.registry.clone();
    let service = StreamableHttpService::new(
        move || Ok(Tools { ctx: ctx.clone(), registry: registry.clone() }),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let app = axum::Router::new().nest_service("/mcp", service);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).with_graceful_shutdown(shutdown.cancelled_owned()).await;
    });
}

#[derive(Clone)]
struct Tools {
    ctx: Ctx,
    registry: Registry,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct Remember {
    /// One durable fact, preference or instruction worth keeping across conversations.
    content: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct Notify {
    /// What your owner should know. Short; it may be pushed to their phone.
    message: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct AskUser {
    /// The question. Blocks until your owner answers (up to 30 minutes).
    question: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ProposeDraft {
    /// post | reply | email | dm | comment | other
    kind: String,
    /// Where it goes: X, Instagram, LinkedIn, Gmail, Reddit, Hacker News...
    channel: String,
    /// Who or what it answers: a handle, an email address, a thread or post URL. Leave out for a new post.
    to: Option<String>,
    /// Email subject (emails only).
    subject: Option<String>,
    /// The exact text to post or send, final and complete: no placeholders.
    body: String,
    /// Workspace files to attach (images for a post, say), as paths in your workspace.
    media: Option<Vec<String>>,
    /// Context for your owner: why this, why now, what it answers. Not part of the text.
    note: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ScheduleTask {
    /// Standard 5-field cron in the owner's local time, e.g. `0 9 * * 1-5` for weekdays at 09:00.
    cron: String,
    /// What to do each time, written as instructions to yourself.
    prompt: String,
    /// `scheduled` (can act, with approvals) or `proactive` (research only, cannot act). Default scheduled.
    kind: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ById {
    id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct Handoff {
    /// Slug of the teammate bot (see list_bots).
    bot_slug: String,
    /// The task, with all the context it needs — it does not see this conversation.
    task: String,
    /// Wait for the result (up to 15 minutes) instead of returning immediately.
    wait: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SaveArtifact {
    /// Path of a file in your workspace (relative or absolute).
    path: String,
}

fn ok(text: impl Into<String>) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![ContentBlock::text(text.into())]))
}

fn fail(text: impl Into<String>) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::error(vec![ContentBlock::text(text.into())]))
}

impl Tools {
    fn scope(&self, parts: &http::request::Parts) -> Result<Scope, ErrorData> {
        parts
            .headers
            .get(http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .and_then(|t| self.registry.get(t))
            .ok_or_else(|| ErrorData::invalid_request("unknown or finished run", None))
    }

    /// The run's scope and its CRM identity, for a tool that changes the CRM (research-only runs only read it).
    fn crm_writer(&self, parts: &http::request::Parts) -> Result<Result<Actor, String>, ErrorData> {
        let s = self.scope(parts)?;
        if s.run.research() {
            return Ok(Err("research-only runs can read the CRM (crm_search, crm_get) but not change it".into()));
        }
        Ok(Ok(Actor::Bot { bot: s.bot.id, run: Some(s.run.id) }))
    }
}

fn answer(r: Result<String, String>) -> Result<CallToolResult, ErrorData> {
    match r {
        Ok(text) => ok(text),
        Err(e) => fail(e),
    }
}

#[tool_router(server_handler)]
impl Tools {
    #[tool(description = "Save something to your long-term memory. Use for durable facts and preferences your owner \
        teaches you, not for task progress.")]
    async fn remember(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<Remember>,
    ) -> Result<CallToolResult, ErrorData> {
        let s = self.scope(&parts)?;
        let content = p.content.trim();
        if content.is_empty() || content.len() > 2000 {
            return fail("content must be 1-2000 characters");
        }
        // Dream runs propose; the owner accepts or rejects them under "What I learned".
        // Everything a bot saves waits for the owner under "What I learned": page text or a webhook body must not be
        // able to plant instructions for future runs.
        let status = "proposed";
        match self.ctx.db.insert_memory(s.bot.id, content, "bot", status).await {
            Ok(_) => ok("Proposed. Your owner reviews it under \"What I learned\"; once accepted it is part of your instructions."),
            Err(e) => fail(format!("could not save: {e:#}")),
        }
    }

    #[tool(description = "Send your owner a message right now (also pushed to their desktop/phone). Use when something \
        needs their attention or a long task finished; don't use it for your final answer.")]
    async fn notify_user(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<Notify>,
    ) -> Result<CallToolResult, ErrorData> {
        let s = self.scope(&parts)?;
        if let Err(e) = self.ctx.db.insert_message(s.run.thread_id, "assistant", &p.message, s.run.id).await {
            return fail(format!("could not notify: {e:#}"));
        }
        self.ctx.signal(Signal::Notify { bot: s.bot.name.clone(), message: p.message, thread: s.run.thread_id });
        ok("Delivered.")
    }

    #[tool(description = "Ask your owner a question and wait for the answer (up to 30 minutes). Use when you are \
        blocked on a decision only they can make.")]
    async fn ask_user(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<AskUser>,
    ) -> Result<CallToolResult, ErrorData> {
        let s = self.scope(&parts)?;
        if s.run.research() {
            return fail("research-only runs cannot ask questions; use notify_user");
        }
        let input = json!({ "question": p.question });
        let ask = Ask {
            tool_use_id: None,
            tool: "ask_user",
            input: &input,
            reason: None,
            offer: Offer::default(),
            timeout: runner::APPROVAL_TIMEOUT,
            preview: None,
        };
        match runner::ask_human(&self.ctx, &s.run, ask, &s.events, &s.cancel).await {
            Ok(Decision { status, response: Some(answer), .. }) if status == "approved" => ok(format!("Owner answered: {answer}")),
            Ok(d) if d.status == "approved" => ok("Owner acknowledged without a written answer."),
            Ok(d) => fail(format!("No answer ({}). Proceed with your best judgement or stop.", d.status)),
            Err(e) => fail(format!("could not ask: {e:#}")),
        }
    }

    // Read-only for the world outside Familiar (nothing is posted or sent by this call), which also lets the CLI run a
    // batch of drafts side by side so the owner can review them together.
    #[tool(
        description = "Propose a post, reply, email, DM or comment to your owner BEFORE it goes anywhere. Returns at once:         the draft waits in your owner's queue (up to 7 days) while you carry on with other work. Don't post or send it         until their decision arrives as a message from Familiar later: approved (post or send EXACTLY the text that         message gives, never your own version), changes asked (propose a revised draft) or rejected (drop it). Propose         several drafts in one turn (one call each) so your owner can review them together.",
        annotations(read_only_hint = true)
    )]
    async fn propose_draft(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<ProposeDraft>,
    ) -> Result<CallToolResult, ErrorData> {
        let s = self.scope(&parts)?;
        if s.run.research() {
            return fail("research-only runs cannot propose drafts; write it to a file and use notify_user");
        }
        let input = match draft_input(&p, &s.workspace) {
            Ok(v) => v,
            Err(e) => return fail(e),
        };
        // Never to someone who asked not to be contacted (checked again before an approved draft is passed on).
        if let Some(to) = input["to"].as_str() {
            match crm::teammate::do_not_contact(&self.ctx.db, to).await {
                Ok(Some(name)) => return fail(crm::teammate::dnc_refusal(to, &name)),
                Ok(None) => {}
                Err(e) => return fail(format!("could not check the do-not-contact list, so not proposed: {e}")),
            }
        }
        // Images and files of the draft go into the conversation, so the owner can open them before deciding.
        for m in input["media"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if let Some(path) = inside(&s.workspace, m) {
                let _ = storage::save(&self.ctx, &s.run, &path, &s.events).await;
            }
        }
        let note = p.note.as_deref().map(str::trim).filter(|n| !n.is_empty());
        let editable = DRAFT_EDITABLE.map(String::from).to_vec();
        // Queued, not awaited: the decision comes back as a follow-up run (crate::drafts::sweep).
        let id = match self
            .ctx
            .db
            .create_approval(&s.run, None, "propose_draft", &input, note, &editable, None, drafts::DRAFT_TIMEOUT, None)
            .await
        {
            Ok(id) => id,
            Err(e) => return fail(format!("could not propose: {e:#}")),
        };
        runner::send(
            &s.events,
            "approval",
            json!({ "approval_id": id, "tool_name": "propose_draft", "input": input, "status": "pending", "reason": note }),
        );
        self.ctx.signal(Signal::ApprovalPending { bot: s.bot.name.clone(), tool: "propose_draft".into() });
        ok(drafts::queued_message(id))
    }

    #[tool(description = "Schedule a recurring task for yourself (cron in the owner's local time).")]
    async fn schedule_task(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<ScheduleTask>,
    ) -> Result<CallToolResult, ErrorData> {
        let s = self.scope(&parts)?;
        if s.run.research() {
            return fail("research-only runs cannot create schedules");
        }
        let kind = p.kind.as_deref().unwrap_or("scheduled");
        if !matches!(kind, "scheduled" | "proactive") {
            return fail("kind must be scheduled or proactive");
        }
        if p.cron.split_whitespace().count() != 5 || croner::Cron::from_str(&p.cron).is_err() {
            return fail("cron must be a valid 5-field expression");
        }
        match self.ctx.db.insert_schedule(s.bot.id, &p.cron, &p.prompt, kind).await {
            Ok(id) => ok(format!("Scheduled ({id}).")),
            Err(e) => fail(format!("could not schedule: {e:#}")),
        }
    }

    #[tool(description = "List your schedules.")]
    async fn list_schedules(&self, Extension(parts): Extension<http::request::Parts>) -> Result<CallToolResult, ErrorData> {
        let s = self.scope(&parts)?;
        match self.ctx.db.list_schedules(s.bot.id).await {
            Ok(v) => ok(v.to_string()),
            Err(e) => fail(format!("{e:#}")),
        }
    }

    #[tool(description = "Delete one of your schedules by id.")]
    async fn cancel_schedule(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<ById>,
    ) -> Result<CallToolResult, ErrorData> {
        let s = self.scope(&parts)?;
        if s.run.research() {
            return fail("research-only runs cannot change schedules");
        }
        let Ok(id) = p.id.parse() else { return fail("bad id") };
        match self.ctx.db.delete_schedule(s.bot.id, id).await {
            Ok(true) => ok("Deleted."),
            Ok(false) => fail("no such schedule"),
            Err(e) => fail(format!("{e:#}")),
        }
    }

    #[tool(description = "List your teammate bots (slug, name, what they do).")]
    async fn list_bots(&self, Extension(parts): Extension<http::request::Parts>) -> Result<CallToolResult, ErrorData> {
        self.scope(&parts)?;
        match self.ctx.db.list_bots().await {
            Ok(v) => ok(v.to_string()),
            Err(e) => fail(format!("{e:#}")),
        }
    }

    #[tool(description = "Hand a task to a teammate bot. It works in its own computer with its own tools and memory. \
        Set wait=true to get its result back.")]
    async fn handoff(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<Handoff>,
    ) -> Result<CallToolResult, ErrorData> {
        let s = self.scope(&parts)?;
        if s.run.research() {
            return fail("research-only runs cannot hand off work");
        }
        let target = match self.ctx.db.bot_by_slug(&p.bot_slug).await {
            Ok(Some(b)) if b.id != s.bot.id => b,
            Ok(Some(_)) => return fail("you cannot hand off to yourself"),
            Ok(None) => return fail("no bot with that slug (see list_bots)"),
            Err(e) => return fail(format!("{e:#}")),
        };
        let title: String = format!("From {}: {}", s.bot.name, p.task).chars().take(80).collect();
        let prompt = format!("Task handed off to you by your teammate {}:\n\n{}", s.bot.name, p.task);
        let run = match self.ctx.db.handoff(target.id, &title, &prompt, s.run.id).await {
            Ok(r) => r,
            Err(e) => return fail(format!("{e:#}")),
        };
        if !p.wait.unwrap_or(false) {
            return ok(format!("Handed off to {} (run {run}). It will work on it independently.", target.name));
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15 * 60);
        loop {
            match self.ctx.db.run_outcome(run).await {
                Ok((status, _, text)) if status == "succeeded" => {
                    return ok(format!("{} finished:\n{}", target.name, text.unwrap_or_default()));
                }
                Ok((status, error, _)) if matches!(status.as_str(), "failed" | "cancelled") => {
                    return fail(format!("{} {status}: {}", target.name, error.unwrap_or_default()));
                }
                Ok(_) => {}
                Err(e) => return fail(format!("{e:#}")),
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {}
                _ = tokio::time::sleep_until(deadline) => return ok(format!("Still running after 15 min (run {run}).")),
                _ = s.cancel.cancelled() => return fail("cancelled"),
            }
        }
    }

    #[tool(
        description = "Search your owner's CRM: companies, contacts and deals (with ids, stages, fit scores, tags and \
            do-not-contact flags). Use it before adding anything, so you update a record instead of duplicating it.",
        annotations(read_only_hint = true)
    )]
    async fn crm_search(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<teammate::SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.scope(&parts)?;
        answer(teammate::search(&self.ctx.db, p).await)
    }

    #[tool(
        description = "Read one CRM record in full: a company with its contacts, deals and recent activity; a contact \
            with its deals and activity; or a deal with its activity.",
        annotations(read_only_hint = true)
    )]
    async fn crm_get(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<teammate::GetArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.scope(&parts)?;
        answer(teammate::get(&self.ctx.db, p).await)
    }

    #[tool(
        description = "The sales pipeline: for each deal stage, how many deals, their total value and the newest ten.",
        annotations(read_only_hint = true)
    )]
    async fn crm_pipeline(&self, Extension(parts): Extension<http::request::Parts>) -> Result<CallToolResult, ErrorData> {
        self.scope(&parts)?;
        answer(teammate::pipeline(&self.ctx.db).await)
    }

    #[tool(description = "Add a company to the CRM or update one (same domain = same company). A new company needs \
        source_urls. Fields your owner set themselves keep their values; tags and source_urls are added to.")]
    async fn crm_upsert_company(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<teammate::CompanyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.crm_writer(&parts)? {
            Ok(actor) => answer(teammate::upsert_company(&self.ctx.db, &actor, p).await),
            Err(e) => fail(e),
        }
    }

    #[tool(description = "Add a person to the CRM or update one (same email = same person). A new contact needs \
        source_urls. Only public business contact details; never a guessed email. Set do_not_contact when they ask \
        not to be contacted (you can never clear it).")]
    async fn crm_upsert_contact(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<teammate::ContactArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.crm_writer(&parts)? {
            Ok(actor) => answer(teammate::upsert_contact(&self.ctx.db, &actor, p).await),
            Err(e) => fail(e),
        }
    }

    #[tool(description = "Add a deal (an opportunity with a company) or update one. Use crm_move_deal to change the \
        stage of an existing deal.")]
    async fn crm_upsert_deal(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<teammate::DealArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.crm_writer(&parts)? {
            Ok(actor) => answer(teammate::upsert_deal(&self.ctx.db, &actor, p).await),
            Err(e) => fail(e),
        }
    }

    #[tool(description = "Move a deal to another stage (new, researching, contacted, replied, meeting, proposal, won, \
        lost), with a one-line note on why. The move is logged on the deal's timeline.")]
    async fn crm_move_deal(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<teammate::MoveArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.crm_writer(&parts)? {
            Ok(actor) => answer(teammate::move_deal(&self.ctx.db, &actor, p).await),
            Err(e) => fail(e),
        }
    }

    #[tool(description = "Add an entry to a company's, contact's or deal's timeline: research, a note, an email or DM \
        you sent (name the approved draft) or received, a call or a meeting.")]
    async fn crm_log_activity(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<teammate::ActivityArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.crm_writer(&parts)? {
            Ok(actor) => answer(teammate::log_activity(&self.ctx.db, &actor, p).await),
            Err(e) => fail(e),
        }
    }

    #[tool(description = "Attach a file from your workspace to this conversation so your owner can open it.")]
    async fn save_artifact(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<SaveArtifact>,
    ) -> Result<CallToolResult, ErrorData> {
        let s = self.scope(&parts)?;
        let Some(path) = inside(&s.workspace, &p.path) else {
            return fail("file not found inside your workspace");
        };
        match storage::save(&self.ctx, &s.run, &path, &s.events).await {
            Ok(id) => ok(format!("Attached ({id}).")),
            Err(e) => fail(format!("could not attach: {e:#}")),
        }
    }
}

/// Check a draft and turn it into the approval's input: `{kind, channel, to?, subject?, body, media?}` (the note is
/// the approval's reason). Media paths must be files inside the workspace and come back relative to it.
fn draft_input(p: &ProposeDraft, workspace: &Path) -> Result<Value, String> {
    let kind = p.kind.trim().to_ascii_lowercase();
    if !DRAFT_KINDS.contains(&kind.as_str()) {
        return Err(format!("kind must be one of: {}", DRAFT_KINDS.join(", ")));
    }
    let field = |v: Option<&str>, name: &str, max: usize| -> Result<Option<String>, String> {
        match v.map(str::trim).filter(|v| !v.is_empty()) {
            Some(v) if v.chars().count() > max => Err(format!("{name} is too long (max {max} characters)")),
            v => Ok(v.map(str::to_owned)),
        }
    };
    let channel = field(Some(&p.channel), "channel", 40)?.ok_or("channel must not be empty (X, Gmail, LinkedIn...)")?;
    let body = field(Some(&p.body), "body", 20_000)?.ok_or("body must not be empty: give the exact text")?;
    let to = field(p.to.as_deref(), "to", 500)?;
    let subject = field(p.subject.as_deref(), "subject", 300)?;
    field(p.note.as_deref(), "note", 2000)?;
    let media = p.media.as_deref().unwrap_or_default();
    if media.len() > 10 {
        return Err("at most 10 media files".into());
    }
    let root = workspace.canonicalize().map_err(|e| format!("workspace: {e}"))?;
    let mut files = Vec::new();
    for m in media {
        let path = inside(workspace, m).ok_or_else(|| format!("media file not found inside your workspace: {m}"))?;
        let rel = path.strip_prefix(&root).unwrap_or(&path);
        files.push(rel.to_string_lossy().replace('\\', "/"));
    }
    let mut v = json!({ "kind": kind, "channel": channel, "body": body });
    if let Some(to) = to {
        v["to"] = json!(to);
    }
    if let Some(subject) = subject {
        v["subject"] = json!(subject);
    }
    if !files.is_empty() {
        v["media"] = json!(files);
    }
    Ok(v)
}

/// Resolve `path` and make sure it stays inside the workspace (no `..` or symlink escapes).
fn inside(workspace: &Path, path: &str) -> Option<PathBuf> {
    let root = workspace.canonicalize().ok()?;
    let p = Path::new(path);
    let full = if p.is_absolute() { p.to_path_buf() } else { root.join(p) };
    let full = full.canonicalize().ok()?;
    (full.starts_with(&root) && full.is_file()).then_some(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(kind: &str, channel: &str, body: &str) -> ProposeDraft {
        ProposeDraft {
            kind: kind.into(),
            channel: channel.into(),
            to: None,
            subject: None,
            body: body.into(),
            media: None,
            note: None,
        }
    }

    fn workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("familiar-draft-test-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("media")).unwrap();
        std::fs::write(dir.join("media").join("shot.png"), b"png").unwrap();
        dir
    }

    #[test]
    fn draft_arguments_are_checked() {
        let ws = workspace();
        let v = draft_input(&draft(" Post ", " X ", "  Shipping today.  "), &ws).unwrap();
        assert_eq!(v, json!({ "kind": "post", "channel": "X", "body": "Shipping today." }));

        let mut d = draft("email", "Gmail", "Hi Sam");
        d.to = Some("sam@example.com".into());
        d.subject = Some("  Quick question ".into());
        d.media = Some(vec!["media/shot.png".into()]);
        d.note = Some("follow-up".into());
        let v = draft_input(&d, &ws).unwrap();
        assert_eq!(v["to"], "sam@example.com");
        assert_eq!(v["subject"], "Quick question");
        assert_eq!(v["media"], json!(["media/shot.png"]));
        assert!(v.get("note").is_none(), "the note is the approval's reason, not part of the draft");

        assert!(draft_input(&draft("tweet", "X", "hi"), &ws).unwrap_err().contains("kind must be one of"));
        assert!(draft_input(&draft("post", " ", "hi"), &ws).unwrap_err().contains("channel"));
        assert!(draft_input(&draft("post", "X", " \n "), &ws).unwrap_err().contains("body must not be empty"));
        assert!(draft_input(&draft("post", &"x".repeat(41), "hi"), &ws).unwrap_err().contains("channel is too long"));
        assert!(draft_input(&draft("post", "X", &"y".repeat(20_001)), &ws).unwrap_err().contains("body is too long"));
        let mut d = draft("post", "X", "hi");
        d.media = Some(vec!["../outside.png".into()]);
        assert!(draft_input(&d, &ws).unwrap_err().contains("not found inside your workspace"));
        d.media = Some(vec!["media/shot.png".into(); 11]);
        assert!(draft_input(&d, &ws).unwrap_err().contains("at most 10"));
        let mut d = draft("post", "X", "hi");
        d.note = Some("n".repeat(2001));
        assert!(draft_input(&d, &ws).unwrap_err().contains("note is too long"));
        let _ = std::fs::remove_dir_all(ws);
    }
}
