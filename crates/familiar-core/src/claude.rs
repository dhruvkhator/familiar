//! Spawning `claude -p` and speaking its stream-json protocol (verified against Claude Code 2.1.282).

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub enum Session {
    New(Uuid),
    Resume(Uuid),
}

pub struct Spec<'a> {
    pub bin: &'a str,
    pub cwd: &'a Path,
    pub model: &'a str,
    pub session: Session,
    /// Claude Code `permissions` settings object (allow / deny / ask lists).
    pub permissions: Value,
    /// Built-in tools the bot gets. Always explicit: `--restricted` drops anything not named.
    pub tools: &'a [&'a str],
    /// Persona + house rules + memories, appended to the system prompt and re-read on every resume.
    pub system_prompt: &'a Path,
    /// `--mcp-config` file. It carries the run token and connector secrets, so it lives outside the workspace.
    pub mcp_config: Option<&'a Path>,
}

pub struct Process {
    child: Child,
    /// Lines written to the child's stdin. Dropping every sender closes stdin.
    pub stdin: mpsc::UnboundedSender<Value>,
    pub stdout: Lines<BufReader<ChildStdout>>,
    stderr: JoinHandle<String>,
}

pub fn spawn(spec: &Spec) -> Result<Process> {
    let mut cmd = Command::new(spec.bin);
    cmd.args(["-p", "--verbose", "--output-format", "stream-json", "--input-format", "stream-json"])
        .arg("--include-partial-messages")
        .args(["--model", spec.model])
        // Restricted: ignores settings files and skill `allowed-tools` (so a bot cannot grant itself
        // permissions by writing them), confines file tools to the workspace, and lets only the permission
        // handler approve writes to tool configuration.
        .args(["--restricted", "--strict-mcp-config"])
        .arg("--tools")
        .arg(spec.tools.join(","))
        .args(["--permission-prompt-tool", "stdio"])
        .arg("--settings")
        .arg(json!({ "permissions": spec.permissions }).to_string());
    match spec.session {
        Session::New(id) => cmd.arg("--session-id").arg(id.to_string()),
        Session::Resume(id) => cmd.arg("--resume").arg(id.to_string()),
    };
    cmd.args(["--system-prompt-snapshot", "off", "--append-system-prompt-file"]).arg(spec.system_prompt);
    if let Some(mcp) = &spec.mcp_config {
        cmd.arg("--mcp-config").arg(mcp);
    }
    // ask_user and handoff(wait) block for up to 30 / 15 minutes, propose_draft for up to a day. One value covers
    // every MCP server, so a hung connector call also waits that long (cancelling the run still stops it).
    cmd.env("MCP_TOOL_TIMEOUT", crate::mcp::TOOL_TIMEOUT.as_millis().to_string());
    subscription_only(&mut cmd);
    // First use of an npx/uvx connector downloads it; the 30 s default startup wait is too short.
    cmd.env("MCP_TIMEOUT", "120000");
    cmd.current_dir(spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash when hosted by the tray app

    let mut child = cmd.spawn().with_context(|| format!("spawning {}", spec.bin))?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let stdout = BufReader::new(child.stdout.take().context("no stdout")?).lines();
    let mut stderr_pipe = child.stderr.take().context("no stderr")?;

    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let line = format!("{msg}\n");
            if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
        // stdin dropped here → EOF for the child
    });
    let stderr = tokio::spawn(async move {
        let mut buf = String::new();
        let _ = (&mut stderr_pipe).take(64 * 1024).read_to_string(&mut buf).await;
        // Keep draining so a chatty child never blocks on a full pipe.
        let _ = tokio::io::copy(&mut stderr_pipe, &mut tokio::io::sink()).await;
        buf
    });

    Ok(Process { child, stdin: tx, stdout, stderr })
}

impl Process {
    pub fn send_user(&self, prompt: &str) {
        let _ = self.stdin.send(json!({
            "type": "user",
            "message": { "role": "user", "content": prompt },
        }));
    }

    pub fn interrupt(&self) {
        let _ = self.stdin.send(json!({
            "type": "control_request",
            "request_id": Uuid::new_v4().to_string(),
            "request": { "subtype": "interrupt" },
        }));
    }

    /// Kill the whole tree (MCP servers started via npx are grandchildren).
    pub async fn kill(&mut self) {
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

    /// Wait for exit (killing after `grace`), returning captured stderr.
    pub async fn finish(mut self, grace: std::time::Duration) -> String {
        // Replace our sender with a dead one so stdin closes once in-flight permission tasks finish.
        self.stdin = mpsc::unbounded_channel().0;
        if tokio::time::timeout(grace, self.child.wait()).await.is_err() {
            self.kill().await;
        }
        self.stderr.await.unwrap_or_default()
    }
}

/// Reply to a `can_use_tool` control request.
pub fn permission_reply(request_id: &str, allow: bool, input: &Value, message: &str) -> Value {
    let response = if allow {
        json!({ "behavior": "allow", "updatedInput": input })
    } else {
        json!({ "behavior": "deny", "message": message })
    };
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request_id, "response": response },
    })
}

/// Familiar runs on the owner's Claude subscription. An API key or auth token in the environment would silently
/// switch the CLI to pay-per-use billing, so they are removed unless FAMILIAR_ALLOW_API_KEY=1 opts in.
pub fn subscription_only(cmd: &mut Command) {
    if std::env::var("FAMILIAR_ALLOW_API_KEY").as_deref() != Ok("1") {
        cmd.env_remove("ANTHROPIC_API_KEY").env_remove("ANTHROPIC_AUTH_TOKEN");
    }
}
