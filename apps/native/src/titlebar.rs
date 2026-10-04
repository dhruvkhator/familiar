//! Familiar's own title bar on Windows: drawn in the app's colours instead of the system caption. Windows still owns
//! the behaviour: the bar is a native caption area (drag, double-click to maximize, Aero Snap, the system menu) and
//! the three buttons are native caption buttons (Snap Layouts on hovering maximize), through GPUI's window control
//! areas. Other platforms keep their system title bar.

use std::sync::OnceLock;

use familiar_ui::theme::{SIDEBAR_WIDTH, Theme};
use gpui::{
    AnyElement, App, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, WindowControlArea, div, prelude::FluentBuilder as _, px, rgb,
    white,
};

/// The app draws its own title bar (Windows only).
pub const CUSTOM: bool = cfg!(windows);

pub const HEIGHT: f32 = 34.0;
/// Windows 11 caption buttons are 46 px wide.
const BUTTON_WIDTH: f32 = 46.0;

// Glyphs from Segoe Fluent Icons (Windows 11) / Segoe MDL2 Assets (Windows 10), the system caption button icons.
const MINIMIZE: &str = "\u{E921}";
const MAXIMIZE: &str = "\u{E922}";
const RESTORE: &str = "\u{E923}";
const CLOSE: &str = "\u{E8BB}";

/// The bar across the top of the window. `with_sidebar` continues the sidebar's colour and edge up into the bar.
pub fn render(with_sidebar: bool, window: &Window, cx: &App) -> AnyElement {
    let theme = Theme::of(cx);
    let active = window.is_window_active();
    let ink = if active { theme.ink } else { theme.muted };
    let font = icon_font(window);
    let max_glyph = if window.is_maximized() { RESTORE } else { MAXIMIZE };
    div()
        .id("titlebar")
        .flex()
        .flex_none()
        .w_full()
        .h(px(HEIGHT))
        .bg(theme.bg)
        .window_control_area(WindowControlArea::Drag)
        .when(with_sidebar, |bar| {
            bar.child(div().flex_none().w(px(SIDEBAR_WIDTH)).h_full().bg(theme.surface).border_r_1().border_color(theme.line))
        })
        .child(div().flex_1().h_full())
        .child(caption_button("titlebar-min", MINIMIZE, WindowControlArea::Min, &font, ink, theme.hover, ink))
        .child(caption_button("titlebar-max", max_glyph, WindowControlArea::Max, &font, ink, theme.hover, ink))
        // Close turns the system red on hover, as everywhere on Windows.
        .child(caption_button("titlebar-close", CLOSE, WindowControlArea::Close, &font, ink, rgb(0xC42B1C).into(), white()))
        .into_any_element()
}

fn caption_button(
    id: &'static str,
    glyph: &'static str,
    area: WindowControlArea,
    font: &SharedString,
    ink: Hsla,
    hover_bg: Hsla,
    hover_ink: Hsla,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(px(BUTTON_WIDTH))
        .h_full()
        .occlude()
        .font_family(font.clone())
        .text_size(px(10.0))
        .text_color(ink)
        .hover(move |s| s.bg(hover_bg).text_color(hover_ink))
        .active(move |s| s.bg(hover_bg.opacity(0.8)).text_color(hover_ink))
        .window_control_area(area)
        .child(glyph)
}

/// Segoe Fluent Icons on Windows 11, Segoe MDL2 Assets on Windows 10 (same glyph codes).
fn icon_font(window: &Window) -> SharedString {
    static FONT: OnceLock<SharedString> = OnceLock::new();
    FONT.get_or_init(|| {
        let fluent = "Segoe Fluent Icons";
        let name = if window.text_system().all_font_names().iter().any(|n| n == fluent) { fluent } else { "Segoe MDL2 Assets" };
        SharedString::new_static(name)
    })
    .clone()
}
