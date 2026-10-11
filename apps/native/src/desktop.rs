//! Desktop integration GPUI doesn't cover: start at login, hiding / showing the main window, and one running copy per
//! user (a second launch nudges the first to show its window, or `--quit` asks it to quit, and exits).
//!
//! - Windows: the HKCU `Run` key; `ShowWindow` (gpui's `Platform::hide` is a no-op there); a named mutex, and named
//!   events for the nudges.
//! - macOS: a LaunchAgent; closing the window hides the app (the Dock icon or the menu bar item brings it back); a lock
//!   file in `~/.familiar/run` and a Unix socket next to it for the nudges. Plus what a Mac app is expected to have
//!   ([`install`]): the app menu (Settings, Hide, Quit with the engine drained) and the Edit menu, so copy and paste
//!   work in every text field, and a drained engine when macOS ends the app (the Dock's Quit, logging out).

use gpui::{App, Global, Window, WindowHandle};

use crate::root::Root;

/// The app's one window, for the tray, notifications and a second launch.
pub struct MainWindow(pub WindowHandle<Root>);

impl Global for MainWindow {}

/// Bring the main window back (from the tray or behind other windows) and focus it.
pub fn show_main(cx: &mut App) {
    let Some(handle) = cx.try_global::<MainWindow>().map(|m| m.0) else { return };
    let _ = handle.update(cx, |_, window, cx| show_window(window, cx));
}

/// Show the main window on a page (`Shell::open`'s targets: `settings`, `needs`, a teammate...).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn open_page(target: &str, cx: &mut App) {
    show_main(cx);
    let Some(handle) = cx.try_global::<MainWindow>().map(|m| m.0) else { return };
    let _ = handle.update(cx, |root, _, cx| {
        if let Some(shell) = root.shell() {
            shell.update(cx, |s, cx| {
                s.open(target, cx);
            });
        }
    });
}

/// Quit for real: Root drains the engine first in host mode.
pub fn quit_app(cx: &mut App) {
    match cx.try_global::<MainWindow>().map(|m| m.0) {
        Some(handle) => {
            let _ = handle.update(cx, |root, window, cx| root.quit(window, cx));
        }
        None => cx.quit(),
    }
}

/// Is the main window on screen and focused?
pub fn main_focused(cx: &mut App) -> bool {
    let Some(handle) = cx.try_global::<MainWindow>().map(|m| m.0) else { return false };
    handle.update(cx, |_, window, _| window.is_window_active() && window_visible(window)).unwrap_or(false)
}

/// The tray's name in words for this system ("the tray" / "the menu bar").
pub const TRAY: &str = if cfg!(target_os = "macos") { "the menu bar" } else { "the tray" };

// ---- window visibility --------------------------------------------------------------------------------------------

#[cfg(windows)]
fn hwnd(window: &Window) -> Option<windows_sys::Win32::Foundation::HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(w) => Some(w.hwnd.get() as _),
        _ => None,
    }
}

/// Hide the window (it keeps running; the tray brings it back).
#[cfg(windows)]
pub fn hide_window(window: &Window, _cx: &mut App) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
    if let Some(h) = hwnd(window) {
        unsafe { ShowWindow(h, SW_HIDE) };
    }
}

#[cfg(windows)]
pub fn show_window(window: &Window, _cx: &mut App) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, SW_RESTORE, SW_SHOW, ShowWindow};
    if let Some(h) = hwnd(window) {
        unsafe {
            ShowWindow(h, if IsIconic(h) != 0 { SW_RESTORE } else { SW_SHOW });
        }
    }
    window.activate_window();
}

#[cfg(windows)]
pub fn window_visible(window: &Window) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible};
    hwnd(window).is_some_and(|h| unsafe { IsWindowVisible(h) != 0 && IsIconic(h) == 0 })
}

/// Hide the app, as a Mac app does when its last window closes but it keeps working: the Dock icon (or the menu bar
/// item) brings it back.
#[cfg(target_os = "macos")]
pub fn hide_window(_window: &Window, cx: &mut App) {
    cx.hide();
}

