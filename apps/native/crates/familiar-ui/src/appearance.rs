//! Light/dark switching: what the user asked for, what the OS reports, and the plumbing that turns a change in
//! either into a repaint.
//!
//! Vendored from zeron's `crates/ui/src/appearance.rs` (MIT, see `LICENSE-zeron`), with zeron's theme-library,
//! accent and glass selections removed, and a Windows title-bar sync added.
//!
//! 1. [`AppearanceMode`] — the user choice: follow the OS, or pin one.
//! 2. [`AppearanceState`] — a gpui global holding that choice alongside the last appearance the OS reported.
//! 3. [`observe_window`] — subscribes to the platform's appearance notification (Windows: `WM_SETTINGCHANGE`
//!    "ImmersiveColorSet", handled in gpui_windows) and re-applies.
//!
//! # Why `refresh_windows` and not `notify`
//!
//! Colors are read *imperatively* (`Theme::of(cx).ink`) at paint time, not through a reactive binding, so no view
//! knows its colors went stale — a `notify()` on some entity would repaint that entity and nothing else.
//! [`App::refresh_windows`] marks every window dirty *and* disables gpui's per-view prepaint cache for the frame,
//! which is the only thing that forces already-laid-out elements to re-run their paint with the new palette.

use std::time::Duration;

use gpui::{App, AppContext as _, Global, Subscription, Window};
use serde::{Deserialize, Serialize};

use crate::theme::{Appearance, Theme};

/// The user's appearance preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppearanceMode {
    /// Follow the OS. The default — matches every other app on the machine.
    #[default]
    System,
    Light,
    Dark,
}

impl AppearanceMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// Shared glyph for appearance controls throughout the app.
    pub fn icon(self) -> &'static str {
        match self {
            Self::System => crate::icons::MONITOR,
            Self::Light => crate::icons::SUN,
            Self::Dark => crate::icons::MOON,
        }
    }

    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];
}

/// What the user chose, and what the OS last said. Kept separate from [`Theme`] so flipping the OS appearance while
/// the user has pinned Light still records the new system value (and takes effect when they return to `System`).
pub struct AppearanceState {
    pub mode: AppearanceMode,
    pub system: Appearance,
}

impl Global for AppearanceState {}

/// Combine the user's choice with the OS state.
pub fn resolve(mode: AppearanceMode, system: Appearance) -> Appearance {
    match mode {
        AppearanceMode::System => system,
        AppearanceMode::Light => Appearance::Light,
        AppearanceMode::Dark => Appearance::Dark,
    }
}

/// Install the appearance globals and the matching theme. Call once at boot, before any window opens, so the first
/// frame is already the right palette (installing later produces a visible flash).
pub fn init(mode: AppearanceMode, cx: &mut App) {
    let system = Appearance::from_window(cx.window_appearance());
    tracing::debug!(?mode, ?system, "appearance: initial");
    cx.set_global(AppearanceState { mode, system });
    Theme::install(Theme::for_appearance(resolve(mode, system)), cx);
}

/// The mode currently in effect (defaults to `System` before [`init`]).
pub fn mode(cx: &App) -> AppearanceMode {
    cx.try_global::<AppearanceState>().map(|s| s.mode).unwrap_or_default()
}

/// The appearance currently painted.
pub fn current(cx: &App) -> Appearance {
    Theme::of(cx).appearance
}

/// Change the user's preference and repaint if that changed the palette.
pub fn set_mode(mode: AppearanceMode, cx: &mut App) {
    if !cx.has_global::<AppearanceState>() {
        return;
    }
    let state = cx.global_mut::<AppearanceState>();
    if state.mode == mode {
        return;
    }
    state.mode = mode;
    apply(cx);
}

/// Subscribe a window to OS appearance changes. The returned [`Subscription`] must outlive the window — callers
/// typically `.detach()` it. Reconciles against the *window's* appearance first: the window knows for certain.
pub fn observe_window(window: &mut Window, cx: &mut App) -> Subscription {
    sync(Appearance::from_window(window.appearance()), cx);
    sync_title_bar(window, current(cx));
    window.observe_window_appearance(|window, cx| {
        sync(Appearance::from_window(window.appearance()), cx);
        // gpui_windows re-applies the *system* DWM dark flag right after this callback returns; a pinned mode has
        // to win, so re-assert ours once the platform handler is done.
        let handle = window.window_handle();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_millis(30)).await;
            let _ = cx.update_window(handle, |_, window, cx| sync_title_bar(window, current(cx)));
        })
        .detach();
    })
}

/// Record the OS appearance and re-apply if it moved.
fn sync(system: Appearance, cx: &mut App) {
    if !cx.has_global::<AppearanceState>() {
        return;
    }
    let state = cx.global_mut::<AppearanceState>();
    if state.system == system {
        return;
    }
    tracing::debug!(?system, "appearance: system changed");
    state.system = system;
    apply(cx);
}

/// Re-resolve the palette and, if it moved, swap the theme and force a full repaint. A no-op when the resolved
/// appearance is unchanged (the OS fires the notification for accent-colour changes too).
pub fn apply(cx: &mut App) {
    let Some(state) = cx.try_global::<AppearanceState>() else {
        return;
    };
    let wanted = resolve(state.mode, state.system);
    if cx.try_global::<Theme>().is_some_and(|t| t.appearance == wanted) {
        return;
    }
    tracing::debug!(?wanted, "appearance: installing palette");
    Theme::install(Theme::for_appearance(wanted), cx);
    cx.refresh_windows();
    // The window handling a click is temporarily taken out of App, so updating it synchronously fails; defer until
    // every window is back (zeron's `reapply_window_background` note).
    cx.defer(move |cx| {
        for window in cx.windows() {
            let _ = window.update(cx, |_, window, _| sync_title_bar(window, wanted));
        }
    });
}

/// Paint the native caption in the app's appearance (Windows 10 1809+ `DWMWA_USE_IMMERSIVE_DARK_MODE`). gpui already
/// follows the *system* setting; this makes pinned Light/Dark modes match too.
#[cfg(windows)]
pub fn sync_title_bar(window: &Window, appearance: Appearance) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return;
    };
    let dark: windows_sys::core::BOOL = appearance.is_dark().into();
    unsafe {
        DwmSetWindowAttribute(
            win32.hwnd.get() as _,
            DWMWA_USE_IMMERSIVE_DARK_MODE as _,
            (&raw const dark).cast(),
            std::mem::size_of::<windows_sys::core::BOOL>() as u32,
        );
    }
}

#[cfg(not(windows))]
pub fn sync_title_bar(_window: &Window, _appearance: Appearance) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_mode_follows_the_os() {
        assert_eq!(resolve(AppearanceMode::System, Appearance::Light), Appearance::Light);
        assert_eq!(resolve(AppearanceMode::System, Appearance::Dark), Appearance::Dark);
    }

    #[test]
    fn pinned_modes_ignore_the_os() {
        for system in [Appearance::Light, Appearance::Dark] {
            assert_eq!(resolve(AppearanceMode::Light, system), Appearance::Light);
            assert_eq!(resolve(AppearanceMode::Dark, system), Appearance::Dark);
        }
    }

    #[test]
    fn mode_serialises_stably() {
        for (mode, json) in [
            (AppearanceMode::System, "\"system\""),
            (AppearanceMode::Light, "\"light\""),
            (AppearanceMode::Dark, "\"dark\""),
        ] {
            assert_eq!(serde_json::to_string(&mode).unwrap(), json);
            assert_eq!(serde_json::from_str::<AppearanceMode>(json).unwrap(), mode);
        }
    }
}
