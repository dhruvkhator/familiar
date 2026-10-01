//! Shared plumbing of the scripted fakes (`fake-claude`, `fake-codex`): scenario discovery, the JSONL invocation log
//! and a stdin reader thread. See the README for the scenario format.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// Append-only JSONL log; every entry carries the invocation number `n` and the process id.
#[derive(Clone)]
pub struct Log {
    file: Option<Arc<Mutex<File>>>,
    n: usize,
}

impl Log {
    pub fn write(&self, mut entry: Value) {
        if let Some(f) = &self.file {
            entry["n"] = json!(self.n);
            entry["pid"] = json!(std::process::id());
            let mut f = f.lock().unwrap();
            let _ = writeln!(f, "{entry}");
            let _ = f.flush();
        }
    }
}

/// One invocation of a fake: which scenario steps to play, its log and its stdin (one JSON value per line).
pub struct Invocation {
    pub n: usize,
    pub steps: Vec<Value>,
    pub log: Log,
    pub stdin: Receiver<Value>,
}

/// Find the scenario (`$<PREFIX>_SCENARIO`, else `<name>.json` in the cwd or an ancestor), log the start of this
/// invocation (argv, cwd, the env vars named in `env`) and start reading stdin.
///
/// Scenario shapes: `{"invocations": [[steps], [steps], ...]}` (the n-th spawn plays entry n, the last one repeats),
/// `{"steps": [steps]}` or a bare `[steps]` (every spawn). Missing scenario = `default`.
pub fn start(name: &str, env_prefix: &str, env: &[&str], default: Value) -> Invocation {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    let scenario_path = std::env::var_os(format!("{env_prefix}_SCENARIO"))
        .map(PathBuf::from)
        .or_else(|| find_up(&cwd, &format!("{name}.json")));
    let log_path = std::env::var_os(format!("{env_prefix}_LOG"))
        .map(PathBuf::from)
        .or_else(|| scenario_path.as_ref().map(|p| p.with_file_name(format!("{name}.log.jsonl"))));
    let scenario: Value = scenario_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(default);
    // Invocation number = how many starts the log already has (spawns of one scenario are sequential).
    let n = log_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.lines().filter(|l| l.contains("\"event\":\"start\"")).count())
        .unwrap_or(0);
    let file = log_path.and_then(|p| OpenOptions::new().create(true).append(true).open(p).ok()).map(|f| Arc::new(Mutex::new(f)));
    let log = Log { file, n };
    let env: serde_json::Map<String, Value> =
        env.iter().filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), json!(v)))).collect();
    log.write(json!({ "event": "start", "argv": args, "cwd": cwd.display().to_string(), "env": env }));

    let steps = match (&scenario["invocations"], &scenario["steps"]) {
        (Value::Array(inv), _) if !inv.is_empty() => inv[n.min(inv.len() - 1)].clone(),
        (_, Value::Array(_)) => scenario["steps"].clone(),
        _ => scenario,
    };
    let steps = steps.as_array().cloned().unwrap_or_default();

    let (tx, stdin) = channel::<Value>();
    let reader_log = log.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(std::io::stdin()).lines() {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            reader_log.write(json!({ "event": "stdin", "line": v }));
            if tx.send(v).is_err() {
                break;
            }
        }
    });
    Invocation { n, steps, log, stdin }
}

pub fn find_up(start: &Path, name: &str) -> Option<PathBuf> {
    start.ancestors().map(|d| d.join(name)).find(|p| p.is_file())
}

/// Write one JSON line to stdout and flush.
pub fn emit(v: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

/// The one key of a step object and its argument (`{"text": "hi"}` → ("text", "hi")).
pub fn step_kind(step: &Value) -> Option<(&str, &Value)> {
    step.as_object().and_then(|o| o.iter().next()).map(|(k, v)| (k.as_str(), v))
}
