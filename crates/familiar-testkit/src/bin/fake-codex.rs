//! `fake-codex`: a scripted stand-in for `codex app-server` (JSON-RPC over stdio, app-server protocol v2), covering
//! the subset `familiar-core/src/codex.rs` uses: initialize, config/read, model/list, skills/extraRoots/set,
//! thread/start|resume, turn/start, turn/interrupt, item notifications, exec approval requests and turn completion.
//!
//! Scenario: `$FAKE_CODEX_SCENARIO`, else `fake-codex.json` in the cwd or an ancestor; log: `$FAKE_CODEX_LOG`, else
//! `fake-codex.log.jsonl` next to it. Steps run after `turn/start` (see the crate README).

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use familiar_testkit::{Log, emit, step_kind};
use serde_json::{Value, json};

struct Fake {
    n: usize,
    log: Log,
    stdin: Receiver<Value>,
    thread: String,
    turn: String,
    next_id: i64,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("codex-cli 0.159.3 (fake-codex)");
        return;
    }
    let default = json!({ "steps": [{ "text": "ok" }, { "complete": {} }] });
    let inv = familiar_testkit::start("fake-codex", "FAKE_CODEX", &[], default);
    let mut fake = Fake { n: inv.n, log: inv.log, stdin: inv.stdin, thread: String::new(), turn: String::new(), next_id: 1000 };
    let no_rollout = inv.steps.iter().any(|s| step_kind(s).is_some_and(|(k, _)| k == "no_rollout"));

    // Setup requests until turn/start, then the scripted turn, then serve until stdin closes.
    while let Some(msg) = fake.next(None) {
        let (Some(method), Some(id)) = (msg["method"].as_str(), msg.get("id").cloned()) else { continue };
        let p = &msg["params"];
        match method {
            "initialize" => reply(&id, json!({ "userAgent": "fake-codex/0.159.3" })),
            // An MCP server from the owner's own Codex config: the daemon must switch it off for the bot.
            "config/read" => reply(&id, json!({ "config": { "mcp_servers": { "owner_own": { "command": "x" } } } })),
            "model/list" => reply(&id, json!({ "data": [{ "id": "gpt-fake", "isDefault": true }] })),
            "skills/extraRoots/set" => reply(&id, json!({})),
            "thread/start" => {
                fake.thread = format!("thr_fake_{}", fake.n);
                reply(&id, json!({ "thread": { "id": fake.thread } }));
            }
            "thread/resume" if no_rollout => {
                let t = p["threadId"].as_str().unwrap_or_default();
                reply_error(&id, &format!("no rollout found for thread id {t}"));
            }
            "thread/resume" => {
                fake.thread = p["threadId"].as_str().unwrap_or_default().to_owned();
                reply(&id, json!({ "thread": { "id": fake.thread } }));
            }
            "turn/start" => {
                fake.turn = format!("turn_fake_{}", fake.n);
                reply(&id, json!({ "turn": { "id": fake.turn, "model": "gpt-fake" } }));
                for step in &inv.steps {
                    fake.step(step);
                }
            }
            "turn/interrupt" => fake.interrupted(&id),
            other => reply_error(&id, &format!("fake-codex does not implement {other}")),
        }
    }
    fake.exit(0, "stdin closed");
}

fn reply(id: &Value, result: Value) {
    emit(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
}

fn reply_error(id: &Value, message: &str) {
    emit(&json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32600, "message": message } }));
}

