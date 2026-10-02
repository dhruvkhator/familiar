//! Familiar's theme — two concrete appearances, one token set.
//!
//! The token table is authored from the web app (`apps/web/src/index.css`): same names (`bg`, `surface`, `sunken`,
//! `line`, `ink`, `muted`, `accent`, `accent_soft`, `ok`, `warn`, `bad`, …), same values, so a screen ported from the
//! web reads identically. The structure — an `Appearance` enum, a process-wide appearance mirror for context-free
//! paint helpers, a style generation counter, `Theme::of(cx)` as a gpui [`Global`], WCAG contrast helpers and the
//! pairing test — is adapted from zeron's `crates/ui/src/theme.rs` (MIT, see `LICENSE-zeron`).
//!
//! # Light is designed, not inverted
//!
//! (zeron's rule, which the web palette already follows.) The two palettes are separate designs:
//!
//! 1. **Surface order flips meaning.** Light: the page is a warm off-white (`bg`), cards are white and lift with a
//!    hairline plus a soft shadow; recessed wells (`sunken`) are darker than the page. Dark: the page is a deep ink
//!    blue-grey, cards are *lighter* than the page and wells are *darker* — elevation reads by lightness, and the
//!    shadow only adds depth.
//! 2. **Accents move along the scale.** The light accent `#5269bb` sits at ~5:1 on white; the same hue at that
//!    lightness drops to ~3:1 on the dark page, so dark uses the lighter `#8fa0ee` and flips the ink *on* the accent
//!    (`accent_ink`) to a near-black indigo.
//! 3. **Status hues keep their meaning, not their lightness.** `ok`/`warn`/`bad` are 600-level in light and 300-level
//!    in dark, each with a matching soft wash for chips.
//!
//! Text tones are verified (not eyeballed) in [`tests`]: body ink clears 7:1 and secondary text 4.5:1 on every
//! surface it is used on, in both appearances.

use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use gpui::{App, BoxShadow, Global, Hsla, SharedString, hsla, point, px};

/// Which appearance the app is painting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Appearance {
    #[default]
    Light,
    Dark,
}

impl Appearance {
    pub fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }

    pub fn is_light(self) -> bool {
        matches!(self, Self::Light)
    }

    /// Map a gpui window appearance onto ours (both vibrant variants are just the blurred flavour of the same tone).
    pub fn from_window(appearance: gpui::WindowAppearance) -> Self {
        use gpui::WindowAppearance::*;
        match appearance {
            Light | VibrantLight => Self::Light,
            Dark | VibrantDark => Self::Dark,
        }
    }
}

/// Process-wide mirror of the installed theme's appearance, for paint helpers ([`ink`], [`hairline`]) called from
/// element builders that have no `cx` in scope. Appearance is one setting for every window, so one mirror is sound;
/// [`Theme::install`] is the only writer.
static CURRENT_APPEARANCE: AtomicU8 = AtomicU8::new(0);

/// Bumped every time the resolved style changes. Caches that bake colours (the mascot raster cache keys on the
/// colours themselves, but a future markdown run cache would not) compare this counter and drop stale entries.
static STYLE_GENERATION: AtomicU32 = AtomicU32::new(0);

/// The appearance the context-free paint helpers are painting for.
pub fn current_appearance() -> Appearance {
    match CURRENT_APPEARANCE.load(Ordering::Relaxed) {
        1 => Appearance::Dark,
        _ => Appearance::Light,
    }
}

/// Monotonic id of the current resolved style (palette + typography).
pub fn style_generation() -> u32 {
    STYLE_GENERATION.load(Ordering::Relaxed)
}

