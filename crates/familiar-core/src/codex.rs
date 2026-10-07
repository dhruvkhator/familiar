//! The Codex engine: one `codex app-server` per run, spoken to over JSON-RPC on stdio (app-server protocol v2:
//! `thread/start|resume` → `turn/start` → `item/*` notifications; approvals arrive as server requests).
//!
//! Verified against codex-cli 0.159.3 (ChatGPT login). codex-cli 0.46 only speaks the older v1 protocol
//! (`newConversation`/`sendUserMessage`) and the ChatGPT backend no longer serves any model to it, so it is not
//! supported: the run fails with an upgrade hint.
//!
//! Permission mapping (so the owner's rules and the always-human list apply unchanged):
//! - shell commands → `Bash {command}` (the shell wrapper Codex adds is stripped), patches → `Edit {file_path,
//!   file_paths, changes}`, MCP tool calls → `mcp__<server>__<tool>` with the tool arguments.
//! - Normal runs: `approvalPolicy untrusted` + `danger-full-access`. Codex then asks before every command that is not
//!   on its known-safe (read-only) list and before every patch; Familiar decides. (With a sandbox Codex would run
//!   sandboxed writes without asking, which would skip the owner's approvals.)
//! - Research runs: `read-only` sandbox and every request is denied by [`runner::decide_tool`].
//! - Every MCP server except Familiar's own runs with `default_tools_approval_mode = "prompt"`: each call comes back to us
//!   as an `mcpServer/elicitation/request` and goes through the same decision.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::daemon::Ctx;
use crate::db::{Bot, Rule, Run, Thread};
use crate::folders::Folder;
use crate::permissions;
use crate::runner::{self, Deltas, Events, Outcome, send};

const INTERRUPT_GRACE: Duration = Duration::from_secs(10);
/// initialize → thread/start → turn/start. MCP servers start in the background, so this is quick.
const SETUP_TIMEOUT: Duration = Duration::from_secs(120);
/// Codex's own features that would give the bot capabilities outside Familiar's approvals (ChatGPT apps, its own
/// browser/computer use, plugins, sub-agents) or ignore Familiar's memory. Unknown names are ignored by Codex.
const DISABLED_FEATURES: &[&str] = &[
    "apps",
    "plugins",
    "remote_plugin",
    "browser_use",
    "browser_use_external",
    "computer_use",
    "image_generation",
    "multi_agent",
    "tool_suggest",
    "skill_mcp_dependency_install",
    "memories",
];

/// `codex` → the executable to spawn. On Windows npm installs a `codex.cmd` shim, which `CreateProcess` won't find
/// from a bare name.
pub fn resolve_bin(bin: &str) -> PathBuf {
    let path = PathBuf::from(bin);
    if !cfg!(windows) || path.extension().is_some() {
        return path;
    }
    if path.components().count() > 1 {
        for ext in ["exe", "cmd"] {
            let p = path.with_extension(ext);
            if p.is_file() {
                return p;
            }
        }
        return path;
    }
    for dir in std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default() {
        for ext in ["exe", "cmd"] {
            let p = dir.join(format!("{bin}.{ext}"));
            if p.is_file() {
                return p;
            }
        }
    }
    path
}

/// A Claude model alias (the default for new bots) means "Codex's default model" on this engine.
fn wants_default_model(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    m.is_empty() || matches!(m.as_str(), "default" | "auto" | "sonnet" | "opus" | "haiku" | "fable") || m.starts_with("claude")
}

// ---------------------------------------------------------------------------------------------------------------
// JSON-RPC over stdio

enum Incoming {
    Request { id: Value, method: String, params: Value },
    Notification { method: String, params: Value },
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;

/// Cheap handle for sending to the app-server (requests, notifications, replies to its requests).
#[derive(Clone)]
struct Rpc {
    out: mpsc::UnboundedSender<Value>,
    pending: Pending,
    next: Arc<AtomicI64>,
}

impl Rpc {
    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let _ = self.out.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(anyhow!("{method}: {e}")),
            Err(_) => Err(anyhow!("{method}: codex app-server exited")),
        }
    }

    /// A request whose answer we don't wait for (turn/interrupt while shutting down).
    fn call_nowait(&self, method: &str, params: Value) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let _ = self.out.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    fn notify(&self, method: &str) {
        let _ = self.out.send(json!({ "jsonrpc": "2.0", "method": method }));
    }

    fn reply(&self, id: Value, result: Value) {
        let _ = self.out.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn reply_error(&self, id: Value, message: &str) {
        let _ = self.out.send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": message } }));
    }
}

