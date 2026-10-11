//! Applying a staged update: a detached helper (this same program, started with `--apply-update`) waits for the app
//! to quit, verifies the staged artifact once more, installs it, and starts Familiar again, the new version or, when
//! the install failed, the old one, which then reads [`RESULT`] and says what happened.
//!
//! - **Windows**: the helper runs from a copy of the exe in the update's folder (so it holds no file the installer
//!   replaces), opens the installer with a share mode that stops anyone writing, renaming or deleting it until the
//!   install is over, verifies it from that handle and runs it silently (`/VERYSILENT /SUPPRESSMSGBOXES /NORESTART`:
//!   same AppId, so an in-place per-user upgrade). Then it starts `familiar-native.exe` from the install folder the
//!   installer registered (falling back to the exe that was running).
//! - **macOS**: the helper verifies the archive, unpacks it next to the running `Familiar.app` (same volume), checks
//!   the new bundle holds the app's binary, and swaps the bundles with two renames, putting the old one back if the
//!   second fails, then opens the bundle. Downloaded by Familiar itself, the files carry no quarantine flag, so there
//!   is no Gatekeeper prompt. The swap follows zeron's `apply_mac_app` (MIT, github.com/zeronsh/zeron).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::source::Staged;
use super::verify::{self, MANIFEST, Platform, SIG_EXT};

/// The helper's flag; it must come first.
pub const FLAG: &str = "--apply-update";
/// The helper's report for the next start, in the updates folder.
pub const RESULT: &str = "result.json";
/// The last installer run's log, kept beside [`RESULT`] (the update's own folder is pruned later).
pub const INSTALL_LOG: &str = "install.log";
/// The report is small; anything bigger isn't ours.
const MAX_RESULT: u64 = 64 * 1024;
/// A report's message is cut to this (Settings shows it).
const MAX_MESSAGE: usize = 1_000;
/// How long the helper waits for the app to finish quitting (its engine drains runs for up to ~45 s).
const WAIT_FOR_EXIT: Duration = Duration::from_secs(5 * 60);

/// What the last update attempt did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub version: String,
    pub ok: bool,
    pub message: String,
}

/// The last attempt's outcome, read once (the file is removed). It is only data for Settings, and checked like data
/// from outside: at most 64 KB, a release version, the message as plain text and short.
pub fn take_outcome(updates: &Path) -> Option<Outcome> {
    use std::io::Read as _;
    let path = updates.join(RESULT);
    let mut bytes = Vec::new();
    let read = std::fs::File::open(&path).ok()?.take(MAX_RESULT + 1).read_to_end(&mut bytes);
    let _ = std::fs::remove_file(&path);
    if read.is_err() || bytes.len() as u64 > MAX_RESULT {
        return None;
    }
    let o: Outcome = serde_json::from_slice(&bytes).ok()?;
    verify::parse_version(&o.version)?;
    let mut message = verify::plain_notes(&o.message);
    if message.chars().count() > MAX_MESSAGE {
        message = format!("{}…", message.chars().take(MAX_MESSAGE).collect::<String>().trim_end());
    }
    Some(Outcome { version: o.version, ok: o.ok, message })
}

// ---- in the app ---------------------------------------------------------------------------------------------------

/// Start the helper for `staged`; it waits for this process to exit, so the caller quits right after.
pub fn launch(staged: &Staged) -> Result<(), String> {
    if !super::OFFICIAL {
        return Err("This is a developer build: it doesn't install updates.".into());
    }
    let exe = std::env::current_exe().map_err(|e| format!("couldn't find Familiar's program file: {e}"))?;
    let stage = staged.artifact.parent().ok_or("the update has no folder")?;
    launch_for(&exe, stage, staged)
}

#[cfg(windows)]
fn launch_for(exe: &Path, stage: &Path, staged: &Staged) -> Result<(), String> {
    use std::os::windows::process::CommandExt as _;
    use windows_sys::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
    // A copy: the installer replaces the installed exe, so the helper must not run from it.
    let helper = stage.join("familiar-update-helper.exe");
    std::fs::copy(exe, &helper).map_err(|e| format!("couldn't prepare the update: {e}"))?;
    let command = |flags| {
        std::process::Command::new(&helper)
            .arg(FLAG)
            .arg(&staged.artifact)
            .arg("--wait")
            .arg(std::process::id().to_string())
            .arg("--relaunch")
            .arg(exe)
            .current_dir(stage)
            .creation_flags(flags)
            .spawn()
    };
    // No console (a debug build has one), its own process group.
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    // Out of any job this app runs in (a job may close with the app); not every job allows that.
    command(base | CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| command(base))
        .map(drop)
        .map_err(|e| format!("couldn't start the updater: {e}"))
}

