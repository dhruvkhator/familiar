//! Familiar's motion language: a handful of reusable primitives built on the vendored [`crate::motion`] kit.
//!
//! | Primitive | Use | Shape |
//! |---|---|---|
//! | [`appear`] | anything that arrives (cards, toasts, empty states) | fade + 6 px rise, 420 ms expo-out |
//! | [`stagger`] | list reveal | [`appear`] with a 45 ms per-row delay, capped at 10 rows |
//! | [`spring`] | hover lift on cards, selection indicators | critically-light spring (ζ≈0.7): a hint of overshoot |
//! | [`hover`] | colour washes | zeron's 150 ms `transition-colors` fade |
//! | [`Crossfade`] | swapping route contents | old fades out fast, new fades + rises in, 240 ms |
//! | [`Expand`] | expanding rows | measured-height tween, 260 ms quint-out, then releases to auto height |
//! | [`pulse_ring`] | "working" status | LED with a soft 1.8 s expanding halo on the shared 30 fps pulse clock |
//!
//! Every primitive respects reduced motion ([`crate::motion::reduced_motion`] — the OS "Show animations" setting or
//! the user's override): oneshots snap to their end state, loops rest, springs jump to target, and no frames are
//! scheduled.
//!
//! # Frame driving
//!
//! gpui `with_animation` drives its own frames. The hand-driven primitives here (springs, hover fades, crossfades,
//! height tweens) are pure functions of wall time read during render; while one is mid-flight it calls
//! `window.request_animation_frame()`, which redraws the view that drew it next frame. Call [`frame`] once at the top of
//! each window's root render: it ticks the per-frame bookkeeping (pruning state for elements that unmounted).
//!
//! Views may be cached (`Entity::cached`): a view that isn't redrawn keeps its springs, and hover fades redraw the whole
//! window while they move (a fade can sit inside any view).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnimationElement, AnyElement, App, BoxShadow, Div, ElementId, EntityId, Global, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, Styled, Window, canvas, div, point, px,
};

use crate::motion::{self, AnimationExt as _, CubicBezier, EASE_OUT_EXPO, EASE_OUT_QUINT, MotionSpec, lerp};

/// Entrance: fade + rise.
pub const APPEAR: MotionSpec = MotionSpec::new(420, EASE_OUT_EXPO);
/// How far an entrance travels.
pub const RISE_PX: f32 = 6.0;
/// Per-row delay of a staggered reveal.
pub const STAGGER_STEP_MS: u64 = 45;
/// Rows past this one share the last delay, so long lists never feel slow.
pub const STAGGER_MAX: usize = 10;
/// Route crossfade.
pub const CROSSFADE: MotionSpec = MotionSpec::new(240, EASE_OUT_QUINT);
/// Height tween for expanding rows.
pub const EXPAND: MotionSpec = MotionSpec::new(260, EASE_OUT_QUINT);
/// "Working" halo period (the web's `pulse-led` 1.8 s).
pub const PULSE: MotionSpec = MotionSpec::new(1800, CubicBezier::new(0.0, 0.0, 0.58, 1.0));

/// Tick the per-frame bookkeeping. Call once at the top of every window's root `render`.
pub fn frame(window: &mut Window) {
    let hover_flight = motion::hover_fades_active();
    let spring_flight = SPRINGS.with(|s| s.borrow_mut().tick(Instant::now()));
    if hover_flight {
        // The fading element may be in a cached view that nothing else redraws: redraw everything next frame.
        window.on_next_frame(|window, _| window.refresh());
    } else if spring_flight {
        window.request_animation_frame();
    }
}

// -----------------------------------------------------------------------------------------------------------------
// Entrances
// -----------------------------------------------------------------------------------------------------------------

/// Fade + rise on appear. Keyed by `id`: a new id replays it (bump a generation into the id to re-run).
pub fn appear<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    rise(id, APPEAR, element)
}

/// [`appear`] with a per-index delay — the staggered list reveal.
pub fn stagger<E>(id: impl Into<ElementId>, index: usize, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    let delay = index.min(STAGGER_MAX) as u64 * STAGGER_STEP_MS;
    rise(id, APPEAR.with_delay(delay), element)
}