struct Process {
    child: Child,
    rpc: Rpc,
    incoming: mpsc::UnboundedReceiver<Incoming>,
    stderr: JoinHandle<String>,
}

fn spawn(bin: &str, cwd: &Path) -> Result<Process> {
    let exe = resolve_bin(bin);
    let mut cmd = Command::new(&exe);
    // Same rule as Claude: the owner's ChatGPT sign-in, never an API key that happens to be in the environment.
    if std::env::var("FAMILIAR_ALLOW_API_KEY").as_deref() != Ok("1") {
        cmd.env_remove("OPENAI_API_KEY").env_remove("CODEX_API_KEY");
    }
    cmd.arg("app-server")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = cmd.spawn().with_context(|| format!("spawning {} app-server", exe.display()))?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let mut stderr_pipe = child.stderr.take().context("no stderr")?;

    let (out, mut out_rx) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if stdin.write_all(format!("{msg}\n").as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
    });
    let pending: Pending = Arc::default();
    let (in_tx, incoming) = mpsc::unbounded_channel();
    {
        let pending = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(mut msg) = serde_json::from_str::<Value>(&line) else { continue };
                let method = msg["method"].as_str().map(str::to_owned);
                let params = msg.get_mut("params").map(Value::take).unwrap_or(Value::Null);
                match (method, msg.get("id").cloned()) {
                    (Some(method), Some(id)) => {
                        let _ = in_tx.send(Incoming::Request { id, method, params });
                    }
                    (Some(method), None) => {
                        let _ = in_tx.send(Incoming::Notification { method, params });
                    }
                    (None, Some(id)) => {
                        let Some(tx) = id.as_i64().and_then(|id| pending.lock().unwrap().remove(&id)) else { continue };
                        let result = match msg.get("error") {
                            Some(e) => Err(e["message"].as_str().map(str::to_owned).unwrap_or_else(|| e.to_string())),
                            None => Ok(msg.get_mut("result").map(Value::take).unwrap_or(Value::Null)),
                        };
                        let _ = tx.send(result);
                    }
                    (None, None) => {}
                }
            }
            // EOF: fail calls still waiting.
            pending.lock().unwrap().clear();
        });
    }
    let stderr = tokio::spawn(async move {
        let mut buf = String::new();
        let _ = (&mut stderr_pipe).take(64 * 1024).read_to_string(&mut buf).await;
        let _ = tokio::io::copy(&mut stderr_pipe, &mut tokio::io::sink()).await;
        buf
    });
    Ok(Process { child, rpc: Rpc { out, pending, next: Arc::new(AtomicI64::new(1)) }, incoming, stderr })
}

impl Process {
    /// Kill the whole tree (MCP servers started through npx are grandchildren).
    async fn kill(&mut self) {
        #[cfg(windows)]
        if let Some(pid) = self.child.id() {
            let _ = Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .creation_flags(0x0800_0000)
                .output()
                .await;
        }
        let _ = self.child.kill().await;
    }

    /// Close stdin (app-server exits on EOF), wait up to `grace`, then kill. Returns captured stderr.
    async fn finish(mut self, grace: Duration) -> String {
        self.rpc.out = mpsc::unbounded_channel().0;
        self.incoming.close();
        let _ = tokio::time::timeout(grace, self.child.wait()).await;
        // Exited or not: make sure its MCP server children are gone too.
        self.kill().await;
        self.stderr.await.unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------------------------------------------
// One run

/// Run on the Codex engine. Resumes the thread's Codex conversation, or starts one (seeded with recent history when
/// the old one is gone).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute(
    ctx: &Ctx,
    run: &Run,
    bot: &Bot,
    thread: &Thread,
    cwd: &Path,
    system_prompt: &Path,
    mut prompt: String,
    rules: &[Rule],
    mcp: &Value,
    folders: &[Folder],
    shots: &Path,
    cancel: &CancellationToken,
    events: &Events,
) -> Result<Outcome> {
    let instructions = std::fs::read_to_string(system_prompt).context("reading the bot's instructions")?;
    // Each dream starts fresh, as on the Claude engine.
    let mut resume = thread.codex_thread_id.clone().filter(|_| run.kind != "dream");
    loop {
        let job = Job { ctx, run, bot, cwd, instructions: &instructions, rules, mcp, folders, shots, cancel, events };
        let outcome = job.drive(resume.as_deref(), &prompt).await?;
        if outcome.missing_session && resume.is_some() {
            warn!(run = %run.id, "codex thread missing, starting a fresh one");
            resume = None;
            prompt = runner::seeded_prompt(ctx, run).await?;
            continue;
        }
        return Ok(outcome);
    }
}

struct Job<'a> {
    ctx: &'a Ctx,
    run: &'a Run,
    bot: &'a Bot,
    cwd: &'a Path,
    instructions: &'a str,
    rules: &'a [Rule],
    mcp: &'a Value,
    /// The folders the owner shared (see crate::folders).
    folders: &'a [Folder],
    shots: &'a Path,
    cancel: &'a CancellationToken,
    events: &'a Events,
}

