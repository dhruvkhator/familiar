//! Embedded icon assets + the gpui [`AssetSource`] that serves them.
//!
//! Vendored from zeron's `crates/ui/src/icons.rs` (MIT, see `LICENSE-zeron`): the `icon_assets!` macro, the asset
//! source and [`icon`]. The set is a small, chosen subset of zeron's icons:
//! - Most glyphs are from the **Solar Icons** set (Linear weight) by 480 Design, licensed CC BY 4.0
//!   (<https://creativecommons.org/licenses/by/4.0/>); attribution: "Solar Icons by 480 Design".
//! - A few (`bell`, `home`, `info-circle`, `plus`, `close`) are zeron's own hand-drawn ports in the Solar
//!   Linear style (MIT).
//! - `folder` is Familiar's own drawing in the same style (MIT).
//!
//! No third-party brand marks are bundled. gpui tints SVGs with the text colour (monochrome), so icons take their
//! colour at the call site: `icon(icons::BELL).size(px(16.)).text_color(theme.muted)`.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString, Styled as _, Svg, svg};

macro_rules! icon_assets {
    ($(($const_name:ident, $path:literal)),+ $(,)?) => {
        $(pub const $const_name: &str = concat!("icons/", $path, ".svg");)+

        /// Every bundled icon path (the gallery's icon sheet).
        pub const ALL: &[&str] = &[$(concat!("icons/", $path, ".svg")),+];

        /// Serves the embedded icons to gpui's SVG renderer.
        pub struct Assets;

        impl AssetSource for Assets {
            fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
                Ok(match path {
                    $(concat!("icons/", $path, ".svg") => Some(Cow::Borrowed(
                        include_bytes!(concat!("../assets/icons/", $path, ".svg")).as_slice(),
                    )),)+
                    _ => None,
                })
            }

            fn list(&self, path: &str) -> Result<Vec<SharedString>> {
                Ok(ALL.iter().filter(|p| p.starts_with(path)).map(|p| SharedString::from(*p)).collect())
            }
        }
    };
}

icon_assets![
    (HOME, "home"),
    (BELL, "bell"),
    (SUN, "sun"),
    (MOON, "moon"),
    (MONITOR, "monitor"),
    (LAPTOP, "laptop"),
    (PLUS, "plus"),
    (CHECK, "check"),
    (CLOSE, "close"),
    (CLOSE_CIRCLE, "close-circle"),
    (DANGER_TRIANGLE, "danger-triangle"),
    (INFO_CIRCLE, "info-circle"),
    (CHAT_ROUND_LINE, "chat-round-line"),
    (CALENDAR, "calendar"),
    (CLOCK_CIRCLE, "clock-circle"),
    (SETTINGS, "settings"),
    (LOGOUT_2, "logout-2"),
    (MAGNIFER, "magnifer"),
    (ALT_ARROW_DOWN, "alt-arrow-down"),
    (ALT_ARROW_RIGHT, "alt-arrow-right"),
    (ARROW_RIGHT, "arrow-right"),
    (REFRESH, "refresh"),
    (WIDGET, "widget"),
    (CHECKLIST, "checklist"),
    (MAGIC_STICK_3, "magic-stick-3"),
    (PEN, "pen"),
    (COPY, "copy"),
    (LIST, "list"),
    (STAR, "star"),
    (EYE, "eye"),
    (FOLDER, "folder"),
];

/// An icon element for an embedded asset path. Size and colour are set by the caller.
pub fn icon(path: &'static str) -> Svg {
    svg().path(path).flex_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_icon_loads_and_parses() {
        for path in Assets.list("icons/").unwrap() {
            let bytes = Assets.load(&path).unwrap().unwrap_or_else(|| panic!("missing asset {path}"));
            let text = std::str::from_utf8(&bytes).expect("icon svg is utf-8");
            assert!(text.contains("<svg"), "{path} is not an svg");
            assert!(text.contains("viewBox"), "{path} lacks a viewBox");
        }
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
    }
}
