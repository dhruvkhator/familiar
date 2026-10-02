//! The teammate mascot — a native port of the web app's `components/Mascot.tsx`.
//!
//! The artwork is multi-colour SVG (body fill from a palette, a soft gradient overlay, white eyes, ink features,
//! accessories). gpui's `svg()` element is monochrome (it rasterises an alpha mask and tints it with the text colour),
//! so it can't draw this. Instead the same SVG markup the web builds is generated here, rasterised with `resvg`
//! (the version gpui already links) at the exact *device-pixel* size, converted to the straight-alpha BGRA gpui's
//! sprite atlas expects, and shown with `img(ImageSource::Render)`. Drawn 1:1 with device pixels it stays crisp at
//! any display scale. Rasters are cached per (avatar, state, device size, blink, ring colour) — a handful per bot.
//!
//! State motion (same vocabulary as the web's `.mascot-*` classes):
//! - **working** — 2 s bob (5 px at 54 px) on the shared pulse clock; eyes glance right.
//! - **needs-you** — tilted +6°, wide eyes, "o" mouth, dashed amber ring (theme `warn`).
//! - **done** — tilted −3°, big smile.
//! - **paused** — sleepy eyes, desaturated 50% and at 85% opacity (baked into the raster).
//! - **idle** — blinks every ~6 s (phase offset per teammate so a row never blinks in unison). Between blinks no
//!   frames are scheduled at all: a one-shot wake-up re-renders the view just in time for the next blink.
//!
//! State changes crossfade (old raster fades out over the new one) instead of snapping. Reduced motion freezes the
//! bob and blink and snaps state changes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, ImageSource, IntoElement, ParentElement as _, RenderImage, RenderOnce, SharedString, Styled as _, Window,
    div, img, px,
};
use smallvec::SmallVec;

use crate::motion::{self, CubicBezier, MotionSpec};
use crate::theme::{Theme, to_hex};

/// What a teammate is doing (the web's `MascotState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MascotState {
    Working,
    NeedsYou,
    Done,
    Paused,
    Idle,
}

impl MascotState {
    pub const ALL: [Self; 5] = [Self::Working, Self::NeedsYou, Self::Done, Self::Paused, Self::Idle];

    /// The web's `STATE_LABEL`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::NeedsYou => "Needs you",
            Self::Done => "Just finished",
            Self::Paused => "Paused",
            Self::Idle => "Idle",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Accessory {
    None,
    Hat,
    Glasses,
    Headphones,
    Bow,
    Antenna,
    Crown,
}

impl Accessory {
    pub const ALL: [Self; 7] =
        [Self::None, Self::Hat, Self::Glasses, Self::Headphones, Self::Bow, Self::Antenna, Self::Crown];

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "none" => Self::None,
            "hat" => Self::Hat,
            "glasses" => Self::Glasses,
            "headphones" => Self::Headphones,
            "bow" => Self::Bow,
            "antenna" => Self::Antenna,
            "crown" => Self::Crown,
            _ => return None,
        })
    }
}

/// Stored as `bots.avatar` (jsonb); see [`resolve_avatar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Avatar {
    pub shape: u8,
    /// `0xRRGGBB`.
    pub color: u32,
    pub eyes: u8,
    pub mouth: u8,
    pub accessory: Accessory,
}

pub const PALETTE: [u32; 6] = [0x7285d5, 0xe58fa4, 0x4fb98a, 0xeda84b, 0xa283d8, 0x4fa9cf];
pub const SHAPE_COUNT: u8 = 5;
pub const EYE_COUNT: u8 = 5;
pub const MOUTH_COUNT: u8 = 4;

/// The web's FNV-1a over UTF-16 code units, with JS int32 semantics (`h ^= c; h = Math.imul(h, 16777619)`).
fn hash(s: &str) -> u32 {
    let mut h: i32 = 2166136261u32 as i32;
    for unit in s.encode_utf16() {
        h ^= unit as i32;
        h = h.wrapping_mul(16777619);
    }
    h.unsigned_abs()
}

