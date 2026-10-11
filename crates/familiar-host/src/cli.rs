//! Local things a host UI offers that are not part of the Familiar API: are the Claude Code / Codex CLIs installed
//! and signed in, open a terminal for sign-in or install, open a folder.

use std::path::Path;
use std::time::Duration;

use familiar_core::Config;
use serde::Serialize;

/// Install and sign-in state of one agent CLI (`claude` or `codex`).
#[derive(Debug, Clone, Serialize)]
pub struct CliStatus {
    pub installed: bool,
    pub version: Option<String>,
    pub logged_in: bool,
    pub auth_method: Option<String>,
    /// Codex only: the installed CLI is too old for ChatGPT sign-in models.
    pub needs_update: bool,
}

impl CliStatus {
    fn missing() -> Self {
        CliStatus { installed: false, version: None, logged_in: false, auth_method: None, needs_update: false }
    }
}

/// Run `bin args` hidden (no console window on Windows) with a 20 s limit.
async fn run(bin: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Option<std::process::Output> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    tokio::time::timeout(Duration::from_secs(20), cmd.output()).await.ok()?.ok()
}

/// Is Claude Code installed and signed in? Familiar runs entirely on that login.
pub async fn claude_status() -> CliStatus {
    let bin = Config::load().map(|c| c.claude_bin).unwrap_or_else(|_| "claude".into());
    let Some(version) = run(&bin, &["--version"]).await.filter(|o| o.status.success()) else {
        return CliStatus::missing();
    };
    let auth: serde_json::Value = run(&bin, &["auth", "status"])
        .await
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or_default();
    CliStatus {
        installed: true,
        version: Some(String::from_utf8_lossy(&version.stdout).trim().to_owned()),
        logged_in: auth["loggedIn"] == true,
        auth_method: auth["authMethod"].as_str().map(str::to_owned),
        needs_update: false,
    }
}

/// Can this computer find Claude Code at all (the configured `claude_bin`, on the PATH as the app sees it)? A quick
/// file check, answered once per start: the app's PATH is only read at start, so installing it means a restart.
pub fn claude_found() -> bool {
    static FOUND: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FOUND.get_or_init(|| {
        let bin = Config::load().map(|c| c.claude_bin).unwrap_or_else(|_| "claude".into());
        program_found(&bin, std::env::var_os("PATH").as_deref())
    })
}

/// `bin` as a path (when it names a folder) or on `path`; on Windows also with the extensions npm's shims use.
fn program_found(bin: &str, path: Option<&std::ffi::OsStr>) -> bool {
    let p = Path::new(bin);
    let exts: &[&str] = if cfg!(windows) { &["", "exe", "cmd", "bat"] } else { &[""] };
    let with = |p: &Path, ext: &str| if ext.is_empty() { p.is_file() } else { p.with_extension(ext).is_file() };
    if p.components().count() > 1 {
        return exts.iter().any(|e| with(p, e));
    }
    let dirs = path.map(|p| std::env::split_paths(p).collect::<Vec<_>>()).unwrap_or_default();
    dirs.iter().filter(|d| d.is_absolute()).any(|d| exts.iter().any(|e| with(&d.join(bin), e)))
}

/// What to do when Claude Code can't be found, in plain words (for the first-run screen and Today).
pub const CLAUDE_MISSING: &str = if cfg!(target_os = "macos") {
    "Your teammates run on Claude Code. Install it (in Terminal: npm install -g @anthropic-ai/claude-code, or \
     brew install --cask claude-code), sign in with `claude`, then quit and reopen Familiar."
} else {
    "Your teammates run on Claude Code. Install it (npm install -g @anthropic-ai/claude-code), sign in with `claude`, \
     then quit and reopen Familiar."
};

