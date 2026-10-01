//! Familiar desktop: a Tauri 2 tray app that is the whole local install. It runs the built-in database (or uses a
//! configured Postgres), serves the Familiar API on 127.0.0.1 and hosts familiar-core, the daemon. The UI (apps/web) talks to
//! that API; the commands here expose only local things (startup progress, Claude login, folders).

mod embedded_db;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use familiar_core::{Config, Signal};

type Task = tauri::async_runtime::JoinHandle<()>;

#[derive(Default)]
struct Daemon {
    token: Option<CancellationToken>,
    task: Option<Task>,
    paused: bool,
    error: Option<String>,
    bots_dir: Option<PathBuf>,
}

/// Startup progress, shown by the UI's first-run screen.
#[derive(Default, Clone, Serialize)]
struct Boot {
    /// starting | database | ready | error
    phase: String,
    message: String,
    database_url: Option<String>,
    embedded: bool,
}

struct AppState {
    daemon: Arc<Mutex<Daemon>>,
    signals: broadcast::Sender<Signal>,
    boot: Arc<Mutex<Boot>>,
    pg: Arc<tokio::sync::Mutex<Option<postgresql_embedded::PostgreSQL>>>,
}

#[derive(Serialize)]
struct DaemonStatus {
    running: bool,
    error: Option<String>,
    config_path: String,
}

fn config_path() -> PathBuf {
    Config::path()
}

/// Bring up the database, then the API and the daemon. Runs once, in the background.
fn boot(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let set = |phase: &str, message: &str| {
            let mut b = state.boot.lock().unwrap();
            b.phase = phase.into();
            b.message = message.into();
        };
        if let Err(e) = Config::create_default() {
            return set("error", &format!("could not create {}: {e:#}", config_path().display()));
        }
        let cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => return set("error", &format!("{e:#}")),
        };
        let url = match cfg.database_url.clone() {
            Some(url) => url,
            None => {
                set("database", "Setting up the built-in database (first launch downloads it once)…");
                match embedded_db::start(&cfg).await {
                    Ok((pg, url)) => {
                        *state.pg.lock().await = Some(pg);
                        state.boot.lock().unwrap().embedded = true;
                        url
                    }
                    Err(e) => return set("error", &format!("{e:#}")),
                }
            }
        };
        state.boot.lock().unwrap().database_url = Some(url.clone());
        start_api(&cfg, &url);
        start_daemon(&state);
        set("ready", "");
    });
}

/// The Familiar API on 127.0.0.1 (`local_api_port`, default 47080). It keeps serving while the daemon is paused.
fn start_api(cfg: &Config, url: &str) {
    let port = cfg.local_api_port.unwrap_or(47080);
    let api = familiar_server::Config {
        database_url: url.to_owned(),
        host: [127, 0, 0, 1],
        port,
        secret_key: cfg.secret_key.clone(),
        public_url: Some(format!("http://localhost:{port}")),
        web_origins: vec![],
    };
    tauri::async_runtime::spawn(async move {
        if let Err(e) = familiar_server::serve(api, std::future::pending()).await {
            tracing::error!("local API stopped: {e:#}");
        }
    });
}

/// (Re)start the core. A config or connection error is recorded, never fatal.
fn start_daemon(state: &AppState) {
    let mut d = state.daemon.lock().unwrap();
    d.paused = false;
    let mut cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            d.error = Some(format!("{e:#}"));
            return;
        }
    };
    let Some(url) = state.boot.lock().unwrap().database_url.clone() else {
        d.error = Some("the database is not ready yet".into());
        return;
    };
    cfg.database_url = Some(url);
    d.error = None;
    d.bots_dir = Some(cfg.bots_dir());
    let token = CancellationToken::new();
    d.token = Some(token.clone());
    let previous = d.task.take(); // let a cancelled core finish draining before the new one starts
    let (daemon, signals) = (state.daemon.clone(), state.signals.clone());
    d.task = Some(tauri::async_runtime::spawn(async move {
        if let Some(prev) = previous {
            let _ = prev.await;
        }
        if token.is_cancelled() {
            return;
        }
        if let Err(e) = familiar_core::run_with_signals(cfg, token.clone(), signals).await {
            tracing::error!("familiar-core stopped: {e:#}");
            let mut d = daemon.lock().unwrap();
            if !token.is_cancelled() {
                d.error = Some(format!("{e:#}"));
                d.token = None;
            }
        }
    }));
}

fn stop_daemon(state: &AppState) {
    let mut d = state.daemon.lock().unwrap();
    d.paused = true;
    if let Some(t) = d.token.take() {
        t.cancel();
    }
}

#[tauri::command]
fn daemon_status(state: tauri::State<'_, AppState>) -> DaemonStatus {
    let d = state.daemon.lock().unwrap();
    DaemonStatus {
        running: d.token.is_some() && !d.paused && d.error.is_none(),
        error: d.error.clone(),
        config_path: config_path().display().to_string(),
    }
}

#[derive(Serialize)]
struct AppStatus {
    boot: Boot,
    daemon: DaemonStatus,
}

/// Everything the first-run screen needs, in one call.
#[tauri::command]
fn app_status(state: tauri::State<'_, AppState>) -> AppStatus {
    let boot = state.boot.lock().unwrap().clone();
    AppStatus { boot, daemon: daemon_status(state) }
}

#[derive(Serialize)]
struct ClaudeStatus {
    installed: bool,
    version: Option<String>,
    logged_in: bool,
    auth_method: Option<String>,
    /// Codex only: the installed CLI is too old for ChatGPT sign-in models.
    needs_update: bool,
}