/// What the event loop collects for the final outcome.
#[derive(Default)]
struct Turn {
    /// In-flight items by id (approval requests refer to them).
    items: HashMap<String, Value>,
    final_text: Option<String>,
    last_text: Option<String>,
    usage: [u64; 5],
    error: Option<String>,
    status: Option<String>,
    rate_limits: Option<Value>,
}

enum Setup {
    Started { thread: String, turn: String },
    MissingThread,
}

impl Job<'_> {
    async fn drive(&self, resume: Option<&str>, prompt: &str) -> Result<Outcome> {
        let mut proc = spawn(&self.ctx.cfg.codex_bin, self.cwd)?;
        let rpc = proc.rpc.clone();
        let started = Instant::now();
        let setup = tokio::select! {
            r = tokio::time::timeout(SETUP_TIMEOUT, self.setup(&rpc, resume, prompt)) => match r {
                Ok(r) => r,
                Err(_) => Err(anyhow!("codex app-server did not start a turn within {} s", SETUP_TIMEOUT.as_secs())),
            },
            _ = self.cancel.cancelled() => Err(anyhow!("cancelled")),
        };
        let (thread_id, turn_id) = match setup {
            Ok(Setup::Started { thread, turn }) => (thread, turn),
            Ok(Setup::MissingThread) => {
                // Our clone of the sender would keep stdin open, so finish() would always wait out the grace period.
                drop(rpc);
                proc.finish(Duration::from_secs(5)).await;
                return Ok(Outcome { missing_session: true, ..Outcome::failed("codex thread not found".into()) });
            }
            Err(e) => {
                drop(rpc);
                let stderr = proc.finish(Duration::from_secs(5)).await;
                let stderr = stderr.lines().filter(|l| !l.contains("unrecognized configuration") && !l.contains("is ignored")).collect::<Vec<_>>().join("\n");
                return Err(if stderr.trim().is_empty() { e } else { e.context(stderr.trim().to_owned()) });
            }
        };

        // Approval tasks die with this turn, so a pending approval never outlives the process.
        let scope = self.cancel.child_token();
        let _scope_guard = scope.clone().drop_guard();
        let mut deltas = Deltas::new(self.ctx, self.run.id);
        let mut seen_shots = runner::list_files(self.shots).into_iter().collect();
        let mut turn = Turn::default();
        let mut kill_at: Option<tokio::time::Instant> = None;
        let mut done = false;
        while !done {
            let msg = tokio::select! {
                m = proc.incoming.recv() => m,
                _ = self.cancel.cancelled(), if kill_at.is_none() => {
                    rpc.call_nowait("turn/interrupt", json!({ "threadId": thread_id, "turnId": turn_id }));
                    kill_at = Some(tokio::time::Instant::now() + INTERRUPT_GRACE);
                    continue;
                }
                _ = tokio::time::sleep_until(kill_at.unwrap_or_else(tokio::time::Instant::now)), if kill_at.is_some() => {
                    proc.kill().await;
                    return Err(anyhow!("cancelled"));
                }
            };
            let Some(msg) = msg else { break };
            match msg {
                Incoming::Notification { method, params } => {
                    if !method.contains("elta") {
                        deltas.flush();
                    }
                    done = self.notification(&method, params, &turn_id, &mut turn, &mut deltas, &mut seen_shots);
                }
                Incoming::Request { id, method, params } => self.request(&rpc, id, &method, params, &turn, &scope),
            }
        }
        deltas.flush();
        scope.cancel();
        drop(rpc); // with every sender gone stdin closes and the app-server exits on its own
        let stderr = proc.finish(Duration::from_secs(5)).await;

        let Some(status) = turn.status else {
            let msg = turn.error.unwrap_or_else(|| {
                if stderr.trim().is_empty() { "codex exited before the turn finished".into() } else { stderr.trim().to_owned() }
            });
            return Ok(Outcome::failed(msg));
        };
        let ok = status == "completed";
        let text = turn.final_text.or(turn.last_text);
        let [input, cached, output, reasoning, total] = turn.usage;
        let usage = json!({ "engine": "codex", "input_tokens": input, "cached_input_tokens": cached,
                            "output_tokens": output, "reasoning_output_tokens": reasoning, "total_tokens": total });
        send(
            self.events,
            "result",
            json!({ "subtype": status, "text": text, "num_turns": 1, "cost_usd": null,
                    "duration_ms": started.elapsed().as_millis() as u64, "permission_denials": null }),
        );
        Ok(Outcome {
            ok,
            error: (!ok).then(|| turn.error.clone().unwrap_or_else(|| format!("codex turn {status}"))),
            text: if ok { text } else { None },
            cost: None,
            usage: Some(usage),
            missing_session: false,
        })
    }

    async fn setup(&self, rpc: &Rpc, resume: Option<&str>, prompt: &str) -> Result<Setup> {
        let init = rpc
            .call("initialize", json!({ "clientInfo": { "name": "familiar", "title": "familiar", "version": env!("CARGO_PKG_VERSION") } }))
            .await?;
        rpc.notify("initialized");
        let agent = init["userAgent"].as_str().unwrap_or_default().to_owned();
        // The owner's own Codex MCP servers are not the bot's: switch them off for this thread.
        let user_servers: Vec<String> = match rpc.call("config/read", json!({ "cwd": self.cwd })).await {
            Ok(v) => v["config"]["mcp_servers"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default(),
            Err(e) => {
                return Err(e.context(format!(
                    "this Codex CLI does not speak the app-server v2 protocol Familiar needs ({agent}); \
                     update it with `npm i -g @openai/codex@latest`"
                )));
            }
        };
        let model = if wants_default_model(&self.bot.model) { default_model(rpc).await } else { Some(self.bot.model.clone()) };
        let _ = rpc
            .call("skills/extraRoots/set", json!({ "extraRoots": [self.cwd.join(".claude").join("skills")] }))
            .await;

        let research = self.run.research();
        let mut config = Map::new();
        for f in DISABLED_FEATURES {
            config.insert(format!("features.{f}"), json!(false));
        }
        // Like `--restricted` on Claude: no AGENTS.md from the workspace (the bot's instructions are ours) and no
        // owner notification hooks firing for bot turns.
        config.insert("project_doc_max_bytes".into(), json!(0));
        config.insert("notify".into(), json!([]));
        // Let the first turn wait for slow MCP servers (npx cold starts) instead of starting without their tools.
        config.insert("mcp_optional_startup_grace_ms".into(), json!(120_000));
        // Only the read & write folders are writable roots. Codex applies them only in its workspace-write sandbox;
        // Familiar runs it without one (below), so every patch comes back for approval, where read-only folders are
        // refused (crate::folders::write_refusal). Codex never limits reading: see crate::folders.
        let writable: Vec<String> = self.folders.iter().filter(|f| f.write).map(|f| f.path.display().to_string()).collect();
        if !writable.is_empty() {
            config.insert("sandbox_workspace_write.writable_roots".into(), json!(writable));
        }
        if research && cfg!(windows) {
            // The elevated Windows sandbox needs a one-time admin setup; the unelevated one works out of the box.
            config.insert("windows.sandbox".into(), json!("unelevated"));
        }
        let mut servers = mcp_servers(self.mcp);
        for name in user_servers {
            servers.entry(name).or_insert_with(|| json!({ "enabled": false }));
        }
        config.insert("mcp_servers".into(), Value::Object(servers));
        let params = json!({
            "model": model,
            "cwd": self.cwd,
            "approvalPolicy": "untrusted",
            "sandbox": if research { "read-only" } else { "danger-full-access" },
            "developerInstructions": self.instructions,
            "config": config,
        });

        let thread = match resume {
            Some(id) => {
                let mut p = params;
                p["threadId"] = json!(id);
                match rpc.call("thread/resume", p).await {
                    Ok(_) => id.to_owned(),
                    Err(e) if format!("{e:#}").contains("no rollout found") => return Ok(Setup::MissingThread),
                    Err(e) => return Err(e),
                }
            }
            None => {
                let r = rpc.call("thread/start", params).await?;
                let id = r["thread"]["id"].as_str().context("thread/start returned no thread id")?.to_owned();
                self.ctx.db.set_codex_thread(self.run.thread_id, Some(&id)).await?;
                id
            }
        };
        let r = rpc
            .call("turn/start", json!({ "threadId": thread, "input": [{ "type": "text", "text": prompt }] }))
            .await?;
        let turn = r["turn"]["id"].as_str().context("turn/start returned no turn id")?.to_owned();
        send(
            self.events,
            "status",
            json!({ "state": "started", "engine": "codex", "model": r["turn"]["model"].as_str().map(str::to_owned).or(model),
                    "session_id": thread, "codex": agent }),
        );
        Ok(Setup::Started { thread, turn })
    }

    /// Handle one notification. Returns true when our turn has finished.
    fn notification(
        &self,
        method: &str,
        p: Value,
        turn_id: &str,
        turn: &mut Turn,
        deltas: &mut Deltas,
        seen_shots: &mut std::collections::HashSet<PathBuf>,
    ) -> bool {
        let events = self.events;
        match method {
            "item/agentMessage/delta" => deltas.push("text", p["delta"].as_str().unwrap_or_default()),
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                deltas.push("thinking", p["delta"].as_str().unwrap_or_default())
            }
            "item/started" => {
                let item = &p["item"];
                let id = item["id"].as_str().unwrap_or_default().to_owned();
                if let Some((name, input)) = tool_of(item) {
                    send(events, "tool_call", json!({ "id": id, "name": name, "input": input }));
                }
                turn.items.insert(id, item.clone());
            }
            "item/completed" => {
                let item = &p["item"];
                let id = item["id"].as_str().unwrap_or_default();
                turn.items.remove(id);
                match item["type"].as_str().unwrap_or_default() {
                    "agentMessage" => {
                        let text = item["text"].as_str().unwrap_or_default().to_owned();
                        if !text.trim().is_empty() {
                            send(events, "text", json!({ "text": text }));
                            if item["phase"] == "final_answer" {
                                turn.final_text = Some(text.clone());
                            }
                            turn.last_text = Some(text);
                        }
                    }
                    "reasoning" => {
                        let parts: Vec<&str> = ["summary", "content"]
                            .iter()
                            .flat_map(|k| item[*k].as_array().into_iter().flatten())
                            .filter_map(Value::as_str)
                            .collect();
                        let text = parts.join("\n\n");
                        if !text.trim().is_empty() {
                            send(events, "thinking", json!({ "text": text }));
                        }
                    }
                    "commandExecution" => {
                        let failed = item["status"] != "completed" || item["exitCode"].as_i64().is_some_and(|c| c != 0);
                        let content = item["aggregatedOutput"].as_str().map(str::to_owned).unwrap_or_else(|| {
                            format!("{} (exit {})", item["status"].as_str().unwrap_or("?"), item["exitCode"])
                        });
                        send(events, "tool_result", json!({ "tool_use_id": id, "content": content, "is_error": failed }));
                    }
                    "fileChange" => send(
                        events,
                        "tool_result",
                        json!({ "tool_use_id": id, "content": item["status"], "is_error": item["status"] != "completed" }),
                    ),
                    "mcpToolCall" => {
                        let failed = !item["error"].is_null() || item["status"] == "failed";
                        let content = if failed { item["error"].clone() } else { item["result"]["content"].clone() };
                        send(events, "tool_result", json!({ "tool_use_id": id, "content": content, "is_error": failed }));
                        runner::save_new_shots(self.ctx, self.run, self.shots, seen_shots, events);
                    }
                    "webSearch" => send(
                        events,
                        "tool_result",
                        json!({ "tool_use_id": id, "content": item["action"], "is_error": false }),
                    ),
                    _ => {}
                }
            }
            "thread/tokenUsage/updated" => {
                // `last` is one model request; summing them gives this turn's usage (`total` is the whole thread).
                let last = &p["tokenUsage"]["last"];
                for (i, k) in ["inputTokens", "cachedInputTokens", "outputTokens", "reasoningOutputTokens", "totalTokens"].iter().enumerate() {
                    turn.usage[i] += last[*k].as_u64().unwrap_or(0);
                }
            }
            // Sent after every model request; only record changes.
            "account/rateLimits/updated" if turn.rate_limits.as_ref() != Some(&used(&p["rateLimits"])) => {
                turn.rate_limits = Some(used(&p["rateLimits"]));
                send(events, "rate_limit", json!({ "engine": "codex", "rate_limits": p["rateLimits"] }));
            }
            "error" if p["willRetry"] != true => {
                turn.error = p["error"]["message"].as_str().map(|m| friendly_error(m));
            }
            "mcpServer/startupStatus/updated" if p["status"] != "starting" => send(
                events,
                "status",
                json!({ "state": "mcp", "server": p["name"], "status": p["status"], "error": p["error"] }),
            ),
            "turn/completed" if p["turn"]["id"] == turn_id => {
                let t = &p["turn"];
                turn.status = Some(t["status"].as_str().unwrap_or("completed").to_owned());
                if let Some(e) = t["error"]["message"].as_str() {
                    turn.error = Some(friendly_error(e));
                }
                if turn.final_text.is_none() {
                    turn.final_text = t["items"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .rev()
                        .find(|i| i["type"] == "agentMessage")
                        .and_then(|i| i["text"].as_str())
                        .map(str::to_owned);
                }
                return true;
            }
            _ => tracing::debug!(method, "codex notification"),
        }
        false
    }

    /// Answer a server request (approvals). Decisions run as their own tasks so the event loop keeps draining.
    fn request(&self, rpc: &Rpc, id: Value, method: &str, p: Value, turn: &Turn, scope: &CancellationToken) {
        let ask = |id: Value, tool: String, input: Value, tool_use_id: Option<String>, reason: Option<String>, accept: Value, decline: Value| {
            let (ctx, run, rules, events, scope, rpc) =
                (self.ctx.clone(), self.run.clone(), self.rules.to_vec(), self.events.clone(), scope.clone(), rpc.clone());
            tokio::spawn(async move {
                let allow = decide(&ctx, &run, &tool, &input, tool_use_id.as_deref(), reason.as_deref(), &rules, &events, &scope).await;
                rpc.reply(id, if allow { accept } else { decline });
            });
        };
        let item_id = p["itemId"].as_str().map(str::to_owned);
        let reason = p["reason"].as_str().map(str::to_owned);
        match method {
            "item/commandExecution/requestApproval" => {
                let raw = p["command"]
                    .as_str()
                    .or_else(|| item_id.as_deref().and_then(|i| turn.items.get(i)).and_then(|i| i["command"].as_str()))
                    .unwrap_or_default();
                let input = json!({ "command": unwrap_shell(raw), "cwd": p["cwd"] });
                ask(id, "Bash".into(), input, item_id, reason, json!({ "decision": "accept" }), json!({ "decision": "decline" }));
            }
            "item/fileChange/requestApproval" => {
                let changes = item_id.as_deref().and_then(|i| turn.items.get(i)).map(|i| i["changes"].clone()).unwrap_or(Value::Null);
                let paths: Vec<Value> = changes.as_array().into_iter().flatten().map(|c| c["path"].clone()).collect();
                let input = json!({ "file_path": paths.first(), "file_paths": paths, "changes": changes });
                ask(id, "Edit".into(), input, item_id, reason, json!({ "decision": "accept" }), json!({ "decision": "decline" }));
            }
            "mcpServer/elicitation/request" if p["_meta"]["codex_approval_kind"] == "mcp_tool_call" => {
                let server = p["serverName"].as_str().unwrap_or_default();
                let tool = mcp_tool_name(p["message"].as_str().unwrap_or_default())
                    .or_else(|| {
                        turn.items.values().find(|i| i["type"] == "mcpToolCall" && i["server"] == server).and_then(|i| i["tool"].as_str()).map(str::to_owned)
                    })
                    .unwrap_or_default();
                let input = p["_meta"]["tool_params"].clone();
                ask(id, format!("mcp__{server}__{tool}"), input, None, None, json!({ "action": "accept" }), json!({ "action": "decline" }));
            }
            // A real MCP elicitation (a form for the user): not supported, the server gets a decline.
            "mcpServer/elicitation/request" => rpc.reply(id, json!({ "action": "decline" })),
            // Extra sandbox permissions: grant nothing.
            "item/permissions/requestApproval" => rpc.reply(id, json!({ "permissions": {}, "scope": "turn" })),
            // v1-style approvals (older servers): deny.
            "execCommandApproval" | "applyPatchApproval" => rpc.reply(id, json!({ "decision": "denied" })),
            _ => rpc.reply_error(id, "not supported by Familiar"),
        }
    }
}

