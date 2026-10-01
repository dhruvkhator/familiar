//! The Familiar MCP server: tools a bot calls to reach its owner and the rest of Familiar.
//! Served on 127.0.0.1 (random port, streamable HTTP). Each run gets its own bearer token, so a call is always
//! attributed to the run that made it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::daemon::{Ctx, Signal};
use crate::db::{Bot, Run};
use crate::runner::{self, Events};
use crate::storage;

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
        match runner::ask_human(&self.ctx, &s.run, None, "ask_user", &input, None, &s.events, &s.cancel).await {
            Ok((status, Some(answer))) if status == "approved" => ok(format!("Owner answered: {answer}")),
            Ok((status, _)) if status == "approved" => ok("Owner acknowledged without a written answer."),
            Ok((status, _)) => fail(format!("No answer ({status}). Proceed with your best judgement or stop.")),
            Err(e) => fail(format!("could not ask: {e:#}")),
        }
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

/// Resolve `path` and make sure it stays inside the workspace (no `..` or symlink escapes).
fn inside(workspace: &Path, path: &str) -> Option<PathBuf> {
    let root = workspace.canonicalize().ok()?;
    let p = Path::new(path);
    let full = if p.is_absolute() { p.to_path_buf() } else { root.join(p) };
    let full = full.canonicalize().ok()?;
    (full.starts_with(&root) && full.is_file()).then_some(full)
}
