//! `familiar-ui` — Familiar's native design system over GPUI.
//!
//! - [`theme`] — the token table from the web app (`apps/web/src/index.css`), light and dark, as `Theme::of(cx)`.
//! - [`appearance`] — System/Light/Dark, following Windows live (vendored from zeron).
//! - [`typography`] — bundled Geist / Geist Mono (vendored from zeron).
//! - [`motion`] — zeron's motion catalog, pulse clock and reduced-motion plumbing (vendored).
//! - [`anim`] — Familiar's motion language built on it: appear, stagger, springs, crossfade, expand, pulse.
//! - [`mascot`] — the teammate mascot, rasterised from the web's SVG artwork.
//! - [`components`], [`toast`], [`notice`] — the component kit.
//! - [`icons`], [`edge_fade`] — vendored from zeron.
//!
//! Several modules are vendored from zeron (<https://github.com/zeronsh/zeron>, MIT © 2026 Wing; `LICENSE-zeron`).
//! Bundled assets: Geist fonts (SIL OFL 1.1), Solar Icons (CC BY 4.0, 480 Design). See THIRD_PARTY_NOTICES.md.

pub mod anim;
pub mod appearance;
pub mod components;
pub mod edge_fade;
pub mod icons;
pub mod mascot;
pub mod motion;
pub mod notice;
pub mod theme;
pub mod toast;
pub mod typography;

pub use appearance::AppearanceMode;
pub use theme::{Appearance, Theme, Tone};

use gpui::App;

/// Boot the design system: fonts, theme (resolved against the OS appearance), reduced motion. Call inside
/// `Application::run` before opening a window, after `Application::with_assets(familiar_ui::icons::Assets)`.
pub fn init(mode: AppearanceMode, cx: &mut App) {
    typography::init(typography::UiFontFamily::Geist, cx);
    appearance::init(mode, cx);
    motion::init(motion::ReduceMotion::System, false, cx);
}

/// Per-window wiring: follow OS appearance changes and re-read the reduce-motion setting on focus. Call from the
/// root view constructor.
pub fn observe_window<V: 'static>(window: &mut gpui::Window, cx: &mut gpui::Context<V>) {
    appearance::observe_window(window, cx).detach();
    cx.observe_window_activation(window, |_, window, cx| {
        motion::window_activation_changed(window.is_window_active(), cx)
    })
    .detach();
}
