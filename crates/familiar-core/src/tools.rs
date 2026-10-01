//! Helper programs the daemon installs once into `~/.familiar/tools`, so runs never wait on `npx` checking the npm
//! registry (which can stall for minutes, e.g. while another npm install holds the cache lock).

use std::path::PathBuf;

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
