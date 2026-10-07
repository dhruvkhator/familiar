//! Folders on this PC the owner shares with a teammate (Cowork-style), each read only (the default) or read & write.
//!
//! - [`check`] decides whether a folder may be shared at all: it resolves links and junctions first, then refuses
//!   whole drives, the home folder (and folders that contain it), other users' folders, hidden settings folders in the
//!   home folder (`.ssh`, `.aws`, `.config`, `.claude`...), app data (saved passwords and sign-ins), Familiar's own
//!   data and every teammate's workspace, system and program folders, and network folders.
//! - [`active`] runs the same check again at the start of every run (and of every file-change decision): a folder that
//!   now resolves anywhere else (a link or junction changed) or became forbidden is skipped, with a notice.
//! - Claude gets `--add-dir` for each active folder: restricted mode confines its file tools to the workspace and these.
//!   Reading is free there; [`write_refusal`] refuses any file change outside the workspace and the read & write
//!   folders (so read-only folders stay unchanged), and every allowed change still asks the owner as before. Shell
//!   commands always ask the owner.
//! - Codex: Familiar runs it without a sandbox so that every patch and every command that isn't on Codex's read-only list
//!   comes back for approval, where the same [`write_refusal`] applies. Codex's own sandboxes never limit reading (its
//!   read-only commands can read anywhere on this PC), so on Codex sharing a folder can't confine what it reads; the
//!   app says so. The read & write folders are also passed as Codex's writable roots, which only matter if its
//!   workspace-write sandbox is ever used.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

/// A shared folder, as checked for this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub path: PathBuf,
    /// Read & write (else read only).
    pub write: bool,
}

/// At most this many folders per teammate.
pub const MAX_FOLDERS: usize = 20;

/// The places [`check`] protects, resolved for this computer.
#[derive(Debug, Clone)]
pub struct Env {
    pub home: PathBuf,
    /// Familiar's own data (`~/.familiar`).
    pub familiar_home: PathBuf,
    /// Every teammate's workspace.
    pub bots_dir: PathBuf,
    /// More folders that are never shared, nor any folder that contains them, with why.
    pub protected: Vec<(PathBuf, &'static str)>,
}

impl Env {
    /// This computer's protected places. `bots_dir`: where the teammates' workspaces are.
    pub fn current(bots_dir: &Path) -> Env {
        let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()).unwrap_or_default();
        let mut protected: Vec<(PathBuf, &'static str)> = vec![
            (home.join(".ssh"), "your SSH keys"),
            (home.join(".aws"), "your AWS keys"),
            (home.join(".config").join("gh"), "your GitHub sign-in"),
        ];
        let env = |k: &str| std::env::var_os(k).map(PathBuf::from).filter(|p| p.is_absolute());
        if cfg!(windows) {
            let sys = env("SystemRoot").or_else(|| env("windir")).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            protected.push((sys, "Windows system files"));
            for (k, fallback) in [
                ("ProgramFiles", r"C:\Program Files"),
                ("ProgramFiles(x86)", r"C:\Program Files (x86)"),
                ("ProgramW6432", r"C:\Program Files"),
            ] {
                protected.push((env(k).unwrap_or_else(|| PathBuf::from(fallback)), "installed programs"));
            }
            protected.push((env("ProgramData").unwrap_or_else(|| PathBuf::from(r"C:\ProgramData")), "programs' shared data"));
            let app_data = "app data (saved passwords and sign-ins live there)";
            protected.push((home.join("AppData"), app_data));
            for k in ["APPDATA", "LOCALAPPDATA"] {
                if let Some(p) = env(k) {
                    protected.push((p, app_data));
                }
            }
            if let Some(p) = env("APPDATA") {
                protected.push((p.join("Microsoft").join("Credentials"), "Windows saved credentials"));
            }
        } else {
            for p in [
                "/etc", "/usr", "/bin", "/sbin", "/lib", "/lib64", "/var", "/boot", "/dev", "/proc", "/sys", "/root",
                "/opt", "/snap", "/System", "/Library", "/private", "/Applications",
            ] {
                protected.push((PathBuf::from(p), "system files"));
            }
            protected.push((home.join("Library"), "app data (saved passwords and sign-ins live there)"));
        }
        Env { home, familiar_home: crate::Config::home_dir(), bots_dir: bots_dir.to_path_buf(), protected }
    }
}

