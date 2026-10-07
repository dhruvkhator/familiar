//! Helper programs the daemon installs once into `~/.familiar/tools`, so runs never wait on `npx` checking the npm
//! registry (which can stall for minutes, e.g. while another npm install holds the cache lock): Playwright MCP (the
//! teammates' browser) and, on Windows once a teammate gets desktop control, Windows-MCP (with uv, never `uvx`).

use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::config::Config;

/// Pinned so a surprise upstream release can't change what the bot can do.
pub const PLAYWRIGHT_MCP_VERSION: &str = "0.0.83";

fn dir() -> PathBuf {
    Config::home_dir().join("tools")
}

/// `cli.js` of the installed Playwright MCP, when the pinned version is present.
pub fn playwright_cli() -> Option<PathBuf> {
    let pkg = dir().join("node_modules").join("@playwright").join("mcp");
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(pkg.join("package.json")).ok()?).ok()?;
    (manifest["version"] == PLAYWRIGHT_MCP_VERSION).then(|| pkg.join("cli.js")).filter(|p| p.is_file())
}

/// Install the pinned Playwright MCP if it's missing (runs fall back to `npx` until it is there).
pub async fn ensure_playwright() {
    if playwright_cli().is_some() {
        return;
    }
    let dir = dir();
    let _ = std::fs::create_dir_all(&dir);
    info!("installing Playwright MCP {PLAYWRIGHT_MCP_VERSION} into {}", dir.display());
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let mut cmd = tokio::process::Command::new(npm);
    cmd.args(["install", "--prefix"])
        .arg(&dir)
        .arg(format!("@playwright/mcp@{PLAYWRIGHT_MCP_VERSION}"))
        .args(["--no-audit", "--no-fund", "--loglevel=error"])
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    match tokio::time::timeout(std::time::Duration::from_secs(600), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() && playwright_cli().is_some() => info!("Playwright MCP installed"),
        Ok(Ok(out)) => warn!("Playwright MCP install failed: {}", String::from_utf8_lossy(&out.stderr).trim()),
        Ok(Err(e)) => warn!("could not run npm to install Playwright MCP: {e}"),
        Err(_) => warn!("Playwright MCP install timed out"),
    }
}

// ---- Windows-MCP (desktop control, see crate::desktop) ----

/// Pinned: a surprise upstream release can't change what a teammate can do on the desktop.
pub const WINDOWS_MCP_VERSION: &str = "0.8.7";
/// Windows-MCP 0.8.7 needs Python 3.14 or newer; uv downloads its own copy into the tools folder.
pub const WINDOWS_MCP_PYTHON: &str = "3.14";
/// Its dependencies are resolved as they were on this date (0.8.7 was released on 2026-09-30), so a later release of
/// one of them can't change what gets installed either.
pub const WINDOWS_MCP_EXCLUDE_NEWER: &str = "2026-10-01T00:00:00Z";

/// Where Windows-MCP lives, all of it under one folder: uv's tool environments, the launcher, the Python it runs on,
/// uv's cache, and the (empty) config file the server is pointed at instead of `~/.windows-mcp/config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsMcp {
    pub root: PathBuf,
    pub tools: PathBuf,
    pub bin: PathBuf,
    pub python: PathBuf,
    pub python_bin: PathBuf,
    pub cache: PathBuf,
    pub config: PathBuf,
}

impl WindowsMcp {
    pub fn at(root: &Path) -> WindowsMcp {
        WindowsMcp {
            root: root.to_path_buf(),
            tools: root.join("uv-tools"),
            bin: root.join("bin"),
            python: root.join("python"),
            python_bin: root.join("python-bin"),
            cache: root.join("uv-cache"),
            config: root.join("config.toml"),
        }
    }

    /// `~/.familiar/tools/windows-mcp`.
    pub fn default_location() -> WindowsMcp {
        WindowsMcp::at(&dir().join("windows-mcp"))
    }

    pub fn exe(&self) -> PathBuf {
        self.bin.join(if cfg!(windows) { "windows-mcp.exe" } else { "windows-mcp" })
    }

