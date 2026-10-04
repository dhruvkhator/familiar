//! The tray icon (GPUI has none: `tray-icon` + its `muda` menus, on the GPUI main thread whose message loop drives
//! them). Open Familiar, Pause / Resume work (host mode), Start at login, Quit; a left click opens the window.

use futures::StreamExt as _;
use gpui::{App, Global};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::desktop;
use crate::engine::HOSTED;

struct Tray {
    _icon: TrayIcon,
    pause: MenuItem,
    autostart: CheckMenuItem,
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
    let menu = Menu::with_items(&[&open, &PredefinedMenuItem::separator(), &pause, &autostart, &PredefinedMenuItem::separator(), &quit])
        .map_err(|e| e.to_string())?;
    // The app icon embedded as resource 1 (build.rs).
    #[cfg(windows)]
    let icon = Icon::from_resource(1, Some((32, 32))).map_err(|e| e.to_string())?;
    #[cfg(not(windows))]
    let icon = Icon::from_rgba(vec![0x72, 0x85, 0xd5, 0xff].repeat(16 * 16), 16, 16).map_err(|e| e.to_string())?;
    let icon = TrayIconBuilder::new()
        .with_tooltip("Familiar")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(Tray { _icon: icon, pause, autostart })
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