/// Check a folder the owner wants to share: its resolved path (links and junctions followed), or why not.
pub fn check(raw: &str, env: &Env) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("Pick a folder.".into());
    }
    if raw.chars().count() > 1000 || raw.chars().any(char::is_control) {
        return Err("That path is too long or has unusual characters.".into());
    }
    if raw.starts_with(r"\\") || raw.starts_with("//") {
        return Err("Network folders can't be shared; pick a folder on this PC.".into());
    }
    let p = Path::new(raw);
    if !p.is_absolute() {
        return Err("Give the full path of a folder on this PC.".into());
    }
    let real = p.canonicalize().map_err(|_| "That folder doesn't exist on this PC.".to_owned())?;
    if !real.is_dir() {
        return Err("That's a file; pick a folder.".into());
    }
    let real = simplify(&real).ok_or("Network folders can't be shared; pick a folder on this PC.")?;
    match refusal(&real, env) {
        Some(why) => Err(format!("Familiar won't share this folder: {why}.")),
        None => Ok(real),
    }
}

/// Why a resolved folder must never be shared, if it must not.
fn refusal(c: &Path, env: &Env) -> Option<String> {
    let k = key(c);
    if c.parent().is_none() {
        return Some("it's a whole drive".into());
    }
    if c.components().any(|p| {
        let s = p.as_os_str().to_string_lossy().to_lowercase();
        s == "system volume information" || s == "$recycle.bin"
    }) {
        return Some("it holds system files".into());
    }
    let home = key(&real(&env.home));
    if !home.is_empty() {
        if k == home {
            return Some("it's your whole home folder".into());
        }
        if within(&home, &k) {
            return Some("it contains your home folder".into());
        }
        // C:\Users\<someone else>: only when the home folder's parent isn't a drive root (`/root` on Linux).
        let home_path = real(&env.home);
        if let Some(users) = home_path.parent().filter(|p| p.parent().is_some()) {
            if within(&k, &key(users)) && !within(&k, &home) {
                return Some("it's another user's folder".into());
            }
        }
        if within(&k, &home) {
            let rest = k[home.len()..].trim_start_matches(SEP);
            if rest.starts_with('.') {
                return Some("it's a hidden settings folder (keys and sign-ins live there)".into());
            }
        }
    }
    let fixed = [(&env.familiar_home, "Familiar's own data"), (&env.bots_dir, "your teammates' workspaces")];
    for (p, why) in fixed.into_iter().chain(env.protected.iter().map(|(p, w)| (p, *w))) {
        if p.as_os_str().is_empty() {
            continue;
        }
        let f = key(&real(p));
        if within(&k, &f) {
            return Some(format!("it's part of {why}"));
        }
        if within(&f, &k) {
            return Some(format!("it contains {why}"));
        }
    }
    None
}

/// The teammate's shared folders (`(path, mode)` rows) as they stand now: each checked again, and kept only when it
/// still resolves to exactly the stored path. Returns (active folders, skipped `(path, why)`).
pub fn active(rows: &[(String, String)], env: &Env) -> (Vec<Folder>, Vec<(String, String)>) {
    let mut ok = Vec::new();
    let mut skipped = Vec::new();
    for (path, mode) in rows {
        match check(path, env) {
            Ok(real) if key(&real) == key(Path::new(path)) => ok.push(Folder { path: real, write: mode == "write" }),
            Ok(_) => skipped.push((path.clone(), "it now leads somewhere else (a link or junction changed)".to_owned())),
            Err(why) => skipped.push((path.clone(), why)),
        }
    }
    (ok, skipped)
}

/// The tools that change files (Claude's file tools; Codex patches arrive as `Edit` with `file_paths`).
pub const WRITE_TOOLS: [&str; 4] = ["Write", "Edit", "MultiEdit", "NotebookEdit"];

