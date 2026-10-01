//! `fake-claude`: a scripted stand-in for `claude -p --input-format stream-json --output-format stream-json`.
//!
//! It speaks the same stdio protocol the Familiar daemon expects (see `familiar-core/src/runner.rs::drive` and
//! `claude.rs`), but instead of a model it plays a JSON scenario. Every invocation is recorded (argv, cwd, every
//! stdin line, permission decisions, MCP results) as JSON lines so tests can assert on flags and protocol traffic.
//!
//! Scenario: `$FAKE_CLAUDE_SCENARIO`, else the first `fake-claude.json` found in the working directory or one of
//! its ancestors (the daemon runs claude in `<bots_dir>/<slug>`, so a test drops it in its own `bots_dir`; that
//! keeps parallel tests apart without touching the process environment). Log: `$FAKE_CLAUDE_LOG`, else
//! `fake-claude.log.jsonl` next to the scenario. See the crate README for the step format.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use familiar_testkit::{Log, emit, step_kind};
use serde_json::{Value, json};

struct Fake {
    n: usize,
    log: Log,
    stdin: Receiver<Value>,
    session_id: String,
    model: String,
    mcp_config: Option<PathBuf>,
    next_id: usize,
    turns: usize,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("2.1.282 (fake-claude)");
        return;
    }
    let default = json!({ "steps": [{ "init": {} }, { "result": "ok" }] });
    let inv = familiar_testkit::start("fake-claude", "FAKE_CLAUDE", &["MCP_TOOL_TIMEOUT", "MCP_TIMEOUT"], default);

    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let session_id = flag("--session-id").or_else(|| flag("--resume")).unwrap_or_else(|| "no-session".into());
    let model = flag("--model").unwrap_or_else(|| "fake".into());
    let mcp_config = flag("--mcp-config").map(PathBuf::from);
    let mut fake = Fake { n: inv.n, log: inv.log, stdin: inv.stdin, session_id, model, mcp_config, next_id: 0, turns: 0 };

    // Like claude: nothing happens until the user message arrives.
    let first = fake.wait_for(Duration::from_secs(30), |m| m["type"] == "user");
    if first.is_none() {
        fake.exit(1, "no user message on stdin");
    }
    for step in &inv.steps {
        fake.step(step);
    }
    fake.exit(0, "steps finished without a result");
}

impl Fake {
    fn emit(&self, v: Value) {
        emit(&v);
    }

