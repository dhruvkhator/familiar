//! Interface typography: bundled font registration and the effective family.
//!
//! Vendored from zeron's `crates/ui/src/typography.rs` (MIT, see `LICENSE-zeron`), trimmed for Familiar: the
//! installed-font catalog scan, terminal/code families and the settings writer are gone; what stays is the bundled
//! Geist / Geist Mono registration (SIL OFL 1.1, `assets/fonts/licenses/Geist-OFL.txt`), the family enum with a
//! system fallback, rem helpers and the typography generation counter.

use std::borrow::Cow;

use gpui::{App, Global, Rems, SharedString, rems};

/// Interface sans family name (as registered from the bundled TTFs).
pub const SANS: &str = "Geist";
/// Monospace family name.
pub const MONO: &str = "Geist Mono";

/// A bundled or system interface font choice.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum UiFontFamily {
    #[default]
    Geist,
    GeistMono,
    System,
}

impl UiFontFamily {
    pub fn label(&self) -> &str {
        match self {
            Self::Geist => "Geist",
            Self::GeistMono => "Geist Mono",
            Self::System => "System UI",
        }
    }

    pub fn family_name(&self) -> &str {
        match self {
            Self::Geist => SANS,
            Self::GeistMono => MONO,
            // zeron maps this to `.SystemUIFont` (a macOS name); name the platform face directly.
            Self::System => system_sans(),
        }
    }
}

fn system_sans() -> &'static str {
    if cfg!(target_os = "macos") {
        "Helvetica"
    } else if cfg!(target_os = "windows") {
        "Segoe UI"
    } else {
        "DejaVu Sans"
    }
}

/// Convert a size designed at the 16px baseline into a scalable interface rem.
pub const fn ui_rems(pixels_at_default: f32) -> Rems {
    rems(pixels_at_default / 16.0)
}

const GEIST: [&[u8]; 8] = [
    include_bytes!("../assets/fonts/Geist.ttf"),
    include_bytes!("../assets/fonts/Geist-Italic.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../assets/fonts/Geist-MediumItalic.ttf"),
    include_bytes!("../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Geist-SemiBoldItalic.ttf"),
    include_bytes!("../assets/fonts/Geist-Bold.ttf"),
    include_bytes!("../assets/fonts/Geist-BoldItalic.ttf"),
];

const GEIST_MONO: [&[u8]; 8] = [
    include_bytes!("../assets/fonts/GeistMono.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Italic.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Medium.ttf"),
    include_bytes!("../assets/fonts/GeistMono-MediumItalic.ttf"),
    include_bytes!("../assets/fonts/GeistMono-SemiBold.ttf"),
    include_bytes!("../assets/fonts/GeistMono-SemiBoldItalic.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Bold.ttf"),
    include_bytes!("../assets/fonts/GeistMono-BoldItalic.ttf"),
];

/// Which bundled families registered during this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FontAvailability {
    pub geist: bool,
    pub geist_mono: bool,
}

fn register_family(cx: &App, label: &str, faces: &'static [&'static [u8]]) -> bool {
    let fonts = faces.iter().map(|face| Cow::Borrowed(*face)).collect();
    match cx.text_system().add_fonts(fonts) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(font_family = label, error = %err, "failed to register bundled font family");
            false
        }
    }
}

/// Register each family independently so one bad asset cannot hide the other.
pub fn register_fonts(cx: &App) -> FontAvailability {
    FontAvailability {
        geist: register_family(cx, SANS, &GEIST),
        geist_mono: register_family(cx, MONO, &GEIST_MONO),
    }
}

/// Requested and effective typography for the process.
pub struct TypographyState {
    pub requested: UiFontFamily,
    pub effective: UiFontFamily,
    pub availability: FontAvailability,
    generation: u32,
}

impl Global for TypographyState {}

fn resolve_effective(requested: &UiFontFamily, availability: FontAvailability) -> UiFontFamily {
    match requested {
        UiFontFamily::Geist if !availability.geist => UiFontFamily::System,
        UiFontFamily::GeistMono if !availability.geist_mono => UiFontFamily::System,
        other => other.clone(),
    }
}

/// Register the bundled fonts and install typography state. Call before the first window opens.
pub fn init(requested: UiFontFamily, cx: &mut App) {
    let availability = register_fonts(cx);
    let effective = resolve_effective(&requested, availability);
    cx.set_global(TypographyState { requested, effective, availability, generation: 0 });
}

pub fn effective(cx: &App) -> UiFontFamily {
    cx.try_global::<TypographyState>().map(|s| s.effective.clone()).unwrap_or_default()
}

pub fn effective_family_name(cx: &App) -> SharedString {
    effective(cx).family_name().to_owned().into()
}

/// Monotonic id of the current effective UI typography; layout caches compare it.
pub fn generation(cx: &App) -> u32 {
    cx.try_global::<TypographyState>().map(|s| s.generation).unwrap_or_default()
}

/// Apply a family and repaint. Returns whether the effective family changed.
pub fn set_family(family: UiFontFamily, cx: &mut App) -> bool {
    let Some(state) = cx.try_global::<TypographyState>() else {
        return false;
    };
    let effective = resolve_effective(&family, state.availability);
    if state.effective == effective && state.requested == family {
        return false;
    }
    let changed = state.effective != effective;
    let state = cx.global_mut::<TypographyState>();
    state.requested = family;
    state.effective = effective;
    if changed {
        state.generation = state.generation.wrapping_add(1);
        crate::theme::bump_style_generation();
        cx.refresh_windows();
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_geist_falls_back_to_system() {
        let none = FontAvailability::default();
        assert_eq!(resolve_effective(&UiFontFamily::Geist, none), UiFontFamily::System);
        let all = FontAvailability { geist: true, geist_mono: true };
        assert_eq!(resolve_effective(&UiFontFamily::Geist, all), UiFontFamily::Geist);
    }

    #[test]
    fn rems_scale_from_sixteen() {
        assert_eq!(ui_rems(14.0).0, 0.875);
    }
}
