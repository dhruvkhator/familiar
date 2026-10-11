//! The command search path an app opened from Finder or the Dock should have. macOS starts GUI apps with launchd's
//! bare PATH (`/usr/bin:/bin:/usr/sbin:/sbin`), so `claude`, `codex`, `node`, `npx` and `uv` (Homebrew, npm, nvm and
//! installer locations) would all be "not installed". [`adopt`] asks the owner's login shell for its PATH once, at
//! start, and puts it (then the usual install places) in front of the inherited one, so every lookup and every child
//! process (Claude Code itself runs on `node`) sees what a Terminal sees. Other systems keep their PATH.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

/// How long the login shell may take (an nvm or oh-my-zsh setup takes a second or two); past it, the usual places only.
const SHELL_TIMEOUT: Duration = Duration::from_secs(6);
/// Brackets the PATH in the shell's output, so whatever its startup files print is ignored.
const MARK: &str = "__FAMILIAR_PATH__";

/// What [`adopt`] did, for the log once logging is up.
static OUTCOME: OnceLock<String> = OnceLock::new();

/// Take the login shell's PATH (macOS only; elsewhere nothing changes). Call first thing in `main`, before any other
/// thread starts: it changes the process environment.
pub fn adopt() {
    if !cfg!(target_os = "macos") || std::env::var_os("FAMILIAR_KEEP_PATH").is_some() {
        return;
    }
    let shell = login_shell();
    let from_shell = shell_path(&shell);
    let current = std::env::var_os("PATH").unwrap_or_default();
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.is_absolute());
    let merged = merge(from_shell.as_deref(), &current, &known_places(home.as_deref()));
    let Ok(joined) = std::env::join_paths(&merged) else { return };
    // SAFETY: called at the very start of `main`, before any other thread exists (see the doc comment), so nothing
    // reads the environment concurrently.
    unsafe { std::env::set_var("PATH", &joined) };
    let _ = OUTCOME.set(format!(
        "PATH from {} ({}): {}",
        shell.display(),
        if from_shell.is_some() { "login shell" } else { "login shell gave nothing; usual places only" },
        joined.to_string_lossy()
    ));
}

/// Log what [`adopt`] found, and whether Claude Code is on the path now (the first thing to check when runs fail).
pub fn log_outcome() {
    if let Some(o) = OUTCOME.get() {
        tracing::info!("{o}");
    }
    match find_program("claude") {
        Some(p) => tracing::info!("claude: {}", p.display()),
        None => tracing::warn!("claude is not on the PATH: install Claude Code (npm, Homebrew or its installer)"),
    }
}

/// `name` on the PATH, as the OS would find it.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).filter(|d| d.is_absolute()).map(|d| d.join(name)).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// The owner's shell (`$SHELL`, when it is an absolute path to a program), else zsh, macOS's default.
fn login_shell() -> PathBuf {
    std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|s| s.is_absolute() && is_executable(s))
        .unwrap_or_else(|| PathBuf::from("/bin/zsh"))
}

