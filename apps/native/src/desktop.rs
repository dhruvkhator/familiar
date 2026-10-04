//! Desktop integration GPUI doesn't cover on Windows: start at login (the HKCU `Run` key).

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