#[cfg(target_os = "macos")]
fn launch_for(exe: &Path, _stage: &Path, staged: &Staged) -> Result<(), String> {
    use std::os::unix::process::CommandExt as _;
    let bundle = mac::bundle_of(exe).ok_or("Familiar isn't running from Familiar.app, so it can't replace itself.")?;
    mac::check_writable(&bundle)?;
    std::process::Command::new(exe)
        .arg(FLAG)
        .arg(&staged.artifact)
        .arg("--wait")
        .arg(std::process::id().to_string())
        .arg("--relaunch")
        .arg(&bundle)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map(drop)
        .map_err(|e| format!("couldn't start the updater: {e}"))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn launch_for(_exe: &Path, _stage: &Path, _staged: &Staged) -> Result<(), String> {
    Err("Familiar doesn't update itself on this system.".into())
}

// ---- the helper ---------------------------------------------------------------------------------------------------

struct HelperArgs {
    artifact: PathBuf,
    pid: u32,
    relaunch: PathBuf,
}

fn parse(args: &[OsString]) -> Option<HelperArgs> {
    if args.get(1).and_then(|a| a.to_str()) != Some(FLAG) {
        return None;
    }
    let mut out = HelperArgs { artifact: PathBuf::from(args.get(2)?), pid: 0, relaunch: PathBuf::new() };
    let mut it = args.iter().skip(3);
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("--wait") => out.pid = it.next()?.to_str()?.parse().ok()?,
            Some("--relaunch") => out.relaunch = PathBuf::from(it.next()?),
            _ => return None,
        }
    }
    (out.pid != 0 && !out.relaunch.as_os_str().is_empty()).then_some(out)
}

/// `main` calls this first: when started as the helper, apply the update and return the exit code.
pub fn run_if_helper() -> Option<i32> {
    let args: Vec<OsString> = std::env::args_os().collect();
    if args.get(1).and_then(|a| a.to_str()) != Some(FLAG) {
        return None;
    }
    familiar_host::init_logging("update");
    let Some(args) = parse(&args) else {
        tracing::error!("updater: bad arguments");
        return Some(2);
    };
    Some(helper(args))
}

fn helper(args: HelperArgs) -> i32 {
    let Some(stage) = args.artifact.parent().map(Path::to_path_buf) else { return 2 };
    let updates = stage.parent().map(Path::to_path_buf).unwrap_or_else(|| stage.clone());
    let version = stage.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    tracing::info!("updater: waiting for Familiar ({}) to quit, then installing {version}", args.pid);
    if !wait_for_exit(args.pid, WAIT_FOR_EXIT) {
        // Still running: nothing was touched, and it needs no restart.
        let message = "Familiar didn't finish quitting in time, so the update wasn't installed. Try again.".to_owned();
        tracing::error!("updater: {message}");
        record(&updates, &version, false, &message);
        return 1;
    }
    let result = install(&args.artifact, &args.relaunch);
    let (ok, message) = match &result {
        Ok(()) => (true, format!("Updated to {version}.")),
        Err(e) => (false, e.clone()),
    };
    if ok {
        tracing::info!("updater: {message}");
    } else {
        tracing::error!("updater: {message}");
    }
    record(&updates, &version, ok, &message);
    relaunch(&args.relaunch, ok);
    if ok { 0 } else { 1 }
}

fn record(updates: &Path, version: &str, ok: bool, message: &str) {
    let outcome = Outcome { version: version.to_owned(), ok, message: message.to_owned() };
    if let Ok(json) = serde_json::to_vec_pretty(&outcome) {
        let _ = std::fs::write(updates.join(RESULT), json);
    }
}

