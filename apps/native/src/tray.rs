//! The tray icon (GPUI has none: `tray-icon` + its `muda` menus, on the GPUI main thread whose message loop drives
//! them). Open Familiar, Pause / Resume work (host mode), Start at login, Quit; a left click opens the window. While a
//! teammate is using this PC's desktop the icon turns red, its tooltip says who, and "Stop desktop control" takes the
//! desktop away at once (and denies its waiting desktop requests).

use std::sync::Mutex;

use futures::StreamExt as _;
use gpui::{App, Global};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::desktop;
use crate::engine::HOSTED;

struct Tray {
    icon: TrayIcon,
    pause: MenuItem,
    autostart: CheckMenuItem,
    stop_desktop: MenuItem,
}

/// The API client the tray's "Stop desktop control" uses (set once the app has one).
static CLIENT: Mutex<Option<familiar_client::Client>> = Mutex::new(None);

pub fn set_client(client: familiar_client::Client) {
    *CLIENT.lock().unwrap() = Some(client);
}

impl Global for Tray {}

/// Is the tray up (closing the window then hides it instead of quitting)?
pub fn installed(cx: &App) -> bool {
    cx.has_global::<Tray>()
}

pub fn install(cx: &mut App) {
    match build() {
        Ok(tray) => cx.set_global(tray),
        Err(e) => {
            tracing::warn!("tray: {e}");
            return;
        }
    }
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
    let menu_tx = tx.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = menu_tx.unbounded_send(e.id.0);
    }));
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            let _ = tx.unbounded_send("open".into());
        }
    }));
    cx.spawn(async move |cx| {
        while let Some(id) = rx.next().await {
            cx.update(|cx| on_action(&id, cx));
        }
    })
    .detach();
    // Take the icon out of the notification area on the way out (a killed process leaves a ghost until hovered).
    cx.on_app_quit(|cx| {
        cx.remove_global::<Tray>();
        async {}
    })
    .detach();
    sync(cx);
}

fn build() -> Result<Tray, String> {
    let open = MenuItem::with_id("open", "Open Familiar", true, None);
    let pause = MenuItem::with_id("pause", "Pause work", false, None);
    let autostart = CheckMenuItem::with_id("autostart", "Start at login", true, desktop::autostart_enabled(), None);
    let quit = MenuItem::with_id("quit", "Quit Familiar", true, None);
    let stop_desktop = MenuItem::with_id("stop_desktop", "Stop desktop control", false, None);
    let menu = Menu::with_items(&[
        &open,
        &PredefinedMenuItem::separator(),
        &stop_desktop,
        &pause,
        &autostart,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .map_err(|e| e.to_string())?;
    // The app icon embedded as resource 1 (build.rs).
    let icon = app_icon()?;
    let icon = TrayIconBuilder::new()
        .with_tooltip("Familiar")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(Tray { icon, pause, autostart, stop_desktop })
}

/// The app icon embedded as resource 1 (build.rs).
fn app_icon() -> Result<Icon, String> {
    #[cfg(windows)]
    return Icon::from_resource(1, Some((32, 32))).map_err(|e| e.to_string());
    #[cfg(not(windows))]
    return Icon::from_rgba(vec![0x72, 0x85, 0xd5, 0xff].repeat(16 * 16), 16, 16).map_err(|e| e.to_string());
}

/// The icon while a teammate uses the desktop: a red disc with a white dot ("on air"), unmistakable in the tray.
fn busy_icon() -> Result<Icon, String> {
    const N: usize = 32;
    let mut px = vec![0u8; N * N * 4];
    let c = (N as f32 - 1.0) / 2.0;
    for y in 0..N {
        for x in 0..N {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let rgba = if d <= 5.5 {
                [0xff, 0xff, 0xff, 0xff]
            } else if d <= 15.0 {
                [0xe5, 0x48, 0x4d, 0xff]
            } else {
                [0, 0, 0, 0]
            };
            px[(y * N + x) * 4..][..4].copy_from_slice(&rgba);
        }
    }
    Icon::from_rgba(px, N as u32, N as u32).map_err(|e| e.to_string())
}

/// A teammate started using the desktop (`Some(name)`) or nobody uses it any more: icon, tooltip, the Stop item, and
/// a notification that shows even while Familiar is in front.
pub fn desktop_changed(bot: Option<String>, cx: &mut App) {
    if let Some(tray) = cx.try_global::<Tray>() {
        let (icon, tip) = match &bot {
            Some(name) => (busy_icon(), format!("Familiar: {name} is using your desktop")),
            None => (app_icon(), "Familiar".to_owned()),
        };
        if let Ok(icon) = icon {
            let _ = tray.icon.set_icon(Some(icon));
        }
        let _ = tray.icon.set_tooltip(Some(tip));
        tray.stop_desktop.set_enabled(bot.is_some());
        tray.stop_desktop.set_text(match &bot {
            Some(name) => format!("Stop desktop control ({name})"),
            None => "Stop desktop control".to_owned(),
        });
    }
    let (title, body) = match &bot {
        Some(name) => (
            format!("{name} is using your desktop"),
            "Every step asks you first. Stop it any time from the tray: Stop desktop control.".to_owned(),
        ),
        None => ("Desktop control ended".to_owned(), "No teammate is using your desktop now.".to_owned()),
    };
    cx.show_system_notification(gpui::SystemNotification {
        tag: "desktop|control".into(),
        title: title.into(),
        body: body.into(),
        actions: Vec::new(),
    });
}

/// "Stop desktop control": deny the waiting desktop requests and take the desktop from the teammate using it.
fn stop_desktop(cx: &mut App) {
    let Some(client) = CLIENT.lock().unwrap().clone() else { return };
    if let Some(tray) = cx.try_global::<Tray>() {
        tray.stop_desktop.set_enabled(false);
        tray.stop_desktop.set_text("Stopping desktop control…");
    }
    gpui_tokio::Tokio::spawn(cx, async move {
        if let Err(e) = client.stop_desktop().await {
            tracing::warn!("stop desktop control: {}", e.message());
        }
    })
    .detach();
}

fn on_action(id: &str, cx: &mut App) {
    match id {
        "open" => desktop::show_main(cx),
        "pause" => {
            if let Some(host) = HOSTED.lock().unwrap().clone() {
                if host.daemon_paused() {
                    host.resume_daemon();
                } else {
                    host.pause_daemon();
                }
            }
            sync(cx);
        }
        "autostart" => {
            let on = !desktop::autostart_enabled();
            if let Err(e) = desktop::set_autostart(on) {
                tracing::warn!("start at login: {e}");
            }
            sync(cx);
        }
        "stop_desktop" => stop_desktop(cx),
        "quit" => desktop::quit_app(cx),
        _ => {}
    }
}

/// Bring the menu in line with the engine (pause only exists in host mode) and the Run key.
pub fn sync(cx: &mut App) {
    let Some(tray) = cx.try_global::<Tray>() else { return };
    let host = HOSTED.lock().unwrap().clone();
    tray.pause.set_enabled(host.is_some());
    tray.pause.set_text(if host.is_some_and(|h| h.daemon_paused()) { "Resume work" } else { "Pause work" });
    tray.autostart.set_checked(desktop::autostart_enabled());
}