fn rise<E>(id: impl Into<ElementId>, spec: MotionSpec, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, spec.animation(), |el, t| el.relative().opacity(t).top(px(RISE_PX * (1.0 - t))))
}

// -----------------------------------------------------------------------------------------------------------------
// Springs
// -----------------------------------------------------------------------------------------------------------------

/// Spring constants (unit mass).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringConfig {
    pub stiffness: f32,
    pub damping: f32,
}

/// Hover lift: quick, with a whisper of overshoot (ζ ≈ 0.68).
pub const SPRING_HOVER: SpringConfig = SpringConfig { stiffness: 420.0, damping: 28.0 };
/// Selection indicators: a little softer (ζ ≈ 0.75).
pub const SPRING_SELECT: SpringConfig = SpringConfig { stiffness: 320.0, damping: 27.0 };

#[derive(Debug, Clone, Copy)]
struct SpringEntry {
    x: f32,
    v: f32,
    target: f32,
    last: Instant,
    seen: u64,
    /// The view that draws it (`EntityId::as_u64`).
    view: u64,
}

/// A spring nothing has read for this long is dropped even if its view was never drawn again.
const SPRING_FORGET: Duration = Duration::from_secs(60);

impl SpringEntry {
    fn settled(&self) -> bool {
        (self.x - self.target).abs() < 0.0015 && self.v.abs() < 0.01
    }

    /// Semi-implicit Euler in ≤4 ms substeps — stable for these stiffnesses at any frame rate.
    fn advance(&mut self, now: Instant, cfg: SpringConfig) {
        let mut dt = now.saturating_duration_since(self.last).as_secs_f32().min(0.1) / motion::speed_scale();
        self.last = now;
        while dt > 0.0 {
            let h = dt.min(0.004);
            let a = -cfg.stiffness * (self.x - self.target) - cfg.damping * self.v;
            self.v += a * h;
            self.x += self.v * h;
            dt -= h;
        }
        if self.settled() {
            self.x = self.target;
            self.v = 0.0;
        }
    }
}

#[derive(Default)]
struct SpringStore {
    entries: HashMap<SharedString, SpringEntry>,
    frame: u64,
    /// Views that read springs this frame, and in the last one.
    views: HashSet<u64>,
    last_views: HashSet<u64>,
}

impl SpringStore {
    fn value(&mut self, key: &SharedString, target: f32, cfg: SpringConfig, reduced: bool, now: Instant, view: u64) -> (f32, bool) {
        let frame = self.frame;
        self.views.insert(view);
        let entry = self
            .entries
            .entry(key.clone())
            // First sight starts at rest on its target: mounting never animates, only changes do.
            .or_insert(SpringEntry { x: target, v: 0.0, target, last: now, seen: frame, view });
        entry.seen = frame;
        entry.view = view;
        // A spring at rest has nothing to catch up on: after frames unread (its view cached) it moves one frame's worth,
        // not the whole gap.
        if entry.settled()
            && let Some(frame_ago) = now.checked_sub(Duration::from_millis(16))
        {
            entry.last = entry.last.max(frame_ago);
        }
        entry.target = target;
        if reduced {
            entry.x = target;
            entry.v = 0.0;
            entry.last = now;
        } else {
            entry.advance(now, cfg);
        }
        (entry.x, !entry.settled())
    }

    /// A new frame. A spring missing from the last frame is gone when its view drew that frame without it; a view
    /// that drew nothing (cached) keeps its springs, until they go unread for [`SPRING_FORGET`].
    fn tick(&mut self, now: Instant) -> bool {
        self.frame += 1;
        let frame = self.frame;
        self.last_views = std::mem::take(&mut self.views);
        let drawn = &self.last_views;
        let mut active = false;
        self.entries.retain(|_, e| {
            let missed = e.seen + 1 < frame;
            if missed && (drawn.contains(&e.view) || now.saturating_duration_since(e.last) > SPRING_FORGET) {
                return false; // unmounted
            }
            active |= !missed && !e.settled();
            true
        });
        active
    }
}

thread_local! {
    static SPRINGS: RefCell<SpringStore> = RefCell::new(SpringStore::default());
}