/// Owner deny rules and pre-allowed tools first (what Claude Code's own settings do on the Claude engine), then
/// the engine-neutral decision.
#[allow(clippy::too_many_arguments)]
async fn decide(
    ctx: &Ctx,
    run: &Run,
    tool: &str,
    input: &Value,
    tool_use_id: Option<&str>,
    reason: Option<&str>,
    rules: &[Rule],
    events: &Events,
    cancel: &CancellationToken,
) -> bool {
    match permissions::preset(rules, tool, input) {
        Some(true) => true,
        Some(false) => {
            send(events, "approval", json!({ "tool_name": tool, "status": "denied", "decided_by": "rule", "reason": "blocked by an owner rule" }));
            false
        }
        // Codex approvals are accept / decline only: no edited input, no note for the model.
        None => runner::decide_tool(ctx, run, tool, input, tool_use_id, reason, rules, events, cancel, false).await.allow,
    }
}

/// The models this Codex CLI offers (`model/list` on a short-lived app-server) as `(id, label)`, or `None` when it
/// cannot be started or doesn't answer within 30 s.
pub async fn list_models(bin: &str) -> Option<Vec<(String, String)>> {
    let process = spawn(bin, &std::env::temp_dir()).ok()?;
    let rpc = process.rpc.clone();
    let list = tokio::time::timeout(Duration::from_secs(30), async {
        let info = json!({ "clientInfo": { "name": "familiar", "title": "familiar", "version": env!("CARGO_PKG_VERSION") } });
        rpc.call("initialize", info).await.ok()?;
        rpc.notify("initialized");
        rpc.call("model/list", json!({})).await.ok()
    })
    .await
    .ok()
    .flatten();
    drop(rpc);
    let _ = process.finish(Duration::from_secs(3)).await;
    let models = list?["data"]
        .as_array()?
        .iter()
        .filter(|m| m["hidden"] != true)
        .filter_map(|m| {
            let id = m["id"].as_str().or_else(|| m["model"].as_str())?.to_owned();
            let label = m["displayName"].as_str().filter(|s| !s.is_empty()).unwrap_or(&id).to_owned();
            Some((id, label))
        })
        .collect();
    Some(models)
}