/// The staged manifest, verified again, and what it says about `artifact`: never older than (or the same as) the
/// version doing the installing.
fn reverify_manifest(artifact: &Path, platform: Platform) -> Result<(semver::Version, verify::FileEntry, Vec<u8>), String> {
    let stage = artifact.parent().ok_or("the update has no folder")?;
    let bytes = std::fs::read(stage.join(MANIFEST)).map_err(|e| format!("the update's manifest is missing: {e}"))?;
    let sig = std::fs::read(stage.join(format!("{MANIFEST}{SIG_EXT}"))).map_err(|e| format!("the update's signature is missing: {e}"))?;
    // The version named inside is only a claim until the signature (whose trusted comment names it) verifies.
    #[derive(Deserialize)]
    struct Claim {
        version: String,
    }
    let claim: Claim = serde_json::from_slice(&bytes).map_err(|_| "the update's manifest is malformed".to_owned())?;
    let version = verify::parse_version(&claim.version).ok_or("the update's version is malformed")?;
    let manifest = verify::verify_manifest(&bytes, &sig, &verify::release_keys(), &version, super::BUILT_AT)?;
    let current = verify::parse_version(super::VERSION).ok_or("this build's version is malformed")?;
    if version <= current {
        return Err(format!("{version} isn't newer than {current}"));
    }
    let name = platform.artifact(&version);
    if artifact.file_name().and_then(|n| n.to_str()) != Some(name.as_str()) {
        return Err("the update file isn't the one the manifest names".into());
    }
    let entry = manifest.files.get(&name).ok_or_else(|| format!("the release has no {name}"))?.clone();
    let file_sig = std::fs::read(stage.join(format!("{name}{SIG_EXT}"))).map_err(|e| format!("the update's signature is missing: {e}"))?;
    Ok((version, entry, file_sig))
}

#[cfg(windows)]
fn install(artifact: &Path, _relaunch: &Path) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::process::CommandExt as _;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
    let (version, entry, sig) = reverify_manifest(artifact, Platform::Windows)?;
    let name = Platform::Windows.artifact(&version);
    // Held until the installer has finished: nobody can change the file between the check and the run.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(artifact)
        .map_err(|e| format!("couldn't open the installer: {e}"))?;
    verify::verify_artifact(std::io::BufReader::new(&file), &sig, &verify::release_keys(), &name, &version, &entry)?;
    let log = artifact.with_file_name("install.log");
    let status = std::process::Command::new(artifact)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/NOCANCEL", "/SP-"])
        .arg(format!("/LOG={}", log.display()))
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("couldn't run the installer: {e}"))?;
    drop(file);
    // Kept beside the report: the update's folder goes at the next check.
    let kept = artifact.parent().and_then(Path::parent).map(|u| u.join(INSTALL_LOG)).unwrap_or_else(|| log.clone());
    let log = if std::fs::copy(&log, &kept).is_ok() { kept } else { log };
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(format!("{} (installer exit code {code}; its log is {}).", inno_failure(code), log.display())),
        None => Err("The installer stopped unexpectedly.".into()),
    }
}

/// `FILE_SHARE_READ` (winnt.h): others may read the file, nobody may write, rename or delete it.
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x1;

/// Inno Setup's exit codes in words.
#[cfg(any(windows, test))]
fn inno_failure(code: i32) -> &'static str {
    match code {
        1 => "The installer couldn't start",
        2 | 5 => "The installation was cancelled",
        3 => "The installer failed while preparing",
        4 => "The installation failed",
        6 => "The installer was stopped",
        7 => "The installer couldn't continue: something on this PC is in the way",
        8 => "Windows needs to restart before Familiar can be updated",
        _ => "The installation failed",
    }
}

#[cfg(target_os = "macos")]
fn install(artifact: &Path, bundle: &Path) -> Result<(), String> {
    let (version, entry, sig) = reverify_manifest(artifact, Platform::MacOs)?;
    let name = Platform::MacOs.artifact(&version);
    let file = std::fs::File::open(artifact).map_err(|e| format!("couldn't open the update: {e}"))?;
    verify::verify_artifact(std::io::BufReader::new(file), &sig, &verify::release_keys(), &name, &version, &entry)?;
    mac::swap_in(artifact, bundle)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn install(_artifact: &Path, _relaunch: &Path) -> Result<(), String> {
    Err("Familiar doesn't update itself on this system.".into())
}

/// Start Familiar again: on Windows the installed copy after a good install (the install folder may differ from where
/// the old exe ran), else the program that was running; on macOS the bundle.
fn relaunch(target: &Path, installed: bool) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
        let registered = installed_exe().filter(|p| p.is_file());
        let exe = if installed { registered.clone().unwrap_or_else(|| target.to_owned()) } else { target.to_owned() };
        let exe = if exe.is_file() { exe } else { registered.unwrap_or(exe) };
        let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
        if let Err(e) = std::process::Command::new(&exe).current_dir(dir).creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP).spawn() {
            tracing::error!("updater: couldn't start {}: {e}", exe.display());
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = installed;
        // A new instance of the bundle just swapped in (the old app has exited; `-n` so Launch Services doesn't
        // hand the request to a stale record of it). Needs checking on a real Mac.
        if let Err(e) = std::process::Command::new("/usr/bin/open").arg("-n").arg(target).status() {
            tracing::error!("updater: couldn't open {}: {e}", target.display());
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = (target, installed);
}

/// Where the installer put `familiar-native.exe` (its uninstall entry; the AppId is `familiar.iss`'s).
#[cfg(windows)]
fn installed_exe() -> Option<PathBuf> {
    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{B031A143-7CAF-46CB-BC54-235853633C54}_is1";
    let dir = windows_registry::CURRENT_USER.open(KEY).ok()?.get_string("InstallLocation").ok()?;
    Some(PathBuf::from(dir.trim_end_matches('\\')).join("familiar-native.exe"))
}

/// Wait until process `pid` has exited; false when it is still running after `limit`.
#[cfg(windows)]
fn wait_for_exit(pid: u32, limit: Duration) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            // Gone already (or never ours to wait for).
            return true;
        }
        let r = WaitForSingleObject(handle, limit.as_millis().min(u32::MAX as u128 - 1) as u32);
        CloseHandle(handle);
        r == WAIT_OBJECT_0
    }
}

