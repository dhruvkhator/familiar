//! Which models this computer can run, for the model pickers: the Claude catalog checked against the owner's plan,
//! and the models the Codex CLI lists. Published in the device heartbeat as `info.models`:
//!
//! ```json
//! { "claude": { "plan": "max", "models": [{ "id": "opus", "label": "Opus 5.5", "alias": true, "available": true }] },
//!   "codex":  { "models": [{ "id": "gpt-5-codex", "label": "GPT-5-Codex" }] } }
//! ```
//!
//! Aliases (`sonnet`, `opus`, ...) always run the newest version and are always available. Each exact version is
//! verified once with a tiny `claude -p` call ("Reply with: ok") and the answer is cached in `<home>/models.json` for
//! seven days, so the check doesn't keep spending the owner's usage; `available` is `null` until verified. Of
//! `claude auth status` only `subscriptionType` is read: never the email or organisation.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::Config;

/// The aliases: `(id, label)`; the label names the version the alias runs today.
pub const CLAUDE_ALIASES: [(&str, &str); 4] =
    [("sonnet", "Sonnet 5.5"), ("opus", "Opus 5.5"), ("haiku", "Haiku 4.5"), ("fable", "Fable 5.1")];

/// Exact versions: `(id, label)`.
pub const CLAUDE_VERSIONS: [(&str, &str); 5] = [
    ("claude-opus-5-5", "Opus 5.5"),
    ("claude-sonnet-5-5", "Sonnet 5.5"),
    ("claude-sonnet-5", "Sonnet 5"),
    ("claude-fable-5-1", "Fable 5.1"),
    ("claude-haiku-4-5-20251001", "Haiku 4.5"),
];

const CACHE_FOR: Duration = Duration::from_secs(7 * 24 * 3600);
const REFRESH_EVERY: Duration = Duration::from_secs(6 * 3600);
const CHECK_TIMEOUT: Duration = Duration::from_secs(60);