/// Why this file change must not happen, if it must not: it changes something outside the workspace and the read & write
/// folders (a read-only folder included), or its target can't be told. None = it goes on to the usual approval.
pub fn write_refusal(tool: &str, input: &Value, workspace: &Path, folders: &[Folder]) -> Option<String> {
    if !WRITE_TOOLS.contains(&tool) {
        return None;
    }
    let mut targets: Vec<&str> = ["file_path", "notebook_path"].iter().filter_map(|k| input[*k].as_str()).collect();
    targets.extend(input["file_paths"].as_array().into_iter().flatten().filter_map(Value::as_str));
    if targets.is_empty() {
        return Some("Familiar couldn't tell which file this changes, so it was not allowed.".into());
    }
    let ws = key(&real(workspace));
    for t in targets {
        let p = Path::new(t);
        let full = if p.is_absolute() { p.to_path_buf() } else { workspace.join(p) };
        let Some(target) = resolve(&full) else {
            return Some(format!("`{t}` is not a plain file path, so it was not changed."));
        };
        let k = key(&target);
        if within(&k, &ws) {
            continue;
        }
        // The most specific shared folder decides (a read-only folder inside a read & write one stays read only).
        let owner = folders.iter().map(|f| (key(&f.path), f.write)).filter(|(f, _)| within(&k, f)).max_by_key(|(f, _)| f.len());
        match owner {
            Some((_, true)) => {}
            Some((_, false)) => {
                return Some(format!(
                    "`{t}` is in a folder your owner shared read only: you may read files there but never change, add \
                     or delete anything. Write your output to your workspace instead."
                ));
            }
            None => {
                return Some(format!(
                    "`{t}` is outside your workspace and the folders your owner shared for writing, so it was not changed."
                ));
            }
        }
    }
    None
}

/// The note for the teammate's instructions: its folders and what it may do there, and any it can't use this run.
pub fn note(folders: &[Folder], skipped: &[(String, String)]) -> Option<String> {
    if folders.is_empty() && skipped.is_empty() {
        return None;
    }
    let mut s = String::from("## Folders your owner shared\n");
    if !folders.is_empty() {
        s.push_str("Besides your workspace, you may use these folders on this PC:\n");
        for f in folders {
            let mode = if f.write {
                "read & write: changes still ask your owner first"
            } else {
                "read only: read files here, never change, add or delete anything"
            };
            s.push_str(&format!("- `{}` ({mode})\n", f.path.display()));
        }
        s.push_str("Nothing else outside your workspace is yours to read or change.\n");
    }
    for (path, why) in skipped {
        s.push_str(&format!("- Not available this run: `{path}` ({why}).\n"));
    }
    Some(s)
}

#[cfg(windows)]
const SEP: char = '\\';
#[cfg(not(windows))]
const SEP: char = '/';

/// A path without the `\\?\` prefix `canonicalize` adds on Windows; None for a network path (`\\?\UNC\...`).
fn simplify(p: &Path) -> Option<PathBuf> {
    let s = p.to_string_lossy();
    if s.starts_with(r"\\?\UNC\") || (s.starts_with(r"\\") && !s.starts_with(r"\\?\")) {
        return None;
    }
    Some(PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s)))
}

/// `p` resolved when it exists (links and junctions followed), else as given.
fn real(p: &Path) -> PathBuf {
    p.canonicalize().ok().and_then(|c| simplify(&c)).unwrap_or_else(|| p.to_path_buf())
}

/// The file a change would write: its deepest existing folder resolved, plus the plain names below it. None when a part
/// below that is `..`, `.` or (on Windows) names a data stream, or the path is a network path.
fn resolve(p: &Path) -> Option<PathBuf> {
    let mut existing = p.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(c) = existing.canonicalize() {
            let mut out = simplify(&c)?;
            for name in rest.iter().rev() {
                out.push(name);
            }
            return Some(out);
        }
        match existing.components().next_back() {
            Some(Component::Normal(name)) => {
                if cfg!(windows) && name.to_string_lossy().contains(':') {
                    return None;
                }
                rest.push(name.to_owned());
            }
            _ => return None,
        }
        if !existing.pop() {
            return None;
        }
    }
}

