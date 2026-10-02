//! Shared notice chip — a tinted, wrapping failure/warning card.
//!
//! Vendored from zeron's `crates/ui/src/notice.rs` (MIT, see `LICENSE-zeron`); colours mapped onto Familiar's
//! tokens (`bad`/`warn` + their soft washes) and the tooltip onto [`crate::components::tooltip_text`].

use gpui::{AnyElement, Div, FontWeight, SharedString, div, prelude::*, px};

use crate::theme::Theme;

/// The chip's header icon treatment, which also picks its metrics.
pub enum NoticeChipIcon {
    /// Bare 14px triangle in the label color; 12px inset/radius (inline notices).
    Plain,
    /// 20px tinted tile holding a 12px triangle; 10px inset/radius (block notices).
    Tile,
}

/// A tinted rounded chip — a `<accent>/16%` border over a `<accent>/5%` wash, never a bare stroke — with a header
/// row (DangerTriangle + medium label, a small copy button: failure payloads are meant to be pasted) and the message
/// below. The message WRAPS instead of truncating: failure payloads carry exit statuses and stderr.
pub fn notice_chip(
    theme: &Theme,
    warning: bool,
    label: &'static str,
    message: impl Into<SharedString>,
    icon: NoticeChipIcon,
) -> Div {
    let accent = if warning { theme.warn } else { theme.bad };
    let message = message.into();
    let copy_message = message.clone();
    let tile = matches!(icon, NoticeChipIcon::Tile);
    let frame = |inset: f32| {
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .rounded(px(inset))
            .border_1()
            .border_color(accent.opacity(0.16))
            .bg(accent.opacity(0.05))
            .px(px(inset))
            .py(px(8.0))
    };
    let chip = if tile {
        frame(10.0).text_size(px(12.0)).text_color(accent)
    } else {
        frame(12.0).text_size(px(12.0)).line_height(px(16.0)).text_color(accent.opacity(0.9))
    };
    let header_icon: AnyElement = if tile {
        div()
            .flex_none()
            .size(px(20.0))
            .rounded(px(6.0))
            .bg(accent.opacity(0.12))
            .flex()
            .items_center()
            .justify_center()
            .child(crate::icons::icon(crate::icons::DANGER_TRIANGLE).size(px(12.0)).text_color(accent))
            .into_any_element()
    } else {
        crate::icons::icon(crate::icons::DANGER_TRIANGLE).size(px(14.0)).text_color(accent).into_any_element()
    };
    chip.child(
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(header_icon)
            .child(div().font_weight(FontWeight::MEDIUM).child(label))
            .child(div().flex_1())
            .child(
                div()
                    .id("notice-copy")
                    .flex_none()
                    .size(px(20.0))
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(move |s| s.bg(accent.opacity(0.12)))
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_message.to_string()));
                    })
                    .tooltip(crate::components::tooltip_text("Copy message"))
                    .child(crate::icons::icon(crate::icons::COPY).size(px(12.0)).text_color(accent.opacity(0.8))),
            ),
    )
    .child(div().min_w_0().w_full().text_color(theme.ink.opacity(0.8)).child(message))
}