/// Is Claude Code installed and signed in? Familiar runs entirely on that login.
#[tauri::command]
async fn claude_status() -> ClaudeStatus {
    let bin = Config::load().map(|c| c.claude_bin).unwrap_or_else(|_| "claude".into());
    let run = |args: &'static [&'static str]| {
        let bin = bin.clone();
        async move {
            let mut cmd = tokio::process::Command::new(bin);
            cmd.args(args);
            #[cfg(windows)]
            cmd.creation_flags(0x0800_0000);
            tokio::time::timeout(Duration::from_secs(20), cmd.output()).await.ok()?.ok()
        }
    };
    let Some(version) = run(&["--version"]).await.filter(|o| o.status.success()) else {
        return ClaudeStatus { installed: false, version: None, logged_in: false, auth_method: None, needs_update: false };
    };
    let auth: serde_json::Value = run(&["auth", "status"])
        .await
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or_default();
    ClaudeStatus {
        installed: true,
        version: Some(String::from_utf8_lossy(&version.stdout).trim().to_owned()),
        logged_in: auth["loggedIn"] == true,
        auth_method: auth["authMethod"].as_str().map(str::to_owned),
        needs_update: false,
    }
}

/// Is the OpenAI Codex CLI installed and signed in? Bots on the `codex` engine run on that login.
#[tauri::command]
async fn codex_status() -> ClaudeStatus {
    let bin = Config::load().map(|c| c.codex_bin).unwrap_or_else(|_| "codex".into());
    let bin = familiar_core::codex::resolve_bin(&bin);
    let run = |args: &'static [&'static str]| {
        let bin = bin.clone();
        async move {
            let mut cmd = tokio::process::Command::new(bin);
            cmd.args(args);
            #[cfg(windows)]
            cmd.creation_flags(0x0800_0000);
            tokio::time::timeout(Duration::from_secs(20), cmd.output()).await.ok()?.ok()
        }
    };
    let Some(version) = run(&["--version"]).await.filter(|o| o.status.success()) else {
        return ClaudeStatus { installed: false, version: None, logged_in: false, auth_method: None, needs_update: false };
    };
    // `codex login status` prints "Logged in using ChatGPT" / "... an API key" (on stderr) and exits 0 when signed in.
    let status = run(&["login", "status"]).await;
    let text = status
        .as_ref()
        .map(|o| format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
        .unwrap_or_default();
    let logged_in = status.is_some_and(|o| o.status.success()) && text.contains("Logged in");
    let auth_method = logged_in.then(|| if text.contains("ChatGPT") { "chatgpt" } else { "api_key" }.to_owned());
    let version = String::from_utf8_lossy(&version.stdout).trim().to_owned();
    ClaudeStatus { needs_update: codex_too_old(&version), installed: true, version: Some(version), logged_in, auth_method }
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
    match nums.as_slice() {
        [0, minor, ..] => *minor < 159,
        [] => false,
        _ => false,
    }
}

/// Open a visible terminal for a sign-in / install / update the owner asked for. Only these fixed commands; the
/// CLIs then finish sign-in in the browser on their own.
#[tauri::command]
fn open_cli_terminal(action: String) -> Result<(), String> {
    let (title, command) = match action.as_str() {
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

fn bots_folder(state: &AppState) -> PathBuf {
    let dir = state.daemon.lock().unwrap().bots_dir.clone().unwrap_or_else(|| Config::home_dir().join("bots"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn open_folder(dir: PathBuf) -> Result<(), String> {
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

#[tauri::command]
fn open_bots_folder(state: tauri::State<'_, AppState>) -> Result<(), String> {
    open_folder(bots_folder(&state))
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
            let state = app.state::<AppState>();
            match event.id().as_ref() {
                "open" => show_main(app),
                "pause" => {
                    let paused = state.daemon.lock().unwrap().paused;
                    if paused {
                        start_daemon(&state);
                    } else {
                        stop_daemon(&state);
                    }
                    let paused = state.daemon.lock().unwrap().paused;
                    let _ = pause.set_text(if paused { "Resume daemon" } else { "Pause daemon" });
                }
                "folder" => {
                    let _ = open_folder(bots_folder(&state));
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
    // The release app has no console: log to ~/.familiar/logs/desktop.log (truncated past 10 MB).
    let logs = Config::home_dir().join("logs");
    let _ = std::fs::create_dir_all(&logs);
    let log_path = logs.join("desktop.log");
    if std::fs::metadata(&log_path).is_ok_and(|m| m.len() > 10 * 1024 * 1024) {
        let _ = std::fs::remove_file(&log_path);
    }
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    match std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(file) => tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(Mutex::new(file)).init(),
        Err(_) => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--hidden"])))
        .invoke_handler(tauri::generate_handler![daemon_status, app_status, claude_status, codex_status, open_cli_terminal, open_bots_folder])
        .setup(|app| {
            let state = AppState {
                daemon: Arc::default(),
                signals: broadcast::channel(64).0,
                boot: Arc::new(Mutex::new(Boot { phase: "starting".into(), ..Default::default() })),
                pg: Arc::default(),
            };
            forward_signals(app.handle().clone(), state.signals.subscribe());
            app.manage(state);
            boot(app.handle().clone());
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
            let state = handle.state::<AppState>();
            let (token, task) = {
                let mut d = state.daemon.lock().unwrap();
                (d.token.take(), d.task.take())
            };
            if let Some(t) = token {
                t.cancel();
            }
            if let Some(task) = task {
                // Give in-flight runs a moment to record their status.
                let _ = tauri::async_runtime::block_on(tokio::time::timeout(Duration::from_secs(35), task));
            }
            // Shut the built-in database down cleanly.
            let pg = state.pg.clone();
            tauri::async_runtime::block_on(async move {
                if let Some(pg) = pg.lock().await.take() {
                    let _ = tokio::time::timeout(Duration::from_secs(10), pg.stop()).await;
                }
            });
        }
    });
}