/// Is the OpenAI Codex CLI installed and signed in? Bots on the `codex` engine run on that login.
pub async fn codex_status() -> CliStatus {
    let bin = Config::load().map(|c| c.codex_bin).unwrap_or_else(|_| "codex".into());
    let bin = familiar_core::codex::resolve_bin(&bin);
    let Some(version) = run(&bin, &["--version"]).await.filter(|o| o.status.success()) else {
        return CliStatus::missing();
    };
    // `codex login status` prints "Logged in using ChatGPT" / "... an API key" (on stderr) and exits 0 when signed in.
    let status = run(&bin, &["login", "status"]).await;
    let text = status
        .as_ref()
        .map(|o| format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
        .unwrap_or_default();
    let logged_in = status.is_some_and(|o| o.status.success()) && text.contains("Logged in");
    let auth_method = logged_in.then(|| if text.contains("ChatGPT") { "chatgpt" } else { "api_key" }.to_owned());
    let version = String::from_utf8_lossy(&version.stdout).trim().to_owned();
    CliStatus { needs_update: codex_too_old(&version), installed: true, version: Some(version), logged_in, auth_method }
}

/// Familiar's Codex engine speaks the app-server v2 protocol, verified on 0.159; old CLIs (e.g. 0.46) can no longer run
/// any model with a ChatGPT sign-in.
fn codex_too_old(version: &str) -> bool {
    let nums: Vec<u32> = version
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .find(|s| s.contains('.'))
        .unwrap_or_default()
        .split('.')
        .filter_map(|n| n.parse().ok())
        .collect();
    matches!(nums.as_slice(), [0, minor, ..] if *minor < 159)
}

/// Open a visible terminal for a sign-in / install / update the owner asked for. Only these fixed actions:
/// `claude_login`, `claude_install`, `codex_login`, `codex_install`, `codex_update`. The CLIs then finish sign-in in
/// the browser on their own.
pub fn open_cli_terminal(action: &str) -> Result<(), String> {
    let (title, command) = match action {
        "claude_login" => ("Sign in to Claude", "claude auth login"),
        "codex_login" => ("Sign in to Codex", "codex login"),
        "codex_update" => ("Update Codex", "npm install -g @openai/codex@latest"),
        "codex_install" => ("Install Codex", "npm install -g @openai/codex@latest"),
        "claude_install" => ("Install Claude Code", "npm install -g @anthropic-ai/claude-code"),
        _ => return Err(format!("unknown action `{action}`")),
    };
    let spawned = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/c", "start", title, "cmd", "/k", &format!("{command} && echo. && echo Done - you can close this window.")])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("osascript")
            .args(["-e", &format!("tell application \"Terminal\" to do script \"{command}\""), "-e", "tell application \"Terminal\" to activate"])
            .spawn()
    } else {
        std::process::Command::new("x-terminal-emulator").args(["-e", "sh", "-c", &format!("{command}; exec sh")]).spawn()
    };
    spawned.map(|_| ()).map_err(|e| format!("could not open a terminal: {e}"))
}

/// Show a folder in Explorer / Finder / the desktop's file manager.
pub fn open_folder(dir: &Path) -> Result<(), String> {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    // explorer.exe exits non-zero even on success, so only spawn errors count.
    std::process::Command::new(program).arg(dir).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::codex_too_old;

    #[test]
    fn codex_version_gate() {
        assert!(codex_too_old("codex-cli 0.46.0"));
        assert!(codex_too_old("0.158.9"));
        assert!(!codex_too_old("codex-cli 0.159.0"));
        assert!(!codex_too_old("codex-cli 0.200.1-alpha.2"));
        assert!(!codex_too_old("codex-cli 1.2.0"));
        // unparseable output never blocks the user
        assert!(!codex_too_old(""));
        assert!(!codex_too_old("codex-cli dev"));
    }

    #[test]
    fn programs_are_found_on_the_path_or_by_path() {
        let dir = std::env::temp_dir().join(format!("familiar-cli-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = if cfg!(windows) { "claude.cmd" } else { "claude" };
        std::fs::write(dir.join(file), b"").unwrap();
        let path = std::env::join_paths([std::path::PathBuf::from("relative"), dir.clone()]).unwrap();
        assert!(super::program_found("claude", Some(&path)));
        assert!(!super::program_found("codex", Some(&path)));
        assert!(!super::program_found("claude", None));
        assert!(super::program_found(&dir.join("claude").display().to_string(), None));
        assert!(!super::program_found(&dir.join("nope").display().to_string(), None));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