    /// The launcher, when the pinned version is installed here.
    pub fn installed(&self) -> Option<PathBuf> {
        let exe = self.exe();
        let site = self.tools.join("windows-mcp").join("Lib").join("site-packages");
        let pinned = site.join(format!("windows_mcp-{WINDOWS_MCP_VERSION}.dist-info"));
        (exe.is_file() && pinned.is_dir()).then_some(exe)
    }

    /// `uv tool install` of the pinned version, everything inside [`WindowsMcp::root`]: (arguments, environment).
    pub fn install_command(&self) -> (Vec<String>, Vec<(&'static str, String)>) {
        let args = [
            "tool", "install", "--managed-python", "--no-config", "--python", WINDOWS_MCP_PYTHON, "--exclude-newer",
            WINDOWS_MCP_EXCLUDE_NEWER,
        ]
        .into_iter()
        .map(str::to_owned)
        .chain([format!("windows-mcp=={WINDOWS_MCP_VERSION}")])
        .collect();
        let p = |p: &Path| p.display().to_string();
        let env = vec![
            ("UV_TOOL_DIR", p(&self.tools)),
            ("UV_TOOL_BIN_DIR", p(&self.bin)),
            ("UV_PYTHON_INSTALL_DIR", p(&self.python)),
            ("UV_PYTHON_BIN_DIR", p(&self.python_bin)),
            ("UV_CACHE_DIR", p(&self.cache)),
            ("UV_PYTHON_INSTALL_REGISTRY", "0".to_owned()),
            ("UV_NO_MODIFY_PATH", "1".to_owned()),
        ];
        (args, env)
    }
}

/// Where desktop control stands on this PC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Desktop {
    /// Not Windows.
    Unsupported,
    /// uv (Astral's Python package manager) isn't installed, so Windows-MCP can't be.
    NoUv,
    /// Being installed (the first time takes a minute or two).
    Installing,
    /// The last install failed: why.
    Failed(String),
    /// Not installed yet (it is installed once a teammate has desktop control on).
    Missing,
    /// Installed: the launcher.
    Ready(PathBuf),
}

impl Desktop {
    /// For the app and the teammate's instructions.
    pub fn message(&self) -> String {
        match self {
            Desktop::Unsupported => "Desktop control works on Windows only.".into(),
            Desktop::NoUv => "Install uv to enable desktop control (winget install astral-sh.uv), then restart Familiar.".into(),
            Desktop::Installing => "Setting up desktop control (one time, a minute or two)…".into(),
            Desktop::Failed(why) => format!("Desktop control could not be set up: {why}"),
            Desktop::Missing => "Desktop control is set up the first time a teammate gets it.".into(),
            Desktop::Ready(_) => "Ready.".into(),
        }
    }
}

/// The installer's state (one install at a time; a failure is remembered until the next attempt).
static WINDOWS_MCP_STATE: std::sync::Mutex<Option<Desktop>> = std::sync::Mutex::new(None);

/// Where desktop control stands now.
pub fn desktop_status() -> Desktop {
    if !cfg!(windows) {
        return Desktop::Unsupported;
    }
    if let Some(exe) = WindowsMcp::default_location().installed() {
        return Desktop::Ready(exe);
    }
    if let Some(s) = WINDOWS_MCP_STATE.lock().unwrap().clone() {
        return s;
    }
    if find_uv().is_none() { Desktop::NoUv } else { Desktop::Missing }
}

/// uv on this PC: on PATH, or where its installers put it.
pub fn find_uv() -> Option<PathBuf> {
    let name = if cfg!(windows) { "uv.exe" } else { "uv" };
    let on_path = std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>());
    let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let known = [
        local.map(|l| l.join("Microsoft").join("WinGet").join("Links")),
        home.as_ref().map(|h| h.join(".local").join("bin")),
        home.as_ref().map(|h| h.join(".cargo").join("bin")),
    ];
    on_path.chain(known.into_iter().flatten()).map(|d| d.join(name)).find(|p| p.is_file())
}