/// Unhide and activate the app, and bring the window up (also when it was never shown: `--hidden`, or minimized).
#[cfg(target_os = "macos")]
pub fn show_window(window: &Window, cx: &mut App) {
    cx.activate(true);
    window.activate_window();
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn hide_window(window: &Window, _cx: &mut App) {
    window.minimize_window();
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn show_window(window: &Window, _cx: &mut App) {
    window.activate_window();
}

/// A hidden app's window is not active, so focus alone tells.
#[cfg(not(windows))]
pub fn window_visible(_window: &Window) -> bool {
    true
}

// ---- single instance ----------------------------------------------------------------------------------------------

/// What a later launch asks of the running copy: show its window, or quit (`--quit`, gracefully: the engine drains).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nudge {
    Show,
    Quit,
}

impl Nudge {
    /// The word a later launch writes to the running copy's socket.
    #[cfg_attr(windows, allow(dead_code))]
    fn word(self) -> &'static [u8] {
        match self {
            Nudge::Show => b"show",
            Nudge::Quit => b"quit",
        }
    }

    #[cfg_attr(windows, allow(dead_code))]
    fn from_word(word: &[u8]) -> Option<Nudge> {
        [Nudge::Show, Nudge::Quit].into_iter().find(|n| n.word() == word)
    }
}

/// The first copy gets a stream of nudges from later launches; a later copy has nudged the first and should exit.
pub enum Instance {
    First(futures::channel::mpsc::UnboundedReceiver<Nudge>),
    Second,
}

#[cfg(windows)]
pub fn single_instance(nudge: Nudge) -> Instance {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
    };
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let events = [(Nudge::Show, wide(r"Local\dev.familiar.native.show")), (Nudge::Quit, wide(r"Local\dev.familiar.native.quit"))];
    let mutex_name = wide(r"Local\dev.familiar.native");
    let (tx, rx) = futures::channel::mpsc::unbounded();
    unsafe {
        // Held (leaked) for the life of the process.
        let mutex = CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr());
        if !mutex.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
            let name = &events.iter().find(|(n, _)| *n == nudge).expect("every nudge has an event").1;
            let event = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
            if !event.is_null() {
                SetEvent(event);
                CloseHandle(event);
            }
            return Instance::Second;
        }
        for (kind, name) in events {
            let event = CreateEventW(std::ptr::null(), 0, 0, name.as_ptr());
            if event.is_null() {
                continue;
            }
            let event = event as usize;
            let tx = tx.clone();
            std::thread::Builder::new()
                .name("familiar-instance".into())
                .spawn(move || {
                    while WaitForSingleObject(event as _, INFINITE) == 0 {
                        if tx.unbounded_send(kind).is_err() {
                            break;
                        }
                    }
                })
                .ok();
        }
    }
    Instance::First(rx)
}

/// One copy per user: an exclusive lock on `~/.familiar/run/native.lock` (the OS drops it when the process ends, so a
/// crash never leaves it stale), and a Unix socket beside it that later launches write `show` or `quit` to. The folder
/// is the owner's alone (0700), so only their own processes can reach the socket. Anything going wrong here never
/// stops the app from starting: it then just runs without nudges.
#[cfg(not(windows))]
pub fn single_instance(nudge: Nudge) -> Instance {
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::time::Duration;

    let alone = || Instance::First(futures::channel::mpsc::unbounded().1);
    let run = crate::engine::familiar_home().join("run");
    if let Err(e) = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&run) {
        eprintln!("familiar: {}: {e}", run.display());
        return alone();
    }
    // An older folder may have been made with looser rights.
    let _ = std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700));
    let socket = run.join("native.sock");
    match familiar_host::InstanceLock::acquire(&run.join("native.lock")) {
        // Held for the life of the process.
        Ok(Some(lock)) => std::mem::forget(lock),
        Ok(None) => {
            // Another copy runs; right after its start it may not be listening yet.
            for _ in 0..30 {
                if let Ok(mut s) = UnixStream::connect(&socket) {
                    let _ = s.write_all(nudge.word());
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            return Instance::Second;
        }
        Err(e) => {
            eprintln!("familiar: one-copy lock: {e}");
            return alone();
        }
    }
    // Holding the lock means no other copy listens: a socket file left by a crash goes.
    let _ = std::fs::remove_file(&socket);
    let listener = match UnixListener::bind(&socket) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("familiar: {}: {e}", socket.display());
            return alone();
        }
    };
    let (tx, rx) = futures::channel::mpsc::unbounded();
    std::thread::Builder::new()
        .name("familiar-instance".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                // A word of at most 8 bytes, within a second; anything else is ignored.
                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                let mut word = Vec::with_capacity(8);
                let _ = (&mut stream).take(8).read_to_end(&mut word);
                if let Some(n) = Nudge::from_word(&word) {
                    if tx.unbounded_send(n).is_err() {
                        break;
                    }
                }
            }
        })
        .ok();
    Instance::First(rx)
}