/// The current value of the spring behind `key`, chasing `target`. Requests frames while it moves.
pub fn spring(key: impl Into<SharedString>, target: f32, cfg: SpringConfig, window: &mut Window, cx: &App) -> f32 {
    let key = key.into();
    let reduced = motion::reduced_motion(cx);
    let view = window.current_view().as_u64();
    let (x, moving) = SPRINGS.with(|s| s.borrow_mut().value(&key, target, cfg, reduced, Instant::now(), view));
    if moving {
        window.request_animation_frame();
    }
    x
}

/// Hover progress (0..1) for `key` (pair with [`crate::motion::hover_listener`] on the same key). Requests frames
/// while the fade is mid-flight.
pub fn hover(key: &str, window: &mut Window) -> f32 {
    let t = motion::hover_t(key);
    if t > 0.0 && t < 1.0 {
        window.request_animation_frame();
    }
    t
}

/// Blend two colours by a hover/spring progress (premultiplied, like the browser).
pub fn blend(from: Hsla, to: Hsla, t: f32) -> Hsla {
    motion::mix(from, to, t)
}

// -----------------------------------------------------------------------------------------------------------------
// Crossfade
// -----------------------------------------------------------------------------------------------------------------

/// Crossfade between route contents. Keep one in the view; [`Crossfade::set`] on navigation, [`Crossfade::render`]
/// in `render`.
pub struct Crossfade<K> {
    current: K,
    previous: Option<K>,
    switched: Instant,
}

impl<K: Clone + PartialEq + Debug> Crossfade<K> {
    pub fn new(current: K) -> Self {
        Self { current, previous: None, switched: Instant::now() }
    }

    pub fn current(&self) -> &K {
        &self.current
    }

    /// No transition is running: only the current content is drawn, at full opacity.
    pub fn settled(&self, reduced: bool) -> bool {
        reduced || self.previous.is_none() || self.switched.elapsed() >= CROSSFADE.total().mul_f32(motion::speed_scale())
    }

    /// Navigate. Returns whether the key changed.
    pub fn set(&mut self, key: K) -> bool {
        if key == self.current {
            return false;
        }
        self.previous = Some(std::mem::replace(&mut self.current, key));
        self.switched = Instant::now();
        true
    }

    /// Render the current content (and, mid-transition, the outgoing one beneath it). Generic over the context so
    /// a view can build pages with its own `Context<V>`; pass [`crate::motion::reduced_motion`] as `reduced`.
    pub fn render<C>(
        &mut self,
        id: impl Into<ElementId>,
        reduced: bool,
        window: &mut Window,
        cx: &mut C,
        mut build: impl FnMut(&K, &mut Window, &mut C) -> AnyElement,
    ) -> AnyElement {
        let id = id.into();
        let raw = self.switched.elapsed().as_secs_f32() / CROSSFADE.total().mul_f32(motion::speed_scale()).as_secs_f32();
        if raw >= 1.0 || reduced {
            self.previous = None;
        }
        let key_id = |k: &K| ElementId::Name(format!("{k:?}").into());
        let current = div().id(key_id(&self.current)).size_full().child(build(&self.current, window, cx));
        let Some(previous) = self.previous.clone() else {
            return div().id(id).size_full().child(current).into_any_element();
        };
        window.request_animation_frame();
        let t_in = CROSSFADE.curve.eval(raw);
        // The outgoing page gets out of the way in the first 60% of the timeline.
        let t_out = motion::EASE_OUT.eval((raw / 0.6).min(1.0));
        let old = div()
            .id(key_id(&previous))
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .opacity(1.0 - t_out)
            .child(build(&previous, window, cx));
        div()
            .id(id)
            .relative()
            .size_full()
            .child(old)
            .child(current.relative().opacity(t_in).top(px(RISE_PX * 0.66 * (1.0 - t_in))))
            .into_any_element()
    }
}

// -----------------------------------------------------------------------------------------------------------------
// Height tween
// -----------------------------------------------------------------------------------------------------------------

/// Smooth height for expanding rows. The content is always laid out (and measured by a zero-cost canvas probe), so
/// even the first open animates to the right height; once settled open, the wrapper releases to auto height so
/// content changes are never clipped.
pub struct Expand {
    open: bool,
    from: f32,
    changed: Option<Instant>,
    measured: Rc<Cell<f32>>,
}