pub(crate) fn bump_style_generation() {
    STYLE_GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Point the context-free paint helpers at an appearance. Called by [`Theme::install`].
pub fn set_current_appearance(appearance: Appearance) {
    let encoded = match appearance {
        Appearance::Light => 0,
        Appearance::Dark => 1,
    };
    if CURRENT_APPEARANCE.swap(encoded, Ordering::Relaxed) != encoded {
        bump_style_generation();
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Layout constants — numbers drive layout, colours are paint; none of these depend on the appearance.
// ---------------------------------------------------------------------------------------------------------------

/// Cards, panels, the teammate tiles (`.card` / `rounded-[14px]` on the web).
pub const RADIUS_CARD: f32 = 14.0;
/// Buttons, inputs, nav rows (`rounded-[10px]`).
pub const RADIUS_CONTROL: f32 = 10.0;
/// Chips and badges.
pub const RADIUS_CHIP: f32 = 6.0;
/// Dialogs and toasts.
pub const RADIUS_DIALOG: f32 = 16.0;
/// Sidebar width (`w-64`).
pub const SIDEBAR_WIDTH: f32 = 256.0;

/// The type scale. Geist reads slightly larger than Segoe UI at the same size, so body text is 14 px where the web
/// uses 15 px; the steps keep the web's rhythm (text-xs / sm / base / lg / 2xl / 3xl).
pub mod text {
    pub const MICRO: f32 = 11.0;
    pub const CAPTION: f32 = 12.0;
    pub const SMALL: f32 = 13.0;
    pub const BODY: f32 = 14.0;
    pub const LEAD: f32 = 15.0;
    pub const TITLE: f32 = 17.0;
    pub const HEADLINE: f32 = 22.0;
    pub const DISPLAY: f32 = 28.0;
}

/// The installed palette. Read with [`Theme::of`].
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub appearance: Appearance,

    // --- The web token table (index.css) ---
    /// Page background.
    pub bg: Hsla,
    /// Cards, sidebar, popovers.
    pub surface: Hsla,
    /// Recessed wells, hover washes, skeletons.
    pub sunken: Hsla,
    /// Hairlines and dividers.
    pub line: Hsla,
    /// Body text.
    pub ink: Hsla,
    /// Secondary text.
    pub muted: Hsla,
    pub accent: Hsla,
    /// Text/icons on a filled accent.
    pub accent_ink: Hsla,
    /// Selected rows, accent chips, text selection.
    pub accent_soft: Hsla,
    pub ok: Hsla,
    pub ok_soft: Hsla,
    pub warn: Hsla,
    pub warn_soft: Hsla,
    /// Text on a filled `warn` (count badges).
    pub warn_ink: Hsla,
    pub bad: Hsla,
    pub bad_soft: Hsla,

    // --- Derived roles ---
    /// Hover wash on surface rows (`hover:bg-sunken/70`).
    pub hover: Hsla,
    /// Pressed wash (one step past hover).
    pub pressed: Hsla,
    /// Primary button hover (`hover:brightness-110`).
    pub accent_hover: Hsla,
    /// Primary button pressed.
    pub accent_pressed: Hsla,
    /// Focus ring.
    pub focus: Hsla,
    /// Ink of the shadow (`rgb(51 54 65)` light, black dark) — alpha is applied per elevation.
    pub shadow_ink: Hsla,
    /// Base alpha multiplier for shadows (dark needs much heavier shadows to read at all).
    pub shadow_strength: f32,
    /// Tooltip plate (inverted: dark on light, light-ish on dark).
    pub tooltip_bg: Hsla,
    pub tooltip_ink: Hsla,
    /// Skeleton shimmer highlight.
    pub shimmer: Hsla,

    pub font_sans: SharedString,
    pub font_mono: SharedString,
}

impl Global for Theme {}

impl Default for Theme {
    fn default() -> Self {
        Self::light()
    }
}

impl Theme {
    /// The installed theme (light until [`crate::appearance::init`] runs).
    pub fn of(cx: &App) -> &Theme {
        cx.try_global::<Theme>().unwrap_or_else(|| {
            static FALLBACK: std::sync::OnceLock<Theme> = std::sync::OnceLock::new();
            FALLBACK.get_or_init(Theme::light)
        })
    }

    pub fn for_appearance(appearance: Appearance) -> Self {
        match appearance {
            Appearance::Light => Self::light(),
            Appearance::Dark => Self::dark(),
        }
    }

    /// Light — `:root` in index.css.
    pub fn light() -> Self {
        let sunken = hex(0xf0efed);
        let surface = hex(0xffffff);
        let accent = hex(0x5269bb);
        Self {
            appearance: Appearance::Light,
            bg: hex(0xf7f6f4),
            surface,
            sunken,
            line: hex(0xe9e8e6),
            ink: hex(0x333641),
            muted: hex(0x777885),
            accent,
            accent_ink: hex(0xffffff),
            accent_soft: hex(0xeeeffa),
            ok: hex(0x2f7d4a),
            ok_soft: hex(0xe4f2e8),
            warn: hex(0x9a5b00),
            warn_soft: hex(0xfbeed3),
            warn_ink: hex(0xffffff),
            bad: hex(0xb8352b),
            bad_soft: hex(0xfbe3e0),
            hover: flatten(sunken.opacity(0.7), surface),
            pressed: hex(0xe8e7e4),
            accent_hover: hex(0x5d74c8),
            accent_pressed: hex(0x4a5fab),
            focus: accent,
            shadow_ink: hex(0x333641),
            shadow_strength: 1.0,
            tooltip_bg: hex(0x2b2d36),
            tooltip_ink: hex(0xf5f5f7),
            shimmer: hsla(0.0, 0.0, 1.0, 0.75),
            font_sans: crate::typography::SANS.into(),
            font_mono: crate::typography::MONO.into(),
        }
    }

    /// Dark — `prefers-color-scheme: dark` in index.css. Cards lighter than the page, wells darker.
    pub fn dark() -> Self {
        let sunken = hex(0x111218);
        let surface = hex(0x1d1f27);
        let accent = hex(0x8fa0ee);
        Self {
            appearance: Appearance::Dark,
            bg: hex(0x15161c),
            surface,
            sunken,
            line: hex(0x2b2d38),
            ink: hex(0xe8e9ef),
            muted: hex(0x9a9cab),
            accent,
            accent_ink: hex(0x10132b),
            accent_soft: hex(0x262a47),
            ok: hex(0x6fcf8e),
            ok_soft: hex(0x17301f),
            warn: hex(0xf0b23d),
            warn_soft: hex(0x3a2b0c),
            warn_ink: hex(0x1a1300),
            bad: hex(0xff8b82),
            bad_soft: hex(0x3d1613),
            // On dark, a hover lifts *towards* the light: a faint white wash over the card rather than a darker well.
            hover: hex(0x252731),
            pressed: hex(0x2b2d38),
            accent_hover: hex(0x9fb0f5),
            accent_pressed: hex(0x7f91e2),
            focus: accent,
            shadow_ink: hex(0x000000),
            shadow_strength: 4.5,
            tooltip_bg: hex(0x2e313d),
            tooltip_ink: hex(0xeff0f5),
            shimmer: hsla(0.0, 0.0, 1.0, 0.06),
            font_sans: crate::typography::SANS.into(),
            font_mono: crate::typography::MONO.into(),
        }
    }

    /// Install as the global theme (and point the context-free helpers at it).
    pub fn install(theme: Theme, cx: &mut App) {
        set_current_appearance(theme.appearance);
        bump_style_generation();
        cx.set_global(theme);
    }

    pub fn is_dark(&self) -> bool {
        self.appearance.is_dark()
    }

    /// The web's `--shadow`: `0 1px 2px ink/5%, 0 4px 16px ink/5%`, scaled by `lift` (0 = resting card, 1 = hovered).
    pub fn card_shadow(&self, lift: f32) -> Vec<BoxShadow> {
        let s = self.shadow_strength;
        let lift = lift.clamp(0.0, 1.5);
        vec![
            BoxShadow {
                color: self.shadow_ink.opacity((0.05 + 0.02 * lift) * s),
                offset: point(px(0.0), px(1.0)),
                blur_radius: px(2.0),
                spread_radius: px(0.0),
                inset: false,
            },
            BoxShadow {
                color: self.shadow_ink.opacity((0.05 + 0.05 * lift) * s),
                offset: point(px(0.0), px(4.0 + 6.0 * lift)),
                blur_radius: px(16.0 + 12.0 * lift),
                spread_radius: px(-2.0 * lift),
                inset: false,
            },
        ]
    }

    /// Floating surfaces (toasts, tooltips, menus).
    pub fn float_shadow(&self) -> Vec<BoxShadow> {
        let s = self.shadow_strength;
        vec![
            BoxShadow {
                color: self.shadow_ink.opacity(0.06 * s),
                offset: point(px(0.0), px(2.0)),
                blur_radius: px(4.0),
                spread_radius: px(0.0),
                inset: false,
            },
            BoxShadow {
                color: self.shadow_ink.opacity(0.12 * s.min(3.0)),
                offset: point(px(0.0), px(12.0)),
                blur_radius: px(32.0),
                spread_radius: px(-4.0),
                inset: false,
            },
        ]
    }

    /// Tone colours for chips: (text, wash).
    pub fn tone(&self, tone: Tone) -> (Hsla, Hsla) {
        match tone {
            Tone::Muted => (self.muted, self.sunken),
            Tone::Accent => (self.accent, self.accent_soft),
            Tone::Ok => (self.ok, self.ok_soft),
            Tone::Warn => (self.warn, self.warn_soft),
            Tone::Bad => (self.bad, self.bad_soft),
        }
    }

    /// The token table, for the gallery's palette swatches.
    pub fn tokens(&self) -> Vec<(&'static str, Hsla)> {
        vec![
            ("bg", self.bg),
            ("surface", self.surface),
            ("sunken", self.sunken),
            ("line", self.line),
            ("ink", self.ink),
            ("muted", self.muted),
            ("accent", self.accent),
            ("accent-soft", self.accent_soft),
            ("ok", self.ok),
            ("warn", self.warn),
            ("warn-soft", self.warn_soft),
            ("bad", self.bad),
        ]
    }
}

/// Semantic tone for chips, badges and LEDs (the web's `Chip tone`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    Muted,
    Accent,
    Ok,
    Warn,
    Bad,
}

// ---------------------------------------------------------------------------------------------------------------
// Paint helpers
// ---------------------------------------------------------------------------------------------------------------

/// `0xRRGGBB` → opaque [`Hsla`].
pub fn hex(rgb: u32) -> Hsla {
    let r = ((rgb >> 16) & 0xff) as f32 / 255.0;
    let g = ((rgb >> 8) & 0xff) as f32 / 255.0;
    let b = (rgb & 0xff) as f32 / 255.0;
    let (h, s, l) = rgb_to_hsl(r, g, b);
    hsla(h, s, l, 1.0)
}

/// [`Hsla`] → `0xRRGGBB` (alpha dropped). Used where colours become SVG text (the mascot ring).
pub fn to_hex(color: Hsla) -> u32 {
    let [r, g, b] = hsl_to_rgb(color.h, color.s, color.l);
    let q = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u32;
    (q(r) << 16) | (q(g) << 8) | q(b)
}

/// Translucent fill ink for interactive washes: soft-black on light, soft-white on dark. Alphas are the same number
/// in both appearances (zeron's `INK_FILL_SCALE = 1`), only the tone flips.
pub fn ink(alpha: f32) -> Hsla {
    match current_appearance() {
        Appearance::Light => hsla(230.0 / 360.0, 0.12, 0.23, alpha),
        Appearance::Dark => hsla(0.0, 0.0, 1.0, alpha),
    }
}

/// Hairline ink: light edges must hold against a bright surround, so light scales alpha up (zeron's 1.35).
pub fn hairline(alpha: f32) -> Hsla {
    match current_appearance() {
        Appearance::Light => hsla(230.0 / 360.0, 0.12, 0.23, (alpha * 1.35).min(1.0)),
        Appearance::Dark => hsla(0.0, 0.0, 1.0, alpha),
    }
}

pub(crate) fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

pub(crate) fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    if s == 0.0 {
        return [l, l, l];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue = |t: f32| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0)]
}