    fn id(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}_fake{}_{}", self.n, self.next_id)
    }

    fn exit(&self, code: i32, reason: &str) -> ! {
        self.log.write(json!({ "event": "exit", "code": code, "reason": reason }));
        std::process::exit(code)
    }

    /// Wait for a stdin message matching `want`, answering an `interrupt` the way claude does (ack + error result).
    fn wait_for(&mut self, timeout: Duration, want: impl Fn(&Value) -> bool) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.stdin.recv_timeout(left) {
                Ok(m) if m["type"] == "control_request" && m["request"]["subtype"] == "interrupt" => self.interrupted(&m),
                Ok(m) if want(&m) => return Some(m),
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => return None,
                // stdin closed: nothing more can arrive.
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    fn interrupted(&mut self, m: &Value) -> ! {
        let request_id = m["request_id"].clone();
        self.emit(json!({ "type": "control_response", "response": { "subtype": "success", "request_id": request_id } }));
        self.emit(self.result_msg("error_during_execution", true, None, &["interrupted by user"]));
        self.exit(0, "interrupted")
    }

    fn result_msg(&self, subtype: &str, is_error: bool, text: Option<&str>, errors: &[&str]) -> Value {
        let mut r = json!({
            "type": "result", "subtype": subtype, "is_error": is_error, "duration_ms": 12, "duration_api_ms": 10,
            "num_turns": self.turns.max(1), "session_id": self.session_id, "total_cost_usd": 0.0123,
            "usage": { "input_tokens": 10, "output_tokens": 5 }, "permission_denials": [],
        });
        if let Some(t) = text {
            r["result"] = json!(t);
        }
        if !errors.is_empty() {
            r["errors"] = json!(errors);
        }
        r
    }

    fn assistant(&mut self, content: Value) {
        self.turns += 1;
        let id = self.id("msg");
        self.emit(json!({
            "type": "assistant", "session_id": self.session_id,
            "message": { "id": id, "type": "message", "role": "assistant", "model": self.model, "content": content },
        }));
    }

    fn tool_result(&self, tool_use_id: &str, content: &str, is_error: bool) {
        self.emit(json!({
            "type": "user", "session_id": self.session_id,
            "message": { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": tool_use_id, "content": content, "is_error": is_error }
            ] },
        }));
    }

    fn step(&mut self, step: &Value) {
        let Some((kind, arg)) = step_kind(step) else { return };
        match kind {
            "init" => self.emit(json!({
                "type": "system", "subtype": "init", "session_id": self.session_id, "model": self.model,
                "cwd": std::env::current_dir().unwrap_or_default().display().to_string(),
                "tools": [], "mcp_servers": [{ "name": "familiar", "status": "connected" }],
                "permissionMode": "default", "apiKeySource": "none",
            })),
            "delta" => self.emit(json!({
                "type": "stream_event", "session_id": self.session_id,
                "event": { "type": "content_block_delta", "index": 0,
                           "delta": { "type": "text_delta", "text": arg.as_str().unwrap_or_default() } },
            })),
            "text" => self.assistant(json!([{ "type": "text", "text": arg }])),
            "thinking" => self.assistant(json!([{ "type": "thinking", "thinking": arg, "signature": "fake" }])),
            "tool" => self.tool(arg),
            "mcp" => self.mcp(arg),
            "sleep_ms" => {
                // Interruptible: an interrupt arriving while "thinking" ends the run like claude would.
                let until = Instant::now() + Duration::from_millis(arg.as_u64().unwrap_or(0));
                let _ = self.wait_for(until - Instant::now(), |_| false);
                std::thread::sleep(until.saturating_duration_since(Instant::now()));
            }
            "stderr" => {
                let mut e = std::io::stderr().lock();
                let _ = writeln!(e, "{}", arg.as_str().unwrap_or_default());
                let _ = e.flush();
            }
            "rate_limit" => self.emit(json!({ "type": "rate_limit_event", "rate_limit_info": arg, "session_id": self.session_id })),
            "raw" => self.emit(arg.clone()),
            "result" => {
                self.emit(self.result_msg("success", false, Some(arg.as_str().unwrap_or_default()), &[]));
                self.exit(0, "result");
            }
            "error_result" => {
                let msg = arg.as_str().unwrap_or("error");
                self.emit(self.result_msg("error_during_execution", true, None, &[msg]));
                self.exit(1, "error result");
            }
            "exit" => self.exit(arg.as_i64().unwrap_or(1) as i32, "scripted exit"),
            other => {
                self.log.write(json!({ "event": "unknown_step", "step": other }));
            }
        }
    }

    /// assistant tool_use → can_use_tool control_request → wait for the daemon's control_response → tool_result.
    fn tool(&mut self, arg: &Value) {
        let name = arg["name"].as_str().unwrap_or("Write").to_owned();
        let input = arg.get("input").cloned().unwrap_or_else(|| json!({}));
        let tool_use_id = arg["id"].as_str().map(str::to_owned).unwrap_or_else(|| self.id("toolu"));
        self.assistant(json!([{ "type": "tool_use", "id": tool_use_id, "name": name, "input": input }]));
        let request_id = self.id("req");
        self.emit(json!({
            "type": "control_request", "request_id": request_id,
            "request": { "subtype": "can_use_tool", "tool_name": name, "input": input, "tool_use_id": tool_use_id,
                         "description": arg["description"].as_str().unwrap_or("fake tool call"),
                         "permission_suggestions": [] },
        }));
        let timeout = Duration::from_millis(arg["timeout_ms"].as_u64().unwrap_or(60_000));
        let rid = request_id.clone();
        let Some(reply) = self.wait_for(timeout, move |m| m["type"] == "control_response" && m["response"]["request_id"] == rid.as_str())
        else {
            self.exit(2, "no control_response for can_use_tool");
        };
        let decision = &reply["response"]["response"];
        let allowed = decision["behavior"] == "allow";
        self.log.write(json!({ "event": "permission", "tool": name, "request_id": request_id, "decision": decision }));
        if allowed {
            self.tool_result(&tool_use_id, arg["ok"].as_str().unwrap_or("ok"), false);
        } else {
            let msg = decision["message"].as_str().unwrap_or("denied").to_owned();
            self.tool_result(&tool_use_id, &msg, true);
        }
    }

    /// Call a tool on one of the run's HTTP MCP servers (normally `familiar`), as Claude would, and report it.
    fn mcp(&mut self, arg: &Value) {
        let server = arg["server"].as_str().unwrap_or("familiar").to_owned();
        let tool = arg["tool"].as_str().unwrap_or_default().to_owned();
        let arguments = arg.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let tool_use_id = self.id("toolu");
        self.assistant(json!([{
            "type": "tool_use", "id": tool_use_id, "name": format!("mcp__{server}__{tool}"), "input": arguments
        }]));
        let outcome = self.mcp_config.clone().ok_or_else(|| "no --mcp-config".to_owned()).and_then(|path| {
            mcp_call(&path, &server, &tool, &arguments)
        });
        let (text, is_error) = match &outcome {
            Ok(r) => (
                r["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n"),
                r["isError"] == true,
            ),
            Err(e) => (format!("MCP error: {e}"), true),
        };
        self.log.write(json!({ "event": "mcp", "tool": tool, "text": text, "is_error": is_error }));
        self.tool_result(&tool_use_id, &text, is_error);
    }
}