fn notify(method: &str, params: Value) {
    emit(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
}

impl Fake {
    fn exit(&self, code: i32, reason: &str) -> ! {
        self.log.write(json!({ "event": "exit", "code": code, "reason": reason }));
        std::process::exit(code)
    }

    /// Next stdin message (None: EOF, or `timeout` elapsed).
    fn next(&mut self, timeout: Option<Duration>) -> Option<Value> {
        match timeout {
            None => self.stdin.recv().ok(),
            Some(t) => self.stdin.recv_timeout(t).ok(),
        }
    }

    /// Wait for a message matching `want` during the turn, answering `turn/interrupt` like codex does.
    fn wait_for(&mut self, timeout: Duration, want: impl Fn(&Value) -> bool) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            match self.stdin.recv_timeout(left) {
                Ok(m) if m["method"] == "turn/interrupt" => self.interrupted(&m["id"].clone()),
                Ok(m) if want(&m) => return Some(m),
                Ok(m) => {
                    if let (Some(method), Some(id)) = (m["method"].as_str(), m.get("id")) {
                        reply_error(id, &format!("fake-codex: unexpected {method} during a turn"));
                    }
                }
                Err(RecvTimeoutError::Timeout) => return None,
                Err(RecvTimeoutError::Disconnected) => self.exit(0, "stdin closed during the turn"),
            }
        }
    }

    fn interrupted(&mut self, id: &Value) -> ! {
        reply(id, json!({}));
        notify("turn/completed", json!({ "threadId": self.thread, "turn": { "id": self.turn, "status": "interrupted", "items": [] } }));
        // Like the real app-server: stay up until the client closes stdin.
        while self.next(None).is_some() {}
        self.exit(0, "interrupted")
    }

    fn item_id(&mut self) -> String {
        self.next_id += 1;
        format!("item_fake{}_{}", self.n, self.next_id)
    }

    fn step(&mut self, step: &Value) {
        let Some((kind, arg)) = step_kind(step) else { return };
        let (thread, turn) = (self.thread.clone(), self.turn.clone());
        match kind {
            "text" => {
                let id = self.item_id();
                let text = arg.as_str().unwrap_or_default();
                notify("item/agentMessage/delta", json!({ "threadId": thread, "turnId": turn, "itemId": id, "delta": text }));
                notify("item/completed", json!({ "threadId": thread, "turnId": turn,
                    "item": { "type": "agentMessage", "id": id, "text": text, "phase": "final_answer" } }));
            }
            "reasoning" => {
                let id = self.item_id();
                notify("item/completed", json!({ "threadId": thread, "turnId": turn,
                    "item": { "type": "reasoning", "id": id, "summary": [arg], "content": [] } }));
            }
            "exec" => self.exec(arg),
            "sleep_ms" => {
                let until = Instant::now() + Duration::from_millis(arg.as_u64().unwrap_or(0));
                let _ = self.wait_for(until - Instant::now(), |_| false);
                std::thread::sleep(until.saturating_duration_since(Instant::now()));
            }
            "complete" => {
                notify("thread/tokenUsage/updated", json!({ "threadId": thread, "turnId": turn, "tokenUsage": {
                    "last": { "inputTokens": 100, "cachedInputTokens": 10, "outputTokens": 20, "reasoningOutputTokens": 5, "totalTokens": 120 } } }));
                notify("turn/completed", json!({ "threadId": thread, "turn": { "id": turn, "status": "completed", "items": [] } }));
            }
            "fail" => notify("turn/completed", json!({ "threadId": thread,
                "turn": { "id": turn, "status": "failed", "items": [], "error": { "message": arg.as_str().unwrap_or("failed") } } })),
            "no_rollout" => {}
            "raw" => emit(arg),
            "exit" => self.exit(arg.as_i64().unwrap_or(1) as i32, "scripted exit"),
            other => self.log.write(json!({ "event": "unknown_step", "step": other })),
        }
    }

    /// commandExecution item + `item/commandExecution/requestApproval` server request → wait for accept/decline.
    fn exec(&mut self, arg: &Value) {
        let (thread, turn) = (self.thread.clone(), self.turn.clone());
        let item = self.item_id();
        let command = arg["command"].as_str().unwrap_or("bash -lc 'touch x'");
        let cwd = std::env::current_dir().unwrap_or_default().display().to_string();
        notify("item/started", json!({ "threadId": thread, "turnId": turn,
            "item": { "type": "commandExecution", "id": item, "command": command, "cwd": cwd, "status": "inProgress" } }));
        self.next_id += 1;
        let req = self.next_id;
        emit(&json!({ "jsonrpc": "2.0", "id": req, "method": "item/commandExecution/requestApproval",
            "params": { "threadId": thread, "turnId": turn, "itemId": item, "command": command, "cwd": cwd,
                        "reason": arg["reason"].as_str() } }));
        let Some(answer) = self.wait_for(Duration::from_secs(60), move |m| m["id"] == req && m.get("method").is_none()) else {
            self.exit(2, "no answer to requestApproval");
        };
        let decision = answer["result"]["decision"].as_str().unwrap_or_default().to_owned();
        self.log.write(json!({ "event": "approval", "kind": "commandExecution", "decision": decision }));
        let accepted = decision == "accept";
        notify("item/completed", json!({ "threadId": thread, "turnId": turn, "item": {
            "type": "commandExecution", "id": item, "command": command, "cwd": cwd,
            "status": if accepted { "completed" } else { "declined" },
            "exitCode": if accepted { json!(0) } else { Value::Null },
            "aggregatedOutput": if accepted { arg["output"].as_str().unwrap_or("ok") } else { "" } } }));
    }
}