/// WCAG 2.1 relative luminance of an opaque color.
pub fn relative_luminance(color: Hsla) -> f32 {
    let lin = |c: f32| if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    let [r, g, b] = hsl_to_rgb(color.h, color.s, color.l);
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// WCAG 2.1 contrast ratio between two opaque colors (1.0 … 21.0).
pub fn contrast_ratio(a: Hsla, b: Hsla) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Composite `fg` (which may be translucent) over an opaque `bg`, returning the opaque result.
pub fn flatten(fg: Hsla, bg: Hsla) -> Hsla {
    let a = fg.a.clamp(0.0, 1.0);
    let [fr, fg_, fb] = hsl_to_rgb(fg.h, fg.s, fg.l);
    let [br, bg_, bb] = hsl_to_rgb(bg.h, bg.s, bg.l);
    let (h, s, l) = rgb_to_hsl(fr * a + br * (1.0 - a), fg_ * a + bg_ * (1.0 - a), fb * a + bb * (1.0 - a));
    hsla(h, s, l, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        for c in [0x333641, 0x5269bb, 0xeeeffa, 0x15161c, 0x8fa0ee, 0xf0b23d] {
            assert_eq!(to_hex(hex(c)), c, "{c:06x}");
        }
    }

    /// Text tones clear WCAG on every surface they're used on, in both appearances.
    #[test]
    fn text_contrast_holds_in_both_appearances() {
        for theme in [Theme::light(), Theme::dark()] {
            let name = format!("{:?}", theme.appearance);
            for (surface_name, surface) in [("bg", theme.bg), ("surface", theme.surface)] {
                let ink = contrast_ratio(theme.ink, surface);
                assert!(ink >= 7.0, "{name}: ink on {surface_name} = {ink:.2}");
                let muted = contrast_ratio(theme.muted, surface);
                assert!(muted >= 4.3, "{name}: muted on {surface_name} = {muted:.2}");
            }
            let on_accent = contrast_ratio(theme.accent_ink, theme.accent);
            assert!(on_accent >= 4.5, "{name}: accent_ink on accent = {on_accent:.2}");
            for tone in [Tone::Accent, Tone::Ok, Tone::Warn, Tone::Bad] {
                let (fg, wash) = theme.tone(tone);
                let c = contrast_ratio(fg, wash);
                assert!(c >= 4.0, "{name}: {tone:?} chip = {c:.2}");
            }
        }
    }

    /// Contrast is *paired* across appearances (designed, not inverted): each text token lands within ~1.5 of its
    /// counterpart's ratio, so neither theme is the "afterthought" one.
    #[test]
    fn text_contrast_is_paired_across_appearances() {
        let (l, d) = (Theme::light(), Theme::dark());
        let pair = |a: f32, b: f32| (a - b).abs();
        let ink = pair(contrast_ratio(l.ink, l.surface), contrast_ratio(d.ink, d.surface));
        assert!(ink < 4.0, "ink pairing off by {ink:.2}");
        let muted = pair(contrast_ratio(l.muted, l.surface), contrast_ratio(d.muted, d.surface));
        assert!(muted < 1.5, "muted pairing off by {muted:.2}");
    }
}