async fn default_model(rpc: &Rpc) -> Option<String> {
    let list = rpc.call("model/list", json!({})).await.ok()?;
    list["data"].as_array()?.iter().find(|m| m["isDefault"] == true)?["id"].as_str().map(str::to_owned)
}

/// The run's MCP servers (from `runner::mcp_config`) as Codex `mcp_servers` config.
fn mcp_servers(mcp: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    for (name, s) in mcp["mcpServers"].as_object().into_iter().flatten() {
        // Codex server names must match ^[a-zA-Z0-9_-]+$.
        let key: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        let mut o = Map::new();
        if s["type"] == "stdio" {
            o.insert("command".into(), s["command"].clone());
            o.insert("args".into(), s["args"].clone());
            if s["env"].as_object().is_some_and(|e| !e.is_empty()) {
                o.insert("env".into(), s["env"].clone());
            }
        } else {
            o.insert("url".into(), s["url"].clone());
            if s["headers"].as_object().is_some_and(|h| !h.is_empty()) {
                o.insert("http_headers".into(), s["headers"].clone());
            }
        }
        // npx downloads on first use; ask_user / handoff(wait) / human approvals block for up to 30 minutes.
        o.insert("startup_timeout_sec".into(), json!(120));
        o.insert("tool_timeout_sec".into(), json!(if name == "familiar" { crate::mcp::TOOL_TIMEOUT.as_secs() } else { 1900 }));
        // Familiar's own tools are pre-allowed (they enforce research-only limits themselves); everything else asks us.
        o.insert("default_tools_approval_mode".into(), json!(if name == "familiar" { "approve" } else { "prompt" }));
        out.insert(key, Value::Object(o));
    }
    out
}