// ---- start at login -----------------------------------------------------------------------------------------------

/// The `Run` value name. The Tauri app registers its own entry; this one starts the native app.
#[cfg(windows)]
const RUN_VALUE: &str = "Familiar Native";
#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Is this app set to start at login?
#[cfg(windows)]
pub fn autostart_enabled() -> bool {
    windows_registry::CURRENT_USER.open(RUN_KEY).and_then(|k| k.get_string(RUN_VALUE)).is_ok()
}

/// Start (hidden, in the tray) at login, or not.
#[cfg(windows)]
pub fn set_autostart(on: bool) -> Result<(), String> {
    let key = windows_registry::CURRENT_USER.create(RUN_KEY).map_err(|e| e.to_string())?;
    if on {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        key.set_string(RUN_VALUE, format!("\"{}\" --hidden", exe.display())).map_err(|e| e.to_string())
    } else {
        match key.remove_value(RUN_VALUE) {
            Ok(()) => Ok(()),
            Err(_) if !autostart_enabled() => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// `~/Library/LaunchAgents/dev.familiar.desktop.plist`: launchd starts the app (hidden, in the menu bar) at login.
#[cfg(target_os = "macos")]
fn launch_agent() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).filter(|h| h.is_absolute())?;
    Some(home.join("Library").join("LaunchAgents").join("dev.familiar.desktop.plist"))
}

/// On only while the LaunchAgent exists and still names an app that is there (not one moved or deleted since).
#[cfg(target_os = "macos")]
pub fn autostart_enabled() -> bool {
    launch_agent()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|plist| agent_program(&plist))
        .is_some_and(|exe| std::path::Path::new(&exe).is_file())
}

#[cfg(target_os = "macos")]
pub fn set_autostart(on: bool) -> Result<(), String> {
    let path = launch_agent().ok_or("no home folder")?;
    if !on {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_str().ok_or("the app's path is not valid text")?;
    if !permanent_place(exe) {
        return Err("Move Familiar to Applications first, then turn this on: right now it runs from the disk image or a                     temporary copy macOS made."
            .into());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, launch_agent_plist(exe)).map_err(|e| e.to_string())
}

/// Is the app somewhere it will still be at the next login? Not on a disk image (`/Volumes/...`) and not in the
/// temporary copy macOS runs a downloaded app from until it is moved (App Translocation).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn permanent_place(exe: &str) -> bool {
    !exe.starts_with("/Volumes/") && !exe.contains("/AppTranslocation/")
}