/// Run the shell as an interactive login shell (`.zprofile` and `.zshrc` both: nvm and most installers write to the
/// latter) and read its PATH. A fixed script, no input: nothing from outside reaches the command line. None when it
/// fails, prints no PATH or takes longer than [`SHELL_TIMEOUT`].
fn shell_path(shell: &Path) -> Option<String> {
    use std::io::Read as _;
    use std::process::{Command, Stdio};
    // `printf` with the PATH as an argument works the same in sh, bash, zsh and fish.
    let script = format!("printf '%s%s%s' {MARK} \"$PATH\" {MARK}");
    let mut child = Command::new(shell)
        .args(["-i", "-l", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Startup files that check it can skip slow work (oh-my-zsh's update prompt honours DISABLE_AUTO_UPDATE).
        .env("DISABLE_AUTO_UPDATE", "true")
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read on a helper thread so a chatty startup file can't fill the pipe and stall the shell; it touches no
    // environment, and a shell that leaves a background process holding the pipe only leaves this thread waiting.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("familiar-login-path".into())
        .spawn(move || {
            let mut out = Vec::new();
            let _ = (&mut stdout).take(1 << 20).read_to_end(&mut out);
            let _ = tx.send(out);
        })
        .ok()?;
    let out = rx.recv_timeout(SHELL_TIMEOUT);
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill();
    }
    let _ = child.wait();
    parse(&String::from_utf8_lossy(&out.ok()?))
}

/// The PATH between the marks, if the output has one.
fn parse(out: &str) -> Option<String> {
    let start = out.find(MARK)? + MARK.len();
    let end = start + out[start..].find(MARK)?;
    let path = out[start..end].trim();
    (!path.is_empty()).then(|| path.to_owned())
}

/// Where Claude Code, Codex, Node and uv usually live on a Mac, for when the shell gives nothing (or misses some).
fn known_places(home: Option<&Path>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin"].iter().map(PathBuf::from).collect();
    if let Some(h) = home {
        for rel in [".local/bin", ".claude/local", ".npm-global/bin", ".volta/bin", ".bun/bin", ".cargo/bin", "Library/pnpm"] {
            v.push(h.join(rel));
        }
        // nvm: the newest installed Node.
        if let Some(bin) = newest_nvm_node(&h.join(".nvm").join("versions").join("node")) {
            v.push(bin);
        }
    }
    v.retain(|p| p.is_dir());
    v
}

/// `<dir>/v22.3.0/bin` for the highest version under `dir`.
fn newest_nvm_node(dir: &Path) -> Option<PathBuf> {
    let version = |name: &str| -> Option<(u32, u32, u32)> {
        let mut it = name.strip_prefix('v')?.split('.').map(|n| n.parse::<u32>().ok());
        Some((it.next()??, it.next()??, it.next()??))
    };
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| version(&e.file_name().to_string_lossy()).map(|v| (v, e.path().join("bin"))))
        .max_by_key(|(v, _)| *v)
        .map(|(_, p)| p)
}

/// The login shell's entries first, then the inherited ones, then the usual places; each once, absolute only.
fn merge(from_shell: Option<&str>, current: &OsString, known: &[PathBuf]) -> Vec<PathBuf> {
    let shell: Vec<PathBuf> = from_shell.map(|s| std::env::split_paths(s).collect()).unwrap_or_default();
    let mut out: Vec<PathBuf> = Vec::new();
    for p in shell.into_iter().chain(std::env::split_paths(current)).chain(known.iter().cloned()) {
        if p.is_absolute() && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_is_read_between_the_marks() {
        let out = format!("Welcome to zsh!\nnvm loaded\n{MARK}/opt/homebrew/bin:/usr/bin{MARK}");
        assert_eq!(parse(&out).as_deref(), Some("/opt/homebrew/bin:/usr/bin"));
        assert_eq!(parse("no marks here"), None);
        assert_eq!(parse(&format!("{MARK}{MARK}")), None);
        assert_eq!(parse(&format!("{MARK}/usr/bin")), None);
    }

    #[cfg(unix)]
    #[test]
    fn merged_in_order_once_and_absolute_only() {
        let known = vec![PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/Users/me/.local/bin")];
        let merged = merge(Some("/opt/homebrew/bin:/usr/bin:relative/bin"), &OsString::from("/usr/bin:/bin"), &known);
        let want: Vec<PathBuf> =
            ["/opt/homebrew/bin", "/usr/bin", "/bin", "/Users/me/.local/bin"].iter().map(PathBuf::from).collect();
        assert_eq!(merged, want);
        let without_shell = merge(None, &OsString::from("/usr/bin:/bin"), &known);
        assert_eq!(without_shell[..2], [PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    }

    #[test]
    fn newest_node_under_nvm() {
        let dir = std::env::temp_dir().join(format!("familiar-nvm-{}", std::process::id()));
        for v in ["v18.20.1", "v22.3.0", "v20.11.0", "system"] {
            std::fs::create_dir_all(dir.join(v).join("bin")).unwrap();
        }
        assert_eq!(newest_nvm_node(&dir), Some(dir.join("v22.3.0").join("bin")));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(newest_nvm_node(&dir), None);
    }

    /// The real thing, where there is a login shell: its PATH comes back and includes the system folders.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_login_shell_answers() {
        let path = shell_path(&login_shell()).expect("the login shell printed a PATH");
        assert!(path.split(':').any(|p| p == "/usr/bin"), "{path}");
    }
}