/// The tool, as a Familiar tool call (name + input), for items that are tool calls.
fn tool_of(item: &Value) -> Option<(String, Value)> {
    match item["type"].as_str()? {
        "commandExecution" => Some((
            "Bash".into(),
            json!({ "command": unwrap_shell(item["command"].as_str().unwrap_or_default()), "cwd": item["cwd"] }),
        )),
        "fileChange" => Some(("Edit".into(), json!({ "changes": item["changes"] }))),
        "mcpToolCall" => Some((
            format!("mcp__{}__{}", item["server"].as_str().unwrap_or_default(), item["tool"].as_str().unwrap_or_default()),
            item["arguments"].clone(),
        )),
        "webSearch" => Some(("WebSearch".into(), json!({ "query": item["query"], "action": item["action"] }))),
        _ => None,
    }
}

/// The part of a rate-limit update worth recording (resetsAt jitters by a second between updates).
fn used(limits: &Value) -> Value {
    json!([limits["primary"]["usedPercent"], limits["secondary"]["usedPercent"], limits["rateLimitReachedType"]])
}

/// `Allow the Familiar MCP server to run tool "remember"?` → `remember`.
fn mcp_tool_name(message: &str) -> Option<String> {
    let rest = &message[message.find("tool \"")? + 6..];
    Some(rest[..rest.find('"')?].to_owned())
}

