//! Familiar desktop: a Tauri 2 tray app that is the whole local install. familiar-host runs the built-in database (or
//! uses a configured Postgres), serves the Familiar API on 127.0.0.1 and hosts familiar-core, the daemon; this file is
//! only the shell around it: commands, tray, notifications, window. The UI (apps/web) talks to the API; the commands
//! here expose only local things (startup progress, Claude login, folders).

use familiar_host::{AppStatus, CliStatus, DaemonStatus, Host, Signal};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::broadcast;

#[tauri::command]
fn daemon_status(host: tauri::State<'_, Host>) -> DaemonStatus {
    host.daemon_status()
}

/// Everything the first-run screen needs, in one call.
#[tauri::command]
fn app_status(host: tauri::State<'_, Host>) -> AppStatus {
    host.app_status()
}

/// Is Claude Code installed and signed in? Familiar runs entirely on that login.
#[tauri::command]
async fn claude_status() -> CliStatus {
    familiar_host::claude_status().await
}

/// Is the OpenAI Codex CLI installed and signed in? Bots on the `codex` engine run on that login.
#[tauri::command]
async fn codex_status() -> CliStatus {
    familiar_host::codex_status().await
}

#[tauri::command]
fn open_cli_terminal(action: String) -> Result<(), String> {
    familiar_host::open_cli_terminal(&action)
}

#[tauri::command]
fn open_bots_folder(host: tauri::State<'_, Host>) -> Result<(), String> {
    familiar_host::open_folder(&host.bots_dir())
}

/// A session token for the owner without a password (null until an owner exists); not used by the web UI yet.
#[tauri::command]
async fn local_token(host: tauri::State<'_, Host>) -> Result<Option<String>, ()> {
    Ok(host.local_token().await)
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn window_focused(app: &AppHandle) -> bool {
    app.get_webview_window("main").is_some_and(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false))
}

fn notify(app: &AppHandle, title: &str, body: String) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        tracing::warn!("notification failed: {e}");
    }
}

/// Notify unless you are looking at the window (so a run you just started here stays quiet).
fn forward_signals(app: AppHandle, mut rx: broadcast::Receiver<Signal>) {
    tauri::async_runtime::spawn(async move {
        loop {
            let signal = match rx.recv().await {
                Ok(s) => s,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            };
            if window_focused(&app) {
                continue;
            }
            match signal {
                Signal::ApprovalPending { bot, tool } => notify(&app, "Approval needed", format!("{bot} wants to use {tool}")),
                Signal::RunFinished { bot, status } => notify(&app, "Run finished", format!("{bot}: {status}")),
                Signal::Notify { bot, message, .. } => notify(&app, &bot, message),
            }
        }
    });
}

fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Familiar", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "Pause daemon", true, None::<&str>)?;
    let folder = MenuItem::with_id(app, "folder", "Open bots folder", true, None::<&str>)?;
    let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);
    let autostart = CheckMenuItem::with_id(app, "autostart", "Start at login", true, autostart_on, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &pause, &folder, &autostart, &sep, &quit])?;

    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("familiar")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| {
            let host = app.state::<Host>();
            match event.id().as_ref() {
                "open" => show_main(app),
                "pause" => {
                    if host.daemon_paused() {
                        host.resume_daemon();
                    } else {
                        host.pause_daemon();
                    }
                    let _ = pause.set_text(if host.daemon_paused() { "Resume daemon" } else { "Pause daemon" });
                }
                "folder" => {
                    let _ = familiar_host::open_folder(&host.bots_dir());
                }
                "autostart" => {
                    let manager = app.autolaunch();
                    let res = if manager.is_enabled().unwrap_or(false) { manager.disable() } else { manager.enable() };
                    if let Err(e) = res {
                        tracing::warn!("autostart toggle failed: {e}");
                    }
                    let _ = autostart.set_checked(manager.is_enabled().unwrap_or(false));
                }
                "quit" => app.exit(0),
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

pub fn run() {
    // The release app has no console: log to ~/.familiar/logs/desktop.log.
    familiar_host::init_logging("desktop");

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--hidden"])))
        .invoke_handler(tauri::generate_handler![
            daemon_status,
            app_status,
            claude_status,
            codex_status,
            open_cli_terminal,
            open_bots_folder,
            local_token
        ])
        .setup(|app| {
            let host = Host::start(tauri::async_runtime::handle().inner().clone());
            forward_signals(app.handle().clone(), host.signals());
            app.manage(host);
            setup_tray(app.handle())?;
            if std::env::args().any(|a| a == "--hidden") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Always-on: closing the window hides it; quit lives in the tray menu.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error building Familiar desktop app");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            // Drain in-flight runs, stop the built-in database cleanly.
            let host = handle.state::<Host>().inner().clone();
            tauri::async_runtime::block_on(host.shutdown());
        }
    });
}