/// The program a LaunchAgent written by [`launch_agent_plist`] runs (the first `ProgramArguments` string).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn agent_program(plist: &str) -> Option<String> {
    let rest = &plist[plist.find("<key>ProgramArguments</key>")?..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = start + rest[start..].find("</string>")?;
    let s = &rest[start..end];
    Some(s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&"))
}

/// The LaunchAgent for `exe`: run it with `--hidden` at login.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launch_agent_plist(exe: &str) -> String {
    let exe = exe.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>dev.familiar.desktop</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>--hidden</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#
    )
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn autostart_enabled() -> bool {
    false
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn set_autostart(_on: bool) -> Result<(), String> {
    Err("Start at login is only available on Windows and macOS for now.".into())
}

// ---- the Mac app's menus and quitting -------------------------------------------------------------------------------

/// Set up what a Mac app needs beyond its window (call once the main window is open). Elsewhere nothing.
pub fn install(cx: &mut App) {
    #[cfg(target_os = "macos")]
    mac::install(cx);
    #[cfg(not(target_os = "macos"))]
    let _ = cx;
}

#[cfg(target_os = "macos")]
mod mac {
    use std::time::Duration;

    use gpui::{App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, actions};
    use gpui_base::input;

    actions!(familiar, [Quit, Hide, HideOthers, ShowAll, Minimize, CloseWindow, OpenSettings]);

    pub fn install(cx: &mut App) {
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("alt-cmd-h", HideOthers, None),
            KeyBinding::new("cmd-m", Minimize, None),
            KeyBinding::new("cmd-w", CloseWindow, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
        ]);
        cx.on_action(|_: &Quit, cx| super::quit_app(cx));
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        cx.on_action(|_: &OpenSettings, cx| super::open_page("settings", cx));
        cx.on_action(|_: &Minimize, cx| {
            if let Some(w) = cx.active_window() {
                let _ = w.update(cx, |_, window, _| window.minimize_window());
            }
        });
        // Like the window's close button: with the menu bar item up the app hides and keeps working, else it quits.
        cx.on_action(|_: &CloseWindow, cx| {
            if crate::tray::installed(cx) {
                cx.hide();
            } else {
                super::quit_app(cx);
            }
        });
        cx.set_menus([
            Menu::new("Familiar").items([
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide Familiar", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit Familiar", Quit),
            ]),
            // The text fields' own actions (gpui-base binds ⌘Z ⌘X ⌘C ⌘V ⌘A to them).
            Menu::new("Edit").items([
                MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
                MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
                MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
                MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
            ]),
            Menu::new("Window").items([MenuItem::action("Minimize", Minimize), MenuItem::action("Close", CloseWindow)]),
        ]);
        drain_on_terminate(cx);
    }

    /// When macOS ends the app itself (the Dock's Quit, logging out, a restart), AppKit exits right after
    /// `applicationWillTerminate:`: `run` never returns to `main`, which drains the engine on other systems. Stop it
    /// here, bounded: runs get a few seconds, then the API and the built-in database are always stopped (an orphaned
    /// Postgres would block the next start). The app's own Quit drains fully before quitting; if that drain is still
    /// going, this cuts it short the same way.
    fn drain_on_terminate(cx: &mut App) {
        const RUN_GRACE: Duration = Duration::from_secs(6);
        let rt = gpui_tokio::Tokio::handle(cx);
        cx.on_app_quit(move |_| {
            if let Some(host) = crate::engine::HOSTED.lock().unwrap().take() {
                let (tx, rx) = std::sync::mpsc::channel();
                let rt = rt.clone();
                let drained = std::thread::Builder::new().name("familiar-drain".into()).spawn(move || {
                    rt.block_on(host.shutdown_within(RUN_GRACE));
                    let _ = tx.send(());
                });
                // shutdown_within takes at most RUN_GRACE + ~12 s (pool 2 s, database 10 s).
                if drained.is_ok() && rx.recv_timeout(RUN_GRACE + Duration::from_secs(14)).is_err() {
                    tracing::warn!("quit: the engine did not stop in time");
                }
            }
            async {}
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nudge_words_round_trip() {
        for n in [Nudge::Show, Nudge::Quit] {
            assert_eq!(Nudge::from_word(n.word()), Some(n));
        }
        assert_eq!(Nudge::from_word(b"shows"), None);
        assert_eq!(Nudge::from_word(b""), None);
    }

    #[test]
    fn launch_agent_escapes_the_path() {
        let plist = launch_agent_plist("/Applications/A & B <x>.app/Contents/MacOS/familiar-native");
        assert!(plist.contains("<string>/Applications/A &amp; B &lt;x&gt;.app/Contents/MacOS/familiar-native</string>"), "{plist}");
        assert!(plist.contains("<string>--hidden</string>"));
        assert_eq!(agent_program(&plist).as_deref(), Some("/Applications/A & B <x>.app/Contents/MacOS/familiar-native"));
        assert_eq!(agent_program("<plist></plist>"), None);
    }

    #[test]
    fn start_at_login_wants_a_permanent_place() {
        assert!(permanent_place("/Applications/Familiar.app/Contents/MacOS/familiar-native"));
        assert!(permanent_place("/Users/me/Applications/Familiar.app/Contents/MacOS/familiar-native"));
        assert!(!permanent_place("/Volumes/Familiar 1.0/Familiar.app/Contents/MacOS/familiar-native"));
        assert!(!permanent_place(
            "/private/var/folders/x/T/AppTranslocation/0A1B/d/Familiar.app/Contents/MacOS/familiar-native"
        ));
    }
}