fn friendly_error(message: &str) -> String {
    // The backend wraps errors as JSON: surface the human part.
    let inner = serde_json::from_str::<Value>(message).ok().and_then(|v| {
        v["error"]["message"].as_str().or_else(|| v["detail"].as_str()).map(str::to_owned)
    });
    inner.unwrap_or_else(|| message.to_owned())
}

/// Codex reports commands as the full shell invocation (`"C:\…\powershell.exe" -Command '…'`, `bash -lc '…'`).
/// Rules and the always-human check look at what actually runs, so strip one wrapper layer.
pub fn unwrap_shell(command: &str) -> String {
    let s = command.trim();
    let (first, rest) = if let Some(r) = s.strip_prefix('"') {
        match r.find('"') {
            Some(end) => (&r[..end], r[end + 1..].trim_start()),
            None => return s.to_owned(),
        }
    } else {
        match s.split_once(char::is_whitespace) {
            Some((f, r)) => (f, r.trim_start()),
            None => return s.to_owned(),
        }
    };
    let name = first.rsplit(['/', '\\']).next().unwrap_or(first).to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    let flags: &[&str] = match name {
        "powershell" | "pwsh" => &["-command", "-c"],
        "bash" | "sh" | "zsh" => &["-c", "-lc", "-ic"],
        "cmd" => &["/c", "/s"],
        _ => return s.to_owned(),
    };
    // Skip options up to and including the command flag.
    let mut rest = rest;
    loop {
        let (word, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        if word.is_empty() || !(word.starts_with('-') || word.starts_with('/')) {
            break;
        }
        rest = tail.trim_start();
        if flags.contains(&word.to_ascii_lowercase().as_str()) {
            break;
        }
    }
    let inner = rest.trim();
    if inner.len() >= 2 && inner.starts_with('\'') && inner.ends_with('\'') {
        let body = &inner[1..inner.len() - 1];
        return if name == "powershell" || name == "pwsh" { body.replace("''", "'") } else { body.replace("'\\''", "'") };
    }
    if inner.len() >= 2 && inner.starts_with('"') && inner.ends_with('"') {
        return inner[1..inner.len() - 1].replace("\\\"", "\"");
    }
    inner.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_shells() {
        assert_eq!(
            unwrap_shell(r#""C:\\WINDOWS\\System32\\WindowsPowerShell\\v1.0\\powershell.exe" -Command 'Set-Content -Path probe.txt -Value banana'"#),
            "Set-Content -Path probe.txt -Value banana"
        );
        assert_eq!(
            unwrap_shell(r#""C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -Command 'Write-Output ''hi'''"#),
            "Write-Output 'hi'"
        );
        assert_eq!(unwrap_shell("bash -lc 'rm -rf build'"), "rm -rf build");
        assert_eq!(unwrap_shell("/bin/zsh -lc \"git push --force\""), "git push --force");
        assert_eq!(unwrap_shell("cmd.exe /c del /s x"), "del /s x");
        assert_eq!(unwrap_shell("git status"), "git status");
        assert!(permissions::always_human("Bash", &json!({ "command": unwrap_shell("bash -lc 'rm -rf /tmp/x'") })).is_some());
    }

    #[test]
    fn helpers() {
        assert_eq!(mcp_tool_name("Allow the Familiar MCP server to run tool \"remember\"?").as_deref(), Some("remember"));
        assert!(wants_default_model("sonnet") && wants_default_model("claude-opus-4") && !wants_default_model("gpt-6-luna"));
        assert_eq!(
            friendly_error(r#"{"type":"error","status":400,"error":{"type":"invalid_request_error","message":"nope"}}"#),
            "nope"
        );
        let servers = mcp_servers(&json!({ "mcpServers": {
            "familiar": { "type": "http", "url": "http://127.0.0.1:1/mcp", "headers": { "Authorization": "Bearer t" } },
            "my server": { "type": "stdio", "command": "npx.cmd", "args": ["-y", "x"], "env": {} },
        }}));
        assert_eq!(servers["familiar"]["http_headers"]["Authorization"], "Bearer t");
        assert_eq!(servers["familiar"]["default_tools_approval_mode"], "approve");
        assert_eq!(servers["my_server"]["default_tools_approval_mode"], "prompt");
        assert!(servers["my_server"].get("env").is_none());
    }
}