/// The comparable form of a path: separators unified, no trailing separator, lowercase on Windows.
fn key(p: &Path) -> String {
    let s = p.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    let s = if cfg!(windows) { s.replace('/', "\\").to_lowercase() } else { s.to_owned() };
    s.trim_end_matches(SEP).to_owned()
}

/// `child` is `parent` or inside it (both from [`key`]).
fn within(child: &str, parent: &str) -> bool {
    child == parent || (child.len() > parent.len() && child.starts_with(parent) && child[parent.len()..].starts_with(SEP))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A fake computer in a temp folder: a home with documents, hidden folders, app data, Familiar's data and two
    /// teammates' workspaces, another user, and a "system" folder.
    struct Fake {
        root: PathBuf,
        env: Env,
    }

    impl Fake {
        fn new() -> Fake {
            let root = std::env::temp_dir().join(format!("familiar-folders-{}", uuid::Uuid::new_v4().simple()));
            for d in [
                "Users/me/Documents/Taxes",
                "Users/me/Documents/Reports/out",
                "Users/me/.ssh",
                "Users/me/.config/gh",
                "Users/me/.claude",
                "Users/me/AppData/Roaming/Microsoft/Credentials",
                "Users/me/.familiar/bots/scout/notes",
                "Users/me/.familiar/bots/helper",
                "Users/other/Documents",
                "Windows/System32",
                "Data/Projects",
            ] {
                std::fs::create_dir_all(root.join(d)).unwrap();
            }
            std::fs::write(root.join("Users/me/Documents/Taxes/2025.pdf"), b"pdf").unwrap();
            let root = real(&root);
            let home = root.join("Users").join("me");
            let env = Env {
                familiar_home: home.join(".familiar"),
                bots_dir: home.join(".familiar").join("bots"),
                protected: vec![
                    (root.join("Windows"), "Windows system files"),
                    (home.join("AppData"), "app data (saved passwords and sign-ins live there)"),
                    (home.join(".ssh"), "your SSH keys"),
                ],
                home,
            };
            Fake { root, env }
        }

        fn p(&self, rel: &str) -> String {
            self.root.join(rel).display().to_string()
        }

        fn check(&self, rel: &str) -> Result<PathBuf, String> {
            check(&self.p(rel), &self.env)
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn ordinary_folders_are_shared_resolved() {
        let f = Fake::new();
        let ok = f.check("Users/me/Documents/Taxes").unwrap();
        assert_eq!(ok, f.root.join("Users").join("me").join("Documents").join("Taxes"));
        assert!(!ok.to_string_lossy().starts_with(r"\\?\"), "{ok:?}");
        assert!(f.check("Users/me/Documents").is_ok());
        assert!(f.check("Data/Projects").is_ok());
        // `..` and a trailing separator resolve to the real folder.
        assert_eq!(f.check("Users/me/Documents/Reports/../Taxes/").unwrap(), ok);
    }

    #[test]
    fn dangerous_folders_are_refused() {
        let f = Fake::new();
        let refused = |rel: &str, why: &str| {
            let e = f.check(rel).expect_err(rel);
            assert!(e.starts_with("Familiar won't share this folder") && e.contains(why), "{rel}: {e}");
        };
        refused("Users/me", "your whole home folder");
        refused("Users", "contains your home folder");
        refused("Users/other/Documents", "another user's folder");
        refused("Users/me/.ssh", "hidden settings folder");
        refused("Users/me/.config", "hidden settings folder");
        refused("Users/me/.config/gh", "hidden settings folder");
        refused("Users/me/.claude", "hidden settings folder");
        refused("Users/me/.familiar", "hidden settings folder");
        refused("Users/me/.familiar/bots/scout/notes", "hidden settings folder");
        refused("Users/me/AppData", "app data");
        refused("Users/me/AppData/Roaming/Microsoft/Credentials", "app data");
        refused("Windows/System32", "Windows system files");
        refused("Windows", "Windows system files");
        let root = f.root.ancestors().last().unwrap().display().to_string();
        assert!(check(&root, &f.env).unwrap_err().contains("whole drive"), "{root}");
        // Familiar's data and the workspaces are protected even outside the home folder.
        let env = Env { bots_dir: f.root.join("Data"), ..f.env.clone() };
        assert!(check(&f.p("Data/Projects"), &env).unwrap_err().contains("teammates' workspaces"));
        assert!(check(&f.p(""), &env).unwrap_err().contains("contains"));
    }

    #[test]
    fn bad_input_is_refused() {
        let f = Fake::new();
        for (raw, why) in [
            ("", "Pick a folder"),
            ("  ", "Pick a folder"),
            ("Documents", "full path"),
            (r"\\server\share\x", "Network folders"),
            (r"\\?\C:\Users", "Network folders"),
            ("//server/share", "Network folders"),
            ("C:\\a\u{0}b", "unusual characters"),
        ] {
            let e = check(raw, &f.env).unwrap_err();
            assert!(e.contains(why), "{raw:?}: {e}");
        }
        assert!(f.check("Users/me/Documents/nope").unwrap_err().contains("doesn't exist"));
        assert!(f.check("Users/me/Documents/Taxes/2025.pdf").unwrap_err().contains("file"));
    }

    #[cfg(windows)]
    fn link(target: &Path, at: &Path) -> bool {
        // A junction needs no special rights on Windows; mklink wants backslashes.
        let win = |p: &Path| p.display().to_string().replace('/', "\\");
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J", &win(at), &win(target)])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[cfg(unix)]
    fn link(target: &Path, at: &Path) -> bool {
        std::os::unix::fs::symlink(target, at).is_ok()
    }

    #[test]
    fn links_are_followed_before_checking() {
        let f = Fake::new();
        let at = f.root.join("Data").join("innocent");
        assert!(link(&f.root.join("Users/me/.ssh"), &at), "could not create a link");
        let e = check(&at.display().to_string(), &f.env).unwrap_err();
        assert!(e.contains("hidden settings folder"), "{e}");
        let at2 = f.root.join("Data").join("docs");
        assert!(link(&f.root.join("Users/me/Documents"), &at2));
        assert_eq!(check(&at2.display().to_string(), &f.env).unwrap(), f.root.join("Users").join("me").join("Documents"));
    }

    #[test]
    fn re_checked_at_run_start() {
        let f = Fake::new();
        let shared = f.root.join("Data").join("Shared");
        std::fs::create_dir_all(&shared).unwrap();
        let docs = f.check("Users/me/Documents/Taxes").unwrap();
        let rows = vec![
            (docs.display().to_string(), "read".to_owned()),
            (shared.display().to_string(), "write".to_owned()),
            (f.p("Users/me/Documents/Gone"), "read".to_owned()),
        ];
        let (ok, skipped) = active(&rows, &f.env);
        assert_eq!(ok, vec![Folder { path: docs.clone(), write: false }, Folder { path: shared.clone(), write: true }]);
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].1.contains("doesn't exist"), "{skipped:?}");
        // The shared folder is swapped for a junction / link to a protected place: skipped with a notice.
        std::fs::remove_dir(&shared).unwrap();
        assert!(link(&f.root.join("Users/me/.ssh"), &shared));
        let (ok, skipped) = active(&rows, &f.env);
        assert_eq!(ok, vec![Folder { path: docs, write: false }]);
        assert!(skipped.iter().any(|(p, why)| *p == shared.display().to_string() && why.contains("hidden settings folder")), "{skipped:?}");
        // ...or to an allowed but different place: skipped too (it isn't what the owner shared).
        std::fs::remove_dir(&shared).unwrap();
        assert!(link(&f.root.join("Data/Projects"), &shared));
        let (_, skipped) = active(&rows, &f.env);
        assert!(skipped.iter().any(|(_, why)| why.contains("leads somewhere else")), "{skipped:?}");
    }

    #[test]
    fn writes_only_in_the_workspace_and_read_write_folders() {
        let f = Fake::new();
        let ws = f.root.join("Users/me/.familiar/bots/helper");
        let ro = f.check("Users/me/Documents/Taxes").unwrap();
        let rw = f.check("Data/Projects").unwrap();
        std::fs::create_dir_all(rw.join("originals")).unwrap();
        let nested_ro = f.check("Data/Projects/originals").unwrap();
        let folders = vec![
            Folder { path: ro.clone(), write: false },
            Folder { path: rw.clone(), write: true },
            Folder { path: nested_ro.clone(), write: false },
        ];
        let w = |tool: &str, path: &Path| write_refusal(tool, &json!({ "file_path": path.display().to_string(), "content": "x" }), &ws, &folders);
        assert_eq!(w("Write", &ws.join("notes.md")), None);
        assert_eq!(write_refusal("Write", &json!({ "file_path": "out/report.md" }), &ws, &folders), None, "relative = workspace");
        assert_eq!(w("Write", &rw.join("new").join("report.md")), None);
        assert_eq!(w("Edit", &rw.join("a.txt")), None);
        for (tool, p) in [
            ("Write", ro.join("2025.pdf")),
            ("Edit", ro.join("2025.pdf")),
            ("MultiEdit", ro.join("new.txt")),
            ("Write", ro.join("deep").join("new.txt")),
            ("Write", nested_ro.join("a.txt")),
            ("Write", rw.join("..").join("..").join("Users/me/Documents/Taxes/x.txt")),
        ] {
            let why = w(tool, &p).unwrap_or_else(|| panic!("{tool} {p:?} allowed"));
            assert!(why.contains("read only"), "{why}");
        }
        assert!(write_refusal("NotebookEdit", &json!({ "notebook_path": ro.join("n.ipynb").display().to_string() }), &ws, &folders).is_some());
        for p in [f.root.join("Users/other/Documents/x.txt"), f.root.join("Users/me/.familiar/bots/scout/notes/x.md"), f.root.join("elsewhere.txt")] {
            let why = w("Write", &p).unwrap_or_else(|| panic!("{p:?} allowed"));
            assert!(why.contains("outside your workspace"), "{why}");
        }
        assert!(write_refusal("Write", &json!({ "file_path": "../scout/notes/x.md" }), &ws, &folders).unwrap().contains("outside"));
        // Codex patches: every path counts.
        let patch = json!({ "file_paths": [ws.join("a.md").display().to_string(), ro.join("b.md").display().to_string()] });
        assert!(write_refusal("Edit", &patch, &ws, &folders).unwrap().contains("read only"));
        assert!(write_refusal("Edit", &json!({ "changes": [] }), &ws, &folders).unwrap().contains("couldn't tell"));
        #[cfg(windows)]
        assert!(w("Write", &PathBuf::from(format!("{}:stream", rw.join("new").display()))).is_some());
        // Reads and other tools are not this check's business.
        assert_eq!(write_refusal("Read", &json!({ "file_path": ro.join("2025.pdf").display().to_string() }), &ws, &folders), None);
        assert_eq!(write_refusal("Bash", &json!({ "command": "echo x > a" }), &ws, &folders), None);
    }

    #[test]
    fn instructions_note() {
        assert_eq!(note(&[], &[]), None);
        let n = note(
            &[Folder { path: PathBuf::from("/data/taxes"), write: false }, Folder { path: PathBuf::from("/data/out"), write: true }],
            &[("/data/old".into(), "it doesn't exist".into())],
        )
        .unwrap();
        assert!(n.starts_with("## Folders your owner shared\n"), "{n}");
        assert!(n.contains("- `/data/taxes` (read only: read files here, never change, add or delete anything)"), "{n}");
        assert!(n.contains("- `/data/out` (read & write: changes still ask your owner first)"), "{n}");
        assert!(n.contains("Not available this run: `/data/old` (it doesn't exist)"), "{n}");
    }
}