/// Minimal streamable-HTTP MCP client: initialize → notifications/initialized → tools/call.
fn mcp_call(config: &Path, server: &str, tool: &str, arguments: &Value) -> Result<Value, String> {
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(config).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let s = &cfg["mcpServers"][server];
    let url = s["url"].as_str().ok_or_else(|| format!("server `{server}` has no url"))?.to_owned();
    let headers: Vec<(String, String)> = s["headers"]
        .as_object()
        .map(|h| h.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned()))).collect())
        .unwrap_or_default();
    // No timeout: ask_user blocks until the owner answers.
    let client = reqwest::blocking::Client::builder().timeout(None).build().map_err(|e| e.to_string())?;
    let post = |body: Value, session: Option<&str>| {
        let mut rb = client
            .post(&url)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .body(body.to_string());
        for (k, v) in &headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if let Some(sid) = session {
            rb = rb.header("Mcp-Session-Id", sid);
        }
        rb.send().map_err(|e| e.to_string())
    };
    let init = post(
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "fake-claude", "version": "0" } } }),
        None,
    )?;
    let session = init.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()).map(str::to_owned);
    read_rpc(init, 1)?;
    let _ = post(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }), session.as_deref())?;
    let resp = post(
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": tool, "arguments": arguments } }),
        session.as_deref(),
    )?;
    read_rpc(resp, 2)
}

/// The JSON-RPC response with `id`, from a JSON body or an SSE stream.
fn read_rpc(resp: reqwest::blocking::Response, id: i64) -> Result<Value, String> {
    let status = resp.status();
    let sse = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|c| c.contains("event-stream"));
    let pick = |v: Value| -> Option<Result<Value, String>> {
        (v["id"] == id).then(|| match v.get("error") {
            Some(e) => Err(e.to_string()),
            None => Ok(v["result"].clone()),
        })
    };
    if !sse {
        let body = resp.text().map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("HTTP {status}: {body}"));
        }
        let v: Value = serde_json::from_str(&body).map_err(|e| format!("{e}: {body}"))?;
        return pick(v).unwrap_or_else(|| Err(format!("no response with id {id}")));
    }
    let mut data = String::new();
    for line in BufReader::new(resp).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if let Some(d) = line.strip_prefix("data:") {
            data.push_str(d.trim_start());
        } else if line.is_empty()
            && !data.is_empty()
            && let Ok(v) = serde_json::from_str::<Value>(&std::mem::take(&mut data))
            && let Some(r) = pick(v)
        {
            return r;
        }
    }
    Err(format!("stream ended without a response to {id}"))
}