/// Install the pinned Windows-MCP once (never at run time): with uv, into `~/.familiar/tools/windows-mcp`. Without uv
/// this only records that uv is needed.
pub async fn ensure_windows_mcp() {
    if !cfg!(windows) {
        return;
    }
    let at = WindowsMcp::default_location();
    if at.installed().is_some() {
        return;
    }
    // Without uv, `desktop_status` says so (and notices once it is installed).
    let Some(uv) = find_uv() else { return };
    {
        let mut s = WINDOWS_MCP_STATE.lock().unwrap();
        if *s == Some(Desktop::Installing) {
            return;
        }
        *s = Some(Desktop::Installing);
    }
    let _ = std::fs::create_dir_all(&at.root);
    info!("installing Windows-MCP {WINDOWS_MCP_VERSION} into {}", at.root.display());
    let (args, env) = at.install_command();
    let mut cmd = tokio::process::Command::new(&uv);
    cmd.args(&args).envs(env).current_dir(&at.root).stdout(std::process::Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let outcome = match tokio::time::timeout(std::time::Duration::from_secs(900), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() && at.installed().is_some() => {
            info!("Windows-MCP installed");
            None
        }
        Ok(Ok(out)) => {
            let err = String::from_utf8_lossy(&out.stderr);
            let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("uv failed").trim().to_owned();
            warn!("Windows-MCP install failed: {}", err.trim());
            Some(Desktop::Failed(last))
        }
        Ok(Err(e)) => Some(Desktop::Failed(format!("could not run uv: {e}"))),
        Err(_) => Some(Desktop::Failed("the install timed out".into())),
    };
    *WINDOWS_MCP_STATE.lock().unwrap() = outcome;
}

/// The (empty) config file the server reads instead of `~/.windows-mcp/config.toml`, created when missing.
pub fn windows_mcp_config(at: &WindowsMcp) -> std::io::Result<PathBuf> {
    if !at.config.is_file() {
        std::fs::create_dir_all(&at.root)?;
        std::fs::write(&at.config, "# Familiar's Windows-MCP settings: none (everything is on its command line).\n")?;
    }
    Ok(at.config.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_mcp_lives_in_one_folder() {
        let root = std::env::temp_dir().join(format!("familiar-wmcp-{}", uuid::Uuid::new_v4().simple()));
        let at = WindowsMcp::at(&root);
        let (args, env) = at.install_command();
        assert_eq!(args.last().map(String::as_str), Some("windows-mcp==0.8.7"), "pinned");
        let flag = |f: &str| args.iter().position(|a| a == f).map(|i| args[i + 1].as_str());
        assert_eq!(flag("--python"), Some("3.14"));
        assert_eq!(flag("--exclude-newer"), Some(WINDOWS_MCP_EXCLUDE_NEWER));
        assert!(args.contains(&"--managed-python".to_owned()) && args.contains(&"--no-config".to_owned()));
        assert_eq!(&args[..2], ["tool", "install"], "installed once, never `uvx` at run time");
        for (k, v) in &env {
            if k.ends_with("_DIR") {
                assert!(Path::new(v).starts_with(&root), "{k}={v} outside the tools folder");
            }
        }
        let keys: Vec<&str> = env.iter().map(|(k, _)| *k).collect();
        for k in ["UV_TOOL_DIR", "UV_TOOL_BIN_DIR", "UV_PYTHON_INSTALL_DIR", "UV_PYTHON_BIN_DIR", "UV_CACHE_DIR"] {
            assert!(keys.contains(&k), "{k}");
        }
        assert!(env.contains(&("UV_PYTHON_INSTALL_REGISTRY", "0".into())));

        // Installed = the launcher and the pinned version's package; another version doesn't count.
        assert_eq!(at.installed(), None);
        std::fs::create_dir_all(&at.bin).unwrap();
        std::fs::write(at.exe(), b"exe").unwrap();
        let site = at.tools.join("windows-mcp").join("Lib").join("site-packages");
        std::fs::create_dir_all(site.join("windows_mcp-0.8.6.dist-info")).unwrap();
        assert_eq!(at.installed(), None);
        std::fs::create_dir_all(site.join("windows_mcp-0.8.7.dist-info")).unwrap();
        assert_eq!(at.installed(), Some(at.exe()));
        let cfg = windows_mcp_config(&at).unwrap();
        assert!(cfg.starts_with(&root) && cfg.is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn desktop_messages() {
        assert!(Desktop::NoUv.message().starts_with("Install uv to enable desktop control"));
        assert!(Desktop::Unsupported.message().contains("Windows only"));
    }
}