/// `defaultAvatar(id)` — the id-derived look of a teammate who never customised theirs.
pub fn default_avatar(id: &str) -> Avatar {
    let h = hash(id);
    Avatar {
        shape: (h % SHAPE_COUNT as u32) as u8,
        color: PALETTE[((h >> 3) % PALETTE.len() as u32) as usize],
        eyes: 0,
        mouth: 0,
        accessory: Accessory::None,
    }
}

/// `resolveAvatar(id, stored)` — merge a possibly partial / invalid stored avatar over the default.
pub fn resolve_avatar(id: &str, stored: Option<&serde_json::Value>) -> Avatar {
    let d = default_avatar(id);
    let Some(obj) = stored.and_then(|v| v.as_object()) else {
        return d;
    };
    let num = |key: &str, max: u8, dflt: u8| {
        obj.get(key).and_then(|v| v.as_f64()).filter(|v| *v >= 0.0 && *v < max as f64).map(|v| v as u8).unwrap_or(dflt)
    };
    let color = obj
        .get("color")
        .and_then(|v| v.as_str())
        .and_then(|s| s.strip_prefix('#'))
        .filter(|s| s.len() == 6)
        .and_then(|s| u32::from_str_radix(s, 16).ok())
        .unwrap_or(d.color);
    Avatar {
        shape: num("shape", SHAPE_COUNT, d.shape),
        color,
        eyes: num("eyes", EYE_COUNT, d.eyes),
        mouth: num("mouth", MOUTH_COUNT, d.mouth),
        accessory: obj.get("accessory").and_then(|v| v.as_str()).and_then(Accessory::parse).unwrap_or(d.accessory),
    }
}

// -----------------------------------------------------------------------------------------------------------------
// Artwork (1:1 with Mascot.tsx)
// -----------------------------------------------------------------------------------------------------------------

struct Shape {
    d: &'static str,
    top: f32,
    dy: f32,
}

/// Body outlines in a 100×100 box; `top` = highest y, `dy` = vertical shift for the face.
const SHAPES: [Shape; 5] = [
    Shape { d: "M50 10c22 0 38 15 38 38 0 24-14 42-38 42S12 72 12 48C12 25 28 10 50 10z", top: 10.0, dy: 0.0 },
    Shape { d: "M50 6c20 0 32 18 32 42 0 28-14 44-32 44S18 76 18 48C18 24 30 6 50 6z", top: 6.0, dy: -2.0 },
    Shape {
        d: "M30 14h40c12 0 20 8 20 20v34c0 12-8 20-20 20H30C18 88 10 80 10 68V34c0-12 8-20 20-20z",
        top: 14.0,
        dy: 0.0,
    },
    Shape { d: "M50 20c26 0 42 12 42 34 0 20-16 34-42 34S8 74 8 54C8 32 24 20 50 20z", top: 20.0, dy: 7.0 },
    Shape { d: "M14 52C14 28 30 10 50 10s36 18 36 42v38l-12-8-12 8-12-8-12 8-12-8z", top: 10.0, dy: 0.0 },
];

const INK: &str = "#2b2d3a";

/// Padding (viewBox units) around the 100-unit box so tilts, the ring and tall accessories never clip.
const PAD: f32 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Variant {
    avatar: Avatar,
    state: MascotState,
    blink: bool,
    ring: u32,
}