/// What the heartbeat publishes (`Value::Null` until the first look).
pub type Shared = Arc<Mutex<Value>>;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    plan: Option<String>,
    /// Exact model id → when it was checked and whether it answered.
    checked: BTreeMap<String, Checked>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Checked {
    available: bool,
    at: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn cache_path() -> PathBuf {
    Config::home_dir().join("models.json")
}

fn load_cache() -> Cache {
    std::fs::read(cache_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_cache(c: &Cache) {
    if let Ok(json) = serde_json::to_vec_pretty(c) {
        let _ = std::fs::write(cache_path(), json);
    }
}

/// The `info.models` value for a plan and what is known about each exact version.
fn claude_json(plan: Option<&str>, checked: &BTreeMap<String, Checked>) -> Value {
    let mut models: Vec<Value> = CLAUDE_ALIASES
        .iter()
        .map(|(id, label)| json!({ "id": id, "label": label, "alias": true, "available": true }))
        .collect();
    models.extend(CLAUDE_VERSIONS.iter().map(|(id, label)| {
        json!({ "id": id, "label": label, "alias": false, "available": checked.get(*id).map(|c| c.available) })
    }));
    json!({ "plan": plan, "models": models })
}

fn hidden(_cmd: &mut Command) {
    #[cfg(windows)]
    _cmd.creation_flags(0x0800_0000);
}

/// `subscriptionType` from `claude auth status`, or `None` when signed out / not installed. Nothing else is kept.
async fn claude_plan(bin: &str) -> Option<String> {
    let mut cmd = Command::new(bin);
    cmd.args(["auth", "status"]).stdin(Stdio::null()).kill_on_drop(true);
    crate::claude::subscription_only(&mut cmd);
    hidden(&mut cmd);
    let out = tokio::time::timeout(Duration::from_secs(20), cmd.output()).await.ok()?.ok()?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    if v["loggedIn"] != true {
        return None;
    }
    Some(v["subscriptionType"].as_str().unwrap_or("unknown").to_owned())
}

/// One minimal call on `model`: `Some(true)` it answered, `Some(false)` the CLI refused the model, `None` unknown
/// (timeout, rate or usage limit, network): not cached, tried again on the next refresh.
async fn verify(bin: &str, model: &str) -> Option<bool> {
    let mut cmd = Command::new(bin);
    cmd.args(["-p", "--model", model, "--tools", "", "--strict-mcp-config", "--no-session-persistence"])
        .args(["--permission-prompts", "none", "Reply with: ok"])
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::claude::subscription_only(&mut cmd);
    hidden(&mut cmd);
    let out = tokio::time::timeout(CHECK_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    if out.status.success() && !out.stdout.is_empty() {
        return Some(true);
    }
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).to_lowercase();
    let transient = ["rate limit", "usage limit", "overloaded", "network", "timed out", "timeout", "econn", "529", "503"];
    if transient.iter().any(|t| text.contains(t)) { None } else { Some(false) }
}

/// Refresh the Claude part: the plan every time (cheap), each exact version when its check is missing, older than
/// seven days or the plan changed. Publishes after every step so pickers fill in as answers arrive. Returns whether
/// Claude Code is signed in.
async fn refresh_claude(bin: &str, shared: &Shared, shutdown: &CancellationToken) -> bool {
    let Some(plan) = claude_plan(bin).await else {
        // `signed_in: false` lets pickers say so instead of waiting for a plan check that never comes.
        set(shared, "claude", json!({ "plan": null, "models": [], "signed_in": false }));
        return false;
    };
    let mut cache = load_cache();
    if cache.plan.as_deref() != Some(plan.as_str()) {
        cache.checked.clear();
        cache.plan = Some(plan.clone());
        save_cache(&cache);
    }
    let fresh = |c: &Checked| now().saturating_sub(c.at) < CACHE_FOR.as_secs();
    cache.checked.retain(|_, c| fresh(c));
    set(shared, "claude", claude_json(Some(&plan), &cache.checked));
    for (id, _) in CLAUDE_VERSIONS {
        if cache.checked.contains_key(id) {
            continue;
        }
        let answer = tokio::select! { a = verify(bin, id) => a, _ = shutdown.cancelled() => return true };
        if let Some(available) = answer {
            info!(model = id, available, "checked a Claude model against the plan");
            cache.checked.insert(id.to_owned(), Checked { available, at: now() });
            save_cache(&cache);
            set(shared, "claude", claude_json(Some(&plan), &cache.checked));
        }
    }
    true
}

/// Is the Codex CLI signed in (`codex login status` exits 0 and says "Logged in")?
async fn codex_signed_in(bin: &std::path::Path) -> bool {
    let mut cmd = Command::new(bin);
    cmd.args(["login", "status"]).stdin(Stdio::null()).kill_on_drop(true);
    hidden(&mut cmd);
    let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(20), cmd.output()).await else { return false };
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    out.status.success() && text.contains("Logged in")
}

async fn refresh_codex(bin: &str, shared: &Shared) {
    let exe = crate::codex::resolve_bin(bin);
    let models = if codex_signed_in(&exe).await { crate::codex::list_models(bin).await } else { None };
    let list: Vec<Value> = models.unwrap_or_default().into_iter().map(|(id, label)| json!({ "id": id, "label": label })).collect();
    set(shared, "codex", json!({ "models": list }));
}

fn set(shared: &Shared, key: &str, value: Value) {
    let mut v = shared.lock().unwrap();
    if !v.is_object() {
        *v = json!({ "claude": { "plan": null, "models": [] }, "codex": { "models": [] } });
    }
    v[key] = value;
}

/// At daemon start and every six hours until shutdown; every two minutes while Claude Code is signed out, so a
/// sign-in shows up in the pickers soon.
pub async fn run(cfg: Arc<Config>, shared: Shared, shutdown: CancellationToken) {
    if !cfg.check_models {
        return;
    }
    loop {
        let signed_in = refresh_claude(&cfg.claude_bin, &shared, &shutdown).await;
        refresh_codex(&cfg.codex_bin, &shared).await;
        let wait = if signed_in { REFRESH_EVERY } else { Duration::from_secs(120) };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = shutdown.cancelled() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_are_always_available_and_versions_start_unknown() {
        let mut checked = BTreeMap::new();
        checked.insert("claude-opus-5-5".to_owned(), Checked { available: false, at: 1 });
        let v = claude_json(Some("pro"), &checked);
        assert_eq!(v["plan"], "pro");
        let models = v["models"].as_array().unwrap();
        assert_eq!(models.len(), CLAUDE_ALIASES.len() + CLAUDE_VERSIONS.len());
        assert!(models.iter().filter(|m| m["alias"] == true).all(|m| m["available"] == true));
        let opus = models.iter().find(|m| m["id"] == "claude-opus-5-5").unwrap();
        assert_eq!((opus["label"].as_str(), opus["available"].as_bool()), (Some("Opus 5.5"), Some(false)));
        assert!(models.iter().find(|m| m["id"] == "claude-sonnet-5").unwrap()["available"].is_null());
    }

    #[test]
    fn shared_value_keeps_both_engines() {
        let shared: Shared = Arc::default();
        set(&shared, "codex", json!({ "models": [{ "id": "gpt-5-codex", "label": "GPT-5-Codex" }] }));
        let v = shared.lock().unwrap().clone();
        assert_eq!(v["claude"]["models"], json!([]));
        assert_eq!(v["codex"]["models"][0]["id"], "gpt-5-codex");
    }
}
