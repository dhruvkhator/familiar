//! Desktop integration GPUI doesn't cover on Windows: start at login (the HKCU `Run` key), hiding / showing the main
//! window (gpui's `Platform::hide` is a no-op there), and one running copy per user (a named mutex; a second launch
//! signals a named event and exits, and the first shows its window).

use gpui::{App, Global, Window, WindowHandle};

use crate::root::Root;

/// The app's one window, for the tray, notifications and a second launch.
pub struct MainWindow(pub WindowHandle<Root>);

impl Global for MainWindow {}

/// Bring the main window back (from the tray or behind other windows) and focus it.
pub fn show_main(cx: &mut App) {
    let Some(handle) = cx.try_global::<MainWindow>().map(|m| m.0) else { return };
    let _ = handle.update(cx, |_, window, _| show_window(window));
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
pub fn hide_window(window: &Window) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
    if let Some(h) = hwnd(window) {
        unsafe { ShowWindow(h, SW_HIDE) };
    }
}

#[cfg(windows)]
pub fn show_window(window: &Window) {
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

#[cfg(not(windows))]
pub fn hide_window(window: &Window) {
    window.minimize_window();
}

#[cfg(not(windows))]
pub fn show_window(window: &Window) {
    window.activate_window();
}

#[cfg(not(windows))]
pub fn window_visible(_window: &Window) -> bool {
    true
}

// ---- single instance ----------------------------------------------------------------------------------------------

/// The first copy gets a stream of "show yourself" nudges from later launches; a later copy has nudged the first and
/// should exit.
pub enum Instance {
    First(futures::channel::mpsc::UnboundedReceiver<()>),
    Second,
}

#[cfg(windows)]
pub fn single_instance() -> Instance {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
    };
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (mutex_name, event_name) = (wide(r"Local\dev.familiar.native"), wide(r"Local\dev.familiar.native.show"));
    let (tx, rx) = futures::channel::mpsc::unbounded();
    unsafe {
        // Held (leaked) for the life of the process.
        let mutex = CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr());
        if !mutex.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
            let event = OpenEventW(EVENT_MODIFY_STATE, 0, event_name.as_ptr());
            if !event.is_null() {
                SetEvent(event);
                CloseHandle(event);
            }
            return Instance::Second;
        }
        let event = CreateEventW(std::ptr::null(), 0, 0, event_name.as_ptr());
        if !event.is_null() {
            let event = event as usize;
            std::thread::Builder::new()
                .name("familiar-instance".into())
                .spawn(move || {
                    while WaitForSingleObject(event as _, INFINITE) == 0 {
                        if tx.unbounded_send(()).is_err() {
                            break;
                        }
                    }
                })
                .ok();
        }
    }
    Instance::First(rx)
}

#[cfg(not(windows))]
pub fn single_instance() -> Instance {
    Instance::First(futures::channel::mpsc::unbounded().1)
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

#[cfg(not(windows))]
pub fn autostart_enabled() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set_autostart(_on: bool) -> Result<(), String> {
    Err("Start at login is only available on Windows for now.".into())
}