fn eyes(style: u8, state: MascotState, blink: bool) -> String {
    if state == MascotState::Paused {
        return format!(
            r#"<g stroke="{INK}" stroke-width="4" stroke-linecap="round" fill="none"><path d="M30 47q7 6 14 0"/><path d="M56 47q7 6 14 0"/></g>"#
        );
    }
    let wide = state == MascotState::NeedsYou;
    let look = if state == MascotState::Working { 2.0 } else { 0.0 };
    let eye = |cx0: f32| -> String {
        match if wide { 0 } else { style } {
            1 => format!(
                r##"<ellipse cx="{cx0}" cy="46" rx="9" ry="11" fill="#fff"/><circle cx="{}" cy="47" r="5" fill="{INK}"/><circle cx="{}" cy="45" r="1.6" fill="#fff"/>"##,
                cx0 + look,
                cx0 + look + 1.6
            ),
            2 => format!(r#"<circle cx="{}" cy="47" r="4.6" fill="{INK}"/>"#, cx0 + look / 2.0),
            3 => format!(
                r#"<path d="M{} 49q7 -9 14 0" stroke="{INK}" stroke-width="4" stroke-linecap="round" fill="none"/>"#,
                cx0 - 7.0
            ),
            4 => format!(
                r##"<ellipse cx="{cx0}" cy="47" rx="7" ry="7" fill="#fff"/><circle cx="{}" cy="48" r="3.8" fill="{INK}"/><path d="M{} 44h16" stroke="{INK}" stroke-width="3.5" stroke-linecap="round"/>"##,
                cx0 + look,
                cx0 - 8.0
            ),
            _ => format!(
                r##"<ellipse cx="{cx0}" cy="46" rx="{}" ry="{}" fill="#fff"/><circle cx="{}" cy="47" r="4" fill="{INK}"/>"##,
                if wide { 8.5 } else { 7.0 },
                if wide { 10.0 } else { 8.5 },
                cx0 + look
            ),
        }
    };
    // `.mascot-eyes` blink: scaleY(0.1) about the eyes' centre line.
    let transform = if blink { r#" transform="translate(0 46.5) scale(1 0.1) translate(0 -46.5)""# } else { "" };
    format!("<g{transform}>{}{}</g>", eye(37.0), eye(63.0))
}

fn mouth(style: u8, state: MascotState) -> String {
    match state {
        MascotState::Done => {
            return format!(
                r#"<path d="M40 64q10 10 20 0" stroke="{INK}" stroke-width="3.5" stroke-linecap="round" fill="none"/>"#
            );
        }
        MascotState::NeedsYou => return format!(r#"<ellipse cx="50" cy="68" rx="4.5" ry="5.5" fill="{INK}"/>"#),
        _ => {}
    }
    match style {
        1 => format!(r#"<path d="M43 67h14" stroke="{INK}" stroke-width="3.2" stroke-linecap="round"/>"#),
        2 => format!(r#"<path d="M40 63h20q0 12-10 12T40 63z" fill="{INK}"/>"#),
        3 => format!(
            r#"<path d="M40 65q5 6 10 0q5 6 10 0" stroke="{INK}" stroke-width="3" stroke-linecap="round" fill="none"/>"#
        ),
        _ => format!(
            r#"<path d="M43 66q7 4 14 0" stroke="{INK}" stroke-width="3" stroke-linecap="round" fill="none" opacity="0.85"/>"#
        ),
    }
}

fn accessory(kind: Accessory, top: f32) -> String {
    let t = top - 10.0;
    match kind {
        Accessory::Hat => format!(
            r##"<g transform="translate(0 {t})"><path d="M31 24c0-17 8-24 19-24s19 7 19 24z" fill="#3a3f55"/><rect x="25" y="22" width="50" height="6" rx="3" fill="#2b2d3a"/><rect x="31" y="16" width="38" height="3.5" fill="#e58fa4"/></g>"##
        ),
        Accessory::Glasses => format!(
            r##"<g fill="none" stroke="{INK}" stroke-width="3"><circle cx="37" cy="46" r="11.5" fill="#fff" fill-opacity="0.25"/><circle cx="63" cy="46" r="11.5" fill="#fff" fill-opacity="0.25"/><path d="M48.5 46h3"/></g>"##
        ),
        Accessory::Headphones => format!(
            r##"<g transform="translate(0 {})"><path d="M15 54C15 26 30 12 50 12s35 14 35 42" fill="none" stroke="#3a3f55" stroke-width="6" stroke-linecap="round"/><rect x="8" y="46" width="12" height="22" rx="6" fill="#3a3f55"/><rect x="80" y="46" width="12" height="22" rx="6" fill="#3a3f55"/></g>"##,
            t / 2.0
        ),
        Accessory::Bow => format!(
            r##"<g transform="translate(0 {t})"><path d="M68 18l16-9v18z M68 18l-16-9v18z" fill="#e0527a"/><circle cx="68" cy="18" r="4" fill="#b83a60"/></g>"##
        ),
        Accessory::Antenna => format!(
            r##"<g transform="translate(0 {t})"><path d="M50 12V-2" stroke="#3a3f55" stroke-width="3" stroke-linecap="round"/><circle cx="50" cy="-4" r="5" fill="#f26b5b"/></g>"##
        ),
        Accessory::Crown => format!(
            r##"<g transform="translate(0 {t})"><path d="M32 22l3-16 9 8 6-14 6 14 9-8 3 16z" fill="#f2c14e" stroke="#c99a2a" stroke-width="1.5" stroke-linejoin="round"/></g>"##
        ),
        Accessory::None => String::new(),
    }
}

/// The complete SVG for one variant. The viewBox is the web's `0 -10 100 110` (with the span's −5% margin folded
/// in: the 100×100 layout box maps to `0 -5 100 100`) plus [`PAD`] on every side.
fn markup(v: Variant) -> String {
    let a = v.avatar;
    let shape = &SHAPES[(a.shape % SHAPE_COUNT) as usize];
    let state = v.state;
    // `.mascot { transform-origin: 50% 90% }` of the layout box → (50, 85) in viewBox units.
    let tilt = match state {
        MascotState::NeedsYou => 6.0,
        MascotState::Done => -3.0,
        _ => 0.0,
    };
    let ring = if state == MascotState::NeedsYou {
        format!(
            r##"<circle cx="50" cy="52" r="52" fill="none" stroke="#{:06x}" stroke-width="3.5" stroke-dasharray="6 6" opacity="0.85"/>"##,
            v.ring
        )
    } else {
        String::new()
    };
    let d = shape.d;
    let glasses = if a.accessory == Accessory::Glasses { accessory(Accessory::Glasses, shape.top) } else { String::new() };
    let outer = if a.accessory != Accessory::Glasses { accessory(a.accessory, shape.top) } else { String::new() };
    let (x0, y0, side) = (-PAD, -5.0 - PAD, 100.0 + 2.0 * PAD);
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{x0} {y0} {side} {side}" width="{side}" height="{side}"><defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity="0.28"/><stop offset="0.55" stop-color="#fff" stop-opacity="0"/><stop offset="1" stop-color="#000" stop-opacity="0.14"/></linearGradient></defs><g transform="rotate({tilt} 50 85)">{ring}<path d="{d}" fill="#{color:06x}"/><path d="{d}" fill="url(#g)"/><g transform="translate(0 {dy})">{eyes}{mouth}<circle cx="25" cy="60" r="5" fill="#fff" opacity="0.22"/><circle cx="75" cy="60" r="5" fill="#fff" opacity="0.22"/>{glasses}</g>{outer}</g></svg>"##,
        color = a.color,
        dy = shape.dy,
        eyes = eyes(a.eyes, state, v.blink),
        mouth = mouth(a.mouth, state),
    )
}

/// Rasterise to straight-alpha BGRA at `px` × `px` device pixels.
fn rasterize(v: Variant, side_px: u32) -> Option<Arc<RenderImage>> {
    let svg = markup(v);
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(side_px, side_px)?;
    let scale = side_px as f32 / tree.size().width();
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let paused = v.state == MascotState::Paused;
    let mut data = pixmap.take();
    for px in data.chunks_exact_mut(4) {
        let a = px[3];
        if a == 0 {
            continue;
        }
        // Premultiplied RGBA → straight.
        let af = a as f32 / 255.0;
        let (mut r, mut g, mut b) = (px[0] as f32 / af, px[1] as f32 / af, px[2] as f32 / af);
        let mut alpha = a as f32;
        if paused {
            // CSS `saturate(0.5)` (the filter-effects matrix at s = 0.5) and `opacity: 0.85`.
            let s = 0.5;
            let (r0, g0, b0) = (r, g, b);
            r = (0.213 + 0.787 * s) * r0 + (0.715 - 0.715 * s) * g0 + (0.072 - 0.072 * s) * b0;
            g = (0.213 - 0.213 * s) * r0 + (0.715 + 0.285 * s) * g0 + (0.072 - 0.072 * s) * b0;
            b = (0.213 - 0.213 * s) * r0 + (0.715 - 0.715 * s) * g0 + (0.072 + 0.928 * s) * b0;
            alpha *= 0.85;
        }
        let q = |c: f32| c.round().clamp(0.0, 255.0) as u8;
        // gpui's atlas wants BGRA.
        px[0] = q(b);
        px[1] = q(g);
        px[2] = q(r);
        px[3] = q(alpha);
    }
    let buffer = image::RgbaImage::from_raw(side_px, side_px, data)?;
    Some(Arc::new(RenderImage::new(SmallVec::from_buf([image::Frame::new(buffer)]))))
}

thread_local! {
    static CACHE: RefCell<HashMap<(Variant, u32), Arc<RenderImage>>> = RefCell::new(HashMap::new());
    /// Per-mascot-instance state history, for crossfading state changes: key → (state, previous, switched).
    static HISTORY: RefCell<HashMap<SharedString, (MascotState, Option<MascotState>, Instant)>> =
        RefCell::new(HashMap::new());
}

fn raster(v: Variant, side_px: u32) -> Option<Arc<RenderImage>> {
    CACHE.with(|cache| {
        if let Some(hit) = cache.borrow().get(&(v, side_px)) {
            return Some(hit.clone());
        }
        let image = rasterize(v, side_px)?;
        cache.borrow_mut().insert((v, side_px), image.clone());
        Some(image)
    })
}

/// Working bob: 2 s, ease-in-out (a cosine is the same shape as the web's keyframes).
const BOB: MotionSpec = MotionSpec::new(2000, CubicBezier::new(0.42, 0.0, 0.58, 1.0));
const BLINK_PERIOD: f32 = 6.0;
/// The closed-eye window of the web's blink (keyframes 94% → 96% → 100%).
const BLINK_CLOSED: (f32, f32) = (0.948, 0.978);
const STATE_FADE: Duration = Duration::from_millis(220);

fn clock_epoch() -> Instant {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// A mascot. `key` identifies this *instance* (e.g. `"sidebar-<bot id>"`) for state crossfades; the avatar comes
/// from [`resolve_avatar`].
#[derive(IntoElement)]
pub struct Mascot {
    key: SharedString,
    avatar: Avatar,
    state: MascotState,
    size: f32,
}

impl Mascot {
    pub fn new(key: impl Into<SharedString>, avatar: Avatar, state: MascotState, size: f32) -> Self {
        Self { key: key.into(), avatar, state, size }
    }
}

impl RenderOnce for Mascot {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let reduced = motion::reduced_motion(cx);
        let view = window.current_view();
        let scale = window.scale_factor();
        let snap = |v: f32| (v * scale).round() / scale;
        let ring = to_hex(Theme::of(cx).warn);
        let s = self.size;
        // Raster covers the padded viewBox: `side` logical px, drawn at an offset so the 100-unit box is `s`.
        let side = s * (100.0 + 2.0 * PAD) / 100.0;
        let side_px = (side * scale).round().max(1.0) as u32;
        let offset = snap(-s * PAD / 100.0);

        // Idle blink, per-teammate phase so a row of mascots never blinks in unison.
        let blink = if reduced || self.state == MascotState::Paused {
            false
        } else {
            let phase_offset = (hash(&self.key) % 1000) as f32 / 1000.0;
            let elapsed = clock_epoch().elapsed().as_secs_f32();
            let phase = (elapsed / BLINK_PERIOD + phase_offset).fract();
            let closed = phase >= BLINK_CLOSED.0 && phase < BLINK_CLOSED.1;
            let next_edge = if phase < BLINK_CLOSED.0 {
                BLINK_CLOSED.0 - phase
            } else if phase < BLINK_CLOSED.1 {
                BLINK_CLOSED.1 - phase
            } else {
                1.0 - phase + BLINK_CLOSED.0
            };
            if self.state != MascotState::Working {
                // The bob already streams frames while working; otherwise wake exactly at the next blink edge.
                anim_wake(view, Duration::from_secs_f32(next_edge * BLINK_PERIOD), cx);
            }
            closed
        };

        let bob = if self.state == MascotState::Working && !reduced {
            let phase = motion::pulse_delta(&BOB, view, cx);
            let amplitude = (s * 0.0926).min(5.0); // 5 px at the web's 54 px default
            snap(-amplitude * (0.5 - 0.5 * (phase * std::f32::consts::TAU).cos()))
        } else {
            0.0
        };

        // State crossfade bookkeeping.
        let now = Instant::now();
        let (previous, fade) = HISTORY.with(|h| {
            let mut h = h.borrow_mut();
            let entry = h.entry(self.key.clone()).or_insert((self.state, None, now - STATE_FADE));
            if entry.0 != self.state {
                *entry = (self.state, Some(entry.0), now);
            }
            let t = now.saturating_duration_since(entry.2).as_secs_f32() / STATE_FADE.as_secs_f32();
            if t >= 1.0 || reduced {
                entry.1 = None;
            }
            (entry.1, motion::EASE_OUT.eval(t.min(1.0)))
        });

        let variant = |state: MascotState, blink: bool| Variant { avatar: self.avatar, state, blink, ring };
        let layer = |image: Option<Arc<RenderImage>>, top: f32| {
            image.map(|image| {
                img(ImageSource::Render(image))
                    .absolute()
                    .left(px(offset))
                    .top(px(offset + top))
                    .size(px(side))
            })
        };
        let current = layer(raster(variant(self.state, blink), side_px), bob);
        let mut root = div().relative().flex_none().size(px(s));
        if let Some(prev) = previous {
            window.request_animation_frame();
            if let Some(old) = layer(raster(variant(prev, false), side_px), 0.0) {
                root = root.child(old.opacity(1.0 - fade));
            }
            if let Some(current) = current {
                root = root.child(current.opacity(fade));
            }
        } else if let Some(current) = current {
            root = root.child(current);
        }
        root
    }
}

fn anim_wake(view: gpui::EntityId, after: Duration, cx: &mut App) {
    crate::anim::wake_at(view, Instant::now() + after, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_avatar_matches_the_web_hash() {
        // Values computed with the web's `defaultAvatar` (Mascot.tsx) in Node.
        assert_eq!(hash("empty"), 413_646_574);
        assert_eq!((default_avatar("empty").shape, default_avatar("empty").color), (4, PALETTE[5]));
        let a = default_avatar("empty");
        assert_eq!(a.shape, (413_646_574u32 % 5) as u8);
        assert_eq!(a.color, PALETTE[((413_646_574u32 >> 3) % 6) as usize]);
    }

    #[test]
    fn partial_avatars_merge_over_the_default() {
        let stored = serde_json::json!({ "color": "#123456", "eyes": 3, "mouth": 99, "accessory": "crown" });
        let a = resolve_avatar("x", Some(&stored));
        let d = default_avatar("x");
        assert_eq!(a.color, 0x123456);
        assert_eq!(a.eyes, 3);
        assert_eq!(a.mouth, d.mouth, "out-of-range falls back");
        assert_eq!(a.accessory, Accessory::Crown);
        assert_eq!(a.shape, d.shape);
    }

    #[test]
    fn every_variant_rasterises() {
        for shape in 0..SHAPE_COUNT {
            for accessory in Accessory::ALL {
                for state in MascotState::ALL {
                    let avatar = Avatar { shape, color: PALETTE[0], eyes: shape, mouth: shape % 4, accessory };
                    let image = rasterize(Variant { avatar, state, blink: true, ring: 0x9a5b00 }, 64)
                        .unwrap_or_else(|| panic!("{shape} {accessory:?} {state:?}"));
                    let bytes = image.as_bytes(0).unwrap();
                    assert!(bytes.chunks_exact(4).any(|p| p[3] > 200), "visible pixels");
                }
            }
        }
    }
}