impl Expand {
    pub fn new(open: bool) -> Self {
        Self { open, from: 0.0, changed: None, measured: Rc::new(Cell::new(0.0)) }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    fn progress(&self) -> f32 {
        match self.changed {
            None => 1.0,
            Some(at) => {
                let raw = at.elapsed().as_secs_f32() / EXPAND.total().mul_f32(motion::speed_scale()).as_secs_f32();
                EXPAND.curve.eval(raw.min(1.0))
            }
        }
    }

    fn height(&self) -> f32 {
        let to = if self.open { self.measured.get() } else { 0.0 };
        lerp(self.from, to, self.progress())
    }

    pub fn set_open(&mut self, open: bool) {
        if open == self.open {
            return;
        }
        self.from = self.height();
        self.open = open;
        self.changed = Some(Instant::now());
    }

    pub fn toggle(&mut self) {
        self.set_open(!self.open);
    }

    /// Wrap `content`. Rotate a chevron with [`Expand::openness`].
    pub fn render(&self, id: impl Into<ElementId>, window: &mut Window, cx: &App, content: impl IntoElement) -> Div {
        let measured = self.measured.clone();
        let probe = canvas(move |bounds, _, _| measured.set(f32::from(bounds.size.height)), |_, _, _, _| {})
            .absolute()
            .top_0()
            .left_0()
            .size_full();
        let inner = div().id(id).relative().w_full().flex_none().child(content).child(probe);
        let p = self.progress();
        let settled = p >= 1.0 || motion::reduced_motion(cx);
        if settled {
            return if self.open {
                div().w_full().child(inner)
            } else {
                div().w_full().h(px(0.0)).overflow_hidden().child(inner)
            };
        }
        window.request_animation_frame();
        let fade = if self.open { p } else { 1.0 - p };
        div().w_full().h(px(self.height())).overflow_hidden().child(inner.opacity(0.25 + 0.75 * fade))
    }

    /// 0 closed … 1 open, eased — for chevrons and indicators that follow the row.
    pub fn openness(&self) -> f32 {
        let p = self.progress();
        if self.open { p } else { 1.0 - p }
    }
}

// -----------------------------------------------------------------------------------------------------------------
// Pulses
// -----------------------------------------------------------------------------------------------------------------

/// The "working" LED: a dot with a soft halo that expands and fades (the web's `pulse-led`), driven by the shared
/// 30 fps pulse clock so a sidebar full of working teammates costs one redraw stream, not one per dot.
pub fn pulse_ring(color: Hsla, size: f32, window: &mut Window, cx: &mut App) -> Div {
    let view = window.current_view();
    let phase = motion::pulse_delta(&PULSE, view, cx);
    let reduced = motion::reduced_motion(cx);
    let t = PULSE.curve.eval(phase);
    let halo = if reduced {
        vec![]
    } else {
        vec![BoxShadow {
            color: color.opacity(0.5 * (1.0 - t)),
            offset: point(px(0.0), px(0.0)),
            blur_radius: px(0.0),
            spread_radius: px(size * 0.55 * t),
            inset: false,
        }]
    };
    div().flex_none().size(px(size)).rounded_full().bg(color).shadow(halo)
}

/// A repeating phase (0..1) of `spec` on the pulse clock for the current view; 0 under reduced motion.
pub fn loop_phase(spec: &MotionSpec, window: &mut Window, cx: &mut App) -> f32 {
    motion::pulse_delta(spec, window.current_view(), cx)
}

// -----------------------------------------------------------------------------------------------------------------
// Wake-ups
// -----------------------------------------------------------------------------------------------------------------

#[derive(Default)]
struct Wakes(HashMap<EntityId, Instant>);

impl Global for Wakes {}

/// Re-render `view` at `at` (once), without holding a frame stream open until then — for sparse motion like the
/// mascot's idle blink. Coalesces to the earliest pending wake per view.
pub fn wake_at(view: EntityId, at: Instant, cx: &mut App) {
    if motion::reduced_motion(cx) {
        return;
    }
    let now = Instant::now();
    let wakes = cx.default_global::<Wakes>();
    if let Some(pending) = wakes.0.get(&view)
        && *pending > now
        && *pending <= at
    {
        return;
    }
    wakes.0.insert(view, at);
    let delay = at.saturating_duration_since(now) + Duration::from_millis(1);
    cx.spawn(async move |cx| {
        cx.background_executor().timer(delay).await;
        cx.update(|cx| {
            let wakes = cx.default_global::<Wakes>();
            if wakes.0.get(&view) == Some(&at) {
                wakes.0.remove(&view);
            }
            cx.notify(view);
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn springs_start_at_rest_and_settle_on_target() {
        let mut store = SpringStore::default();
        let key: SharedString = "card".into();
        let t0 = Instant::now();
        let (x, moving) = store.value(&key, 0.0, SPRING_HOVER, false, t0, 1);
        assert_eq!((x, moving), (0.0, false), "mounting does not animate");
        let (x, moving) = store.value(&key, 1.0, SPRING_HOVER, false, t0 + Duration::from_millis(16), 1);
        assert!(moving && x > 0.0 && x < 1.0, "moving toward target: {x}");
        let mut peak: f32 = 0.0;
        let mut t = t0 + Duration::from_millis(16);
        for _ in 0..120 {
            t += Duration::from_millis(16);
            let (x, _) = store.value(&key, 1.0, SPRING_HOVER, false, t, 1);
            peak = peak.max(x);
        }
        let (x, moving) = store.value(&key, 1.0, SPRING_HOVER, false, t + Duration::from_millis(16), 1);
        assert_eq!((x, moving), (1.0, false), "settles exactly");
        assert!(peak > 1.0 && peak < 1.08, "a whisper of overshoot, not a wobble: {peak}");
    }

    #[test]
    fn reduced_motion_springs_jump() {
        let mut store = SpringStore::default();
        let key: SharedString = "row".into();
        let t0 = Instant::now();
        store.value(&key, 0.0, SPRING_SELECT, true, t0, 1);
        assert_eq!(store.value(&key, 1.0, SPRING_SELECT, true, t0, 1), (1.0, false));
    }

    #[test]
    fn springs_unmounted_from_a_drawn_view_are_pruned() {
        let mut store = SpringStore::default();
        let (gone, kept): (SharedString, SharedString) = ("gone".into(), "kept".into());
        let now = Instant::now();
        store.tick(now);
        store.value(&gone, 0.0, SPRING_HOVER, false, now, 7);
        store.value(&kept, 0.0, SPRING_HOVER, false, now, 7);
        // The view draws again with only one of them.
        for _ in 0..2 {
            store.tick(now);
            store.value(&kept, 0.0, SPRING_HOVER, false, now, 7);
        }
        store.tick(now);
        assert!(!store.entries.contains_key(&gone));
        assert!(store.entries.contains_key(&kept));
    }

    #[test]
    fn springs_of_an_undrawn_view_are_kept_and_still_animate() {
        let mut store = SpringStore::default();
        let key: SharedString = "tab/pos".into();
        let other: SharedString = "other".into();
        let t0 = Instant::now();
        store.tick(t0);
        store.value(&key, 0.0, SPRING_SELECT, false, t0, 3);
        // The view is cached for many frames while another one draws.
        for i in 1..50 {
            store.tick(t0 + Duration::from_millis(16 * i));
            store.value(&other, 0.0, SPRING_SELECT, false, t0, 4);
        }
        assert!(store.entries.contains_key(&key));
        // Its target moves: it springs from where it was rather than starting on the new target.
        let t = t0 + Duration::from_millis(16 * 51);
        store.tick(t);
        let (x, moving) = store.value(&key, 1.0, SPRING_SELECT, false, t, 3);
        assert!(moving && x < 0.5, "animates from 0: {x}");
        // Springs left unread for long go.
        let later = t + SPRING_FORGET + Duration::from_secs(1);
        store.tick(later);
        store.tick(later);
        assert!(!store.entries.contains_key(&key));
    }

    #[test]
    fn expand_interpolates_from_the_current_height() {
        let mut e = Expand::new(false);
        e.measured.set(120.0);
        assert_eq!(e.height(), 0.0);
        e.set_open(true);
        assert!(e.height() < 120.0);
        e.changed = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(e.height(), 120.0);
        e.set_open(false);
        assert!(e.height() > 0.0, "closing starts from where it is");
    }
}