#[cfg(not(windows))]
fn wait_for_exit(pid: u32, limit: Duration) -> bool {
    let start = std::time::Instant::now();
    let alive = || {
        std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    while alive() {
        if start.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    true
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    /// The `.app` bundle the running binary is in (`…/Familiar.app/Contents/MacOS/<binary>`).
    pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
        let bundle = exe.parent()?.parent()?.parent()?;
        (bundle.extension().is_some_and(|e| e == "app") && exe.parent()?.ends_with("Contents/MacOS")).then(|| bundle.to_owned())
    }

    /// The bundle's folder must take new files (the swap renames inside it): checked before the app quits.
    pub fn check_writable(bundle: &Path) -> Result<(), String> {
        let parent = bundle.parent().ok_or("the app has no folder")?;
        let probe = parent.join(format!(".familiar-write-test-{}", std::process::id()));
        std::fs::write(&probe, b"").map_err(|_| {
            format!("Familiar can't replace itself in {}: move Familiar.app to a folder you can write to (like Applications) and try again.", parent.display())
        })?;
        let _ = std::fs::remove_file(probe);
        Ok(())
    }

    fn run(program: &str, args: &[&OsStr]) -> Result<(), String> {
        let status = std::process::Command::new(program).args(args).status().map_err(|e| format!("couldn't run {program}: {e}"))?;
        if status.success() { Ok(()) } else { Err(format!("{program} failed ({status})")) }
    }

    /// Unpack the (verified) archive beside `bundle` and swap it in: two renames, the old bundle restored if the
    /// second fails.
    pub fn swap_in(archive: &Path, bundle: &Path) -> Result<(), String> {
        let parent = bundle.parent().ok_or("the app has no folder")?;
        let name = bundle.file_name().ok_or("the app has no name")?.to_string_lossy().into_owned();
        let pid = std::process::id();
        let unpack = parent.join(format!(".{name}.new-{pid}"));
        let old = parent.join(format!(".{name}.old-{pid}"));
        let _ = std::fs::remove_dir_all(&unpack);
        std::fs::create_dir(&unpack).map_err(|e| format!("couldn't unpack the update: {e}"))?;
        let cleanup = |r: Result<(), String>| {
            let _ = std::fs::remove_dir_all(&unpack);
            r
        };
        if let Err(e) = run("/usr/bin/tar", &[OsStr::new("-xzf"), archive.as_os_str(), OsStr::new("-C"), unpack.as_os_str()]) {
            return cleanup(Err(format!("couldn't unpack the update: {e}")));
        }
        let fresh = unpack.join("Familiar.app");
        let binary = std::env::current_exe().ok().and_then(|e| e.file_name().map(|n| n.to_owned()));
        let has_binary = binary.is_some_and(|b| fresh.join("Contents/MacOS").join(b).is_file());
        if !has_binary || !fresh.join("Contents/Info.plist").is_file() {
            return cleanup(Err("the update doesn't contain Familiar.app".into()));
        }
        if let Err(e) = std::fs::rename(bundle, &old) {
            return cleanup(Err(format!("couldn't move the current app aside: {e}")));
        }
        if let Err(e) = std::fs::rename(&fresh, bundle) {
            let _ = std::fs::rename(&old, bundle);
            return cleanup(Err(format!("couldn't put the new app in place: {e}")));
        }
        let _ = std::fs::remove_dir_all(&old);
        cleanup(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn helper_arguments_parse_strictly() {
        // A path of this system: on macOS a Windows path is one file name, so `ends_with` below wouldn't match.
        let (artifact, exe) = if cfg!(windows) {
            (r"C:\u\Familiar-1.0.0-setup.exe", r"C:\p\familiar-native.exe")
        } else {
            ("/u/Familiar-1.0.0-setup.exe", "/p/familiar-native")
        };
        let a = parse(&os(&["familiar", FLAG, artifact, "--wait", "42", "--relaunch", exe])).unwrap();
        assert_eq!(a.pid, 42);
        assert!(a.artifact.ends_with("Familiar-1.0.0-setup.exe"));
        assert!(parse(&os(&["familiar", "--hidden"])).is_none());
        assert!(parse(&os(&["familiar", FLAG, "x"])).is_none(), "needs --wait and --relaunch");
        assert!(parse(&os(&["familiar", FLAG, "x", "--wait", "nope", "--relaunch", "y"])).is_none());
        assert!(parse(&os(&["familiar", FLAG, "x", "--wait", "1", "--relaunch", "y", "--evil", "z"])).is_none());
    }

    #[test]
    fn outcomes_round_trip_once() {
        let dir = tempfile::tempdir().unwrap();
        record(dir.path(), "1.2.3", false, "The installation failed");
        let o = take_outcome(dir.path()).unwrap();
        assert_eq!(o, Outcome { version: "1.2.3".into(), ok: false, message: "The installation failed".into() });
        assert!(take_outcome(dir.path()).is_none(), "read once");
    }

    #[test]
    fn outcomes_are_read_like_outside_data() {
        let dir = tempfile::tempdir().unwrap();
        let write = |json: &serde_json::Value| std::fs::write(dir.path().join(RESULT), serde_json::to_vec(json).unwrap()).unwrap();
        // Hidden characters go, long messages are cut.
        write(&serde_json::json!({"version": "1.2.3", "ok": false, "message": format!("bad\u{202E}gnp.exe {}", "x".repeat(5000))}));
        let o = take_outcome(dir.path()).unwrap();
        assert!(o.message.starts_with("badgnp.exe x"));
        assert_eq!(o.message.chars().count(), MAX_MESSAGE + 1);
        // Not a release version.
        write(&serde_json::json!({"version": "../../evil", "ok": true, "message": "Updated"}));
        assert!(take_outcome(dir.path()).is_none());
        // Too big to be ours (and removed all the same).
        std::fs::write(dir.path().join(RESULT), vec![b' '; MAX_RESULT as usize + 1]).unwrap();
        assert!(take_outcome(dir.path()).is_none());
        assert!(!dir.path().join(RESULT).exists());
    }

    #[test]
    fn the_helper_refuses_an_unsigned_or_older_staged_update() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("9.9.9");
        std::fs::create_dir_all(&stage).unwrap();
        let exe = stage.join("Familiar-9.9.9-setup.exe");
        // Signed by the test key, not the release key compiled in.
        for f in ["familiar-update.json", "familiar-update.json.minisig", "Familiar-9.9.9-setup.exe", "Familiar-9.9.9-setup.exe.minisig"] {
            std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/updates/fixtures/good").join(f), stage.join(f)).unwrap();
        }
        assert!(reverify_manifest(&exe, Platform::Windows).unwrap_err().contains("isn't signed"));
        // Missing manifest.
        std::fs::remove_file(stage.join("familiar-update.json")).unwrap();
        assert!(reverify_manifest(&exe, Platform::Windows).is_err());
    }

    #[test]
    fn inno_exit_codes_read_as_words() {
        assert_eq!(inno_failure(8), "Windows needs to restart before Familiar can be updated");
        assert_eq!(inno_failure(99), "The installation failed");
    }

    #[cfg(windows)]
    #[test]
    fn a_locked_installer_still_runs_but_cant_be_swapped() {
        use std::os::windows::fs::OpenOptionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("probe.exe");
        std::fs::copy(r"C:\Windows\System32\whoami.exe", &exe).unwrap();
        let lock = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&exe).unwrap();
        assert!(std::fs::OpenOptions::new().write(true).open(&exe).is_err(), "no writes while held");
        assert!(std::fs::rename(&exe, dir.path().join("other.exe")).is_err(), "no renames while held");
        assert!(std::fs::remove_file(&exe).is_err(), "no deletes while held");
        let out = std::process::Command::new(&exe).output().expect("runs while held");
        assert!(out.status.success());
        drop(lock);
        std::fs::remove_file(&exe).unwrap();
    }
}
