//! The component kit: Familiar's web primitives (`components/ui.tsx`, `Shell.tsx`) as GPUI elements.
//!
//! Every interactive piece animates its state changes — colour washes fade (zeron's 150 ms `transition-colors`),
//! lifts and selection indicators ride springs — and all of it collapses to instant changes under reduced motion.

use std::rc::Rc;

use gpui::{
    AnyElement, AnyView, App, AppContext as _, ClickEvent, Context, ElementId, FontWeight, Hsla, InteractiveElement,
    IntoElement, ParentElement, Render, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div,
    linear_color_stop, linear_gradient, prelude::FluentBuilder as _, px, relative,
};
use smallvec::SmallVec;

use crate::anim::{self, SPRING_HOVER, SPRING_SELECT};
use crate::icons::icon;
use crate::mascot::{Avatar, Mascot, MascotState};
use crate::motion::{self, MotionSpec};
use crate::theme::{RADIUS_CARD, RADIUS_CHIP, RADIUS_CONTROL, Theme, Tone, text};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

fn key_of(id: &ElementId) -> SharedString {
    format!("{id}").into()
}

// -----------------------------------------------------------------------------------------------------------------
// Button
// -----------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonVariant {
    /// Filled accent — one per view.
    Primary,
    /// Surface with a hairline (the web's `default` tone).
    Secondary,
    /// Transparent until hovered.
    Ghost,
    /// Destructive: surface with a `bad` outline.
    Danger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonSize {
    Small,
    Medium,
    Large,
}

/// A button with a fading hover wash and a pressed nudge.
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: Option<SharedString>,
    icon: Option<&'static str>,
    variant: ButtonVariant,
    size: ButtonSize,
    disabled: bool,
    full_width: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: Some(label.into()),
            icon: None,
            variant: ButtonVariant::Secondary,
            size: ButtonSize::Medium,
            disabled: false,
            full_width: false,
            tooltip: None,
            on_click: None,
        }
    }

    /// An icon-only button (give it a [`Button::tooltip`]).
    pub fn icon_only(id: impl Into<ElementId>, path: &'static str) -> Self {
        Self { label: None, icon: Some(path), variant: ButtonVariant::Ghost, ..Self::new(id, "") }
    }

    pub fn primary(mut self) -> Self {
        self.variant = ButtonVariant::Primary;
        self
    }
    pub fn secondary(mut self) -> Self {
        self.variant = ButtonVariant::Secondary;
        self
    }
    pub fn ghost(mut self) -> Self {
        self.variant = ButtonVariant::Ghost;
        self
    }
    pub fn danger(mut self) -> Self {
        self.variant = ButtonVariant::Danger;
        self
    }
    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }
    pub fn icon(mut self, path: &'static str) -> Self {
        self.icon = Some(path);
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }
    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let key = key_of(&self.id);
        let t = if self.disabled { 0.0 } else { anim::hover(&key, window) };
        let (bg, bg_hover, bg_pressed, border, fg, fg_hover) = match self.variant {
            ButtonVariant::Primary => (
                theme.accent,
                theme.accent_hover,
                theme.accent_pressed,
                theme.accent.opacity(0.0),
                theme.accent_ink,
                theme.accent_ink,
            ),
            ButtonVariant::Secondary => {
                (theme.surface, theme.hover, theme.pressed, theme.line, theme.ink, theme.ink)
            }
            ButtonVariant::Ghost => {
                (theme.hover.opacity(0.0), theme.hover, theme.pressed, theme.line.opacity(0.0), theme.muted, theme.ink)
            }
            ButtonVariant::Danger => (theme.surface, theme.bad_soft, theme.bad_soft, theme.bad, theme.bad, theme.bad),
        };
        let (height, pad, size, icon_size) = match self.size {
            ButtonSize::Small => (28.0, 10.0, text::CAPTION, 14.0),
            ButtonSize::Medium => (34.0, 12.0, text::SMALL, 16.0),
            ButtonSize::Large => (44.0, 20.0, text::LEAD, 18.0),
        };
        let fg_now = anim::blend(fg, fg_hover, t);
        let icon_only = self.label.is_none();
        let on_click = self.on_click.clone();
        let tooltip = self.tooltip.clone();
        div()
            .id(self.id.clone())
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .h(px(height))
            .when(icon_only, |b| b.w(px(height)))
            .when(!icon_only, |b| b.px(px(pad)))
            .when(self.full_width, |b| b.w_full())
            .rounded(px(RADIUS_CONTROL))
            .border_1()
            .border_color(border)
            .bg(anim::blend(bg, bg_hover, t))
            .text_color(fg_now)
            .text_size(px(size))
            .font_weight(FontWeight::MEDIUM)
            .when(self.variant == ButtonVariant::Primary, |b| b.shadow(theme.card_shadow(-0.4 + 0.4 * t)))
            .when(self.disabled, |b| b.opacity(0.45))
            .when(!self.disabled, |b| {
                b.cursor_pointer()
                    .on_hover(motion::hover_listener(key.clone()))
                    .active(move |s| s.bg(bg_pressed).top(px(0.5)))
            })
            .when_some(on_click.filter(|_| !self.disabled), |b, handler| {
                b.on_click(move |ev, window, cx| handler(ev, window, cx))
            })
            .when_some(tooltip, |b, text| b.tooltip(tooltip_text(text)))
            .when_some(self.icon, |b, path| b.child(icon(path).size(px(icon_size)).text_color(fg_now)))
            .when_some(self.label, |b, label| b.child(label))
    }
}

// -----------------------------------------------------------------------------------------------------------------
// Cards
// -----------------------------------------------------------------------------------------------------------------

/// A resting card: surface, hairline, 14 px radius, the web's soft shadow.
pub fn card(cx: &App) -> gpui::Div {
    let theme = Theme::of(cx);
    div()
        .bg(theme.surface)
        .border_1()
        .border_color(theme.line)
        .rounded(px(RADIUS_CARD))
        .shadow(theme.card_shadow(0.0))
}

/// A card that lifts on hover: rises 2 px on a spring while its shadow deepens.
#[derive(IntoElement)]
pub struct HoverCard {
    id: ElementId,
    children: SmallVec<[AnyElement; 2]>,
    on_click: Option<ClickHandler>,
    padding: f32,
    flat: bool,
}

impl HoverCard {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self { id: id.into(), children: SmallVec::new(), on_click: None, padding: 16.0, flat: false }
    }
    pub fn padding(mut self, padding: f32) -> Self {
        self.padding = padding;
        self
    }
    /// No resting border/shadow — a transparent tile that only gains a surface on hover (the teammate strip).
    pub fn flat(mut self) -> Self {
        self.flat = true;
        self
    }
    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl ParentElement for HoverCard {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for HoverCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let key = key_of(&self.id);
        let hovered = motion::hover_t(&key) > 0.5 || is_hover_target(&key);
        let lift = anim::spring(format!("{key}/lift"), if hovered { 1.0 } else { 0.0 }, SPRING_HOVER, window, cx);
        let wash = anim::hover(&key, window);
        let (bg, border, shadow) = if self.flat {
            (
                anim::blend(theme.surface.opacity(0.0), theme.surface, wash),
                anim::blend(theme.line.opacity(0.0), theme.line, wash),
                theme.card_shadow(lift).into_iter().map(|mut s| {
                    s.color = s.color.opacity(s.color.a * lift.clamp(0.0, 1.0));
                    s
                }).collect(),
            )
        } else {
            (theme.surface, anim::blend(theme.line, theme.line, wash), theme.card_shadow(lift))
        };
        let on_click = self.on_click.clone();
        let key_for_hover = key.clone();
        div()
            .id(self.id)
            .relative()
            .top(px(-2.0 * lift))
            .rounded(px(RADIUS_CARD))
            .border_1()
            .border_color(border)
            .bg(bg)
            .shadow(shadow)
            .p(px(self.padding))
            .on_hover(move |hovered, window, cx| {
                set_hover_target(&key_for_hover, *hovered);
                motion::hover_listener(key_for_hover.clone())(hovered, window, cx)
            })
            .when_some(on_click, |el, handler| {
                el.cursor_pointer().on_click(move |ev, window, cx| handler(ev, window, cx))
            })
            .children(self.children)
    }
}

thread_local! {
    /// Which hover cards the pointer is over right now (the hover fade's *target*, independent of its progress).
    static HOVER_TARGETS: std::cell::RefCell<std::collections::HashSet<SharedString>> = Default::default();
}

fn set_hover_target(key: &SharedString, hovered: bool) {
    HOVER_TARGETS.with(|t| {
        let mut t = t.borrow_mut();
        if hovered {
            t.insert(key.clone());
        } else {
            t.remove(key);
        }
    });
}

fn is_hover_target(key: &SharedString) -> bool {
    HOVER_TARGETS.with(|t| t.borrow().contains(key))
}

// -----------------------------------------------------------------------------------------------------------------
// Chips, badges, LEDs
// -----------------------------------------------------------------------------------------------------------------

/// The web's `Chip`: a small tinted label.
pub fn chip(tone: Tone, label: impl Into<SharedString>, cx: &App) -> gpui::Div {
    let (fg, bg) = Theme::of(cx).tone(tone);
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.0))
        .h(px(20.0))
        .px(px(7.0))
        .rounded(px(RADIUS_CHIP))
        .bg(bg)
        .text_color(fg)
        .text_size(px(text::CAPTION))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .child(label.into())
}

/// The web's `Badge`: a count pill in `warn` (hidden at zero).
pub fn badge(n: usize, cx: &App) -> Option<gpui::Div> {
    if n == 0 {
        return None;
    }
    let theme = Theme::of(cx);
    Some(
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .min_w(px(20.0))
            .h(px(20.0))
            .px(px(6.0))
            .rounded_full()
            .bg(theme.warn)
            .text_color(theme.warn_ink)
            .text_size(px(text::MICRO))
            .font_weight(FontWeight::SEMIBOLD)
            .child(SharedString::from(n.to_string())),
    )
}

/// Run status (the web's `RunStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Queued,
    Running,
    WaitingApproval,
    Succeeded,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingApproval => "waiting approval",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn tone(self) -> Tone {
        match self {
            Self::Running => Tone::Accent,
            Self::WaitingApproval => Tone::Warn,
            Self::Succeeded => Tone::Ok,
            Self::Failed => Tone::Bad,
            Self::Queued | Self::Cancelled => Tone::Muted,
        }
    }
}

/// The web's `RunChip`; a running chip carries the pulsing LED.
#[derive(IntoElement)]
pub struct StatusChip {
    status: RunStatus,
}

impl StatusChip {
    pub fn new(status: RunStatus) -> Self {
        Self { status }
    }
}

impl RenderOnce for StatusChip {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tone = self.status.tone();
        let (fg, _) = Theme::of(cx).tone(tone);
        let chip = chip(tone, self.status.label(), cx);
        if self.status == RunStatus::Running {
            let dot = anim::pulse_ring(fg, 6.0, window, cx);
            // Chip children are [label]; put the dot first.
            div().child(chip.child(dot).flex_row_reverse())
        } else {
            div().child(chip)
        }
    }
}

/// Machine/teammate status LED (the web's `Led`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedStatus {
    Idle,
    Running,
    Paused,
    Online,
    Offline,
}

#[derive(IntoElement)]
pub struct Led {
    status: LedStatus,
    size: f32,
}

impl Led {
    pub fn new(status: LedStatus) -> Self {
        Self { status, size: 9.0 }
    }
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for Led {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let s = self.size;
        match self.status {
            LedStatus::Running => anim::pulse_ring(theme.accent, s, window, cx),
            LedStatus::Idle => div().flex_none().size(px(s)).rounded_full().border_1().border_color(theme.muted.opacity(0.6)),
            LedStatus::Paused => div().flex_none().size(px(s)).rounded_full().bg(theme.warn),
            LedStatus::Online => div().flex_none().size(px(s)).rounded_full().bg(theme.ok),
            LedStatus::Offline => div().flex_none().size(px(s)).rounded_full().bg(theme.bad),
        }
    }
}

// -----------------------------------------------------------------------------------------------------------------
// Avatar + name row
// -----------------------------------------------------------------------------------------------------------------

/// Mascot + name + status line (the web's `BotNavItem` body).
#[derive(IntoElement)]
pub struct AvatarRow {
    key: SharedString,
    avatar: Avatar,
    name: SharedString,
    state: MascotState,
    size: f32,
    detail: Option<SharedString>,
}

impl AvatarRow {
    pub fn new(key: impl Into<SharedString>, avatar: Avatar, name: impl Into<SharedString>, state: MascotState) -> Self {
        Self { key: key.into(), avatar, name: name.into(), state, size: 30.0, detail: None }
    }
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
    /// Replace the status label with custom detail text.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

impl RenderOnce for AvatarRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let status_color = if self.state == MascotState::NeedsYou { theme.warn } else { theme.muted };
        div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .min_w_0()
            .child(Mascot::new(self.key, self.avatar, self.state, self.size))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(text::SMALL))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.ink)
                            .truncate()
                            .child(self.name),
                    )
                    .child(
                        div()
                            .text_size(px(text::CAPTION))
                            .text_color(status_color)
                            .truncate()
                            .child(self.detail.unwrap_or_else(|| self.state.label().into())),
                    ),
            )
    }
}

// -----------------------------------------------------------------------------------------------------------------
// Sidebar item
// -----------------------------------------------------------------------------------------------------------------

/// A sidebar row: icon (or custom leading element like a mascot) + label, with a selected state whose wash and
/// accent indicator spring in, and a fading hover wash.
#[derive(IntoElement)]
pub struct SidebarItem {
    id: ElementId,
    icon: Option<&'static str>,
    leading: Option<AnyElement>,
    label: SharedString,
    sublabel: Option<(SharedString, Option<Hsla>)>,
    selected: bool,
    badge: usize,
    on_click: Option<ClickHandler>,
}

impl SidebarItem {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            icon: None,
            leading: None,
            label: label.into(),
            sublabel: None,
            selected: false,
            badge: 0,
            on_click: None,
        }
    }
    pub fn icon(mut self, path: &'static str) -> Self {
        self.icon = Some(path);
        self
    }
    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self
    }
    pub fn sublabel(mut self, text: impl Into<SharedString>, color: Option<Hsla>) -> Self {
        self.sublabel = Some((text.into(), color));
        self
    }
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    pub fn badge(mut self, n: usize) -> Self {
        self.badge = n;
        self
    }
    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for SidebarItem {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let key = key_of(&self.id);
        let h = anim::hover(&key, window);
        let s = anim::spring(format!("{key}/sel"), if self.selected { 1.0 } else { 0.0 }, SPRING_SELECT, window, cx);
        let sc = s.clamp(0.0, 1.0);
        let rest = theme.hover.opacity(0.0);
        let bg = anim::blend(anim::blend(rest, theme.hover, h), theme.accent_soft, sc);
        let fg = anim::blend(anim::blend(theme.muted, theme.ink, h), theme.accent, sc);
        let tall = self.sublabel.is_some();
        let row_h = if tall { 48.0 } else { 36.0 };
        let indicator_h = 16.0 * s.max(0.0);
        let on_click = self.on_click.clone();
        let label_color = if self.leading.is_some() { anim::blend(theme.ink, theme.accent, sc) } else { fg };
        div()
            .id(self.id)
            .relative()
            .flex()
            .items_center()
            .gap(px(10.0))
            .h(px(row_h))
            .px(px(if self.leading.is_some() { 8.0 } else { 12.0 }))
            .rounded(px(RADIUS_CONTROL))
            .bg(bg)
            .cursor_pointer()
            .on_hover(motion::hover_listener(key.clone()))
            .when_some(on_click, |el, handler| el.on_click(move |ev, window, cx| handler(ev, window, cx)))
            // The indicator: a 3 px accent pill that grows from the row's centre line.
            .child(
                div()
                    .absolute()
                    .left(px(-6.0))
                    .top(px((row_h - indicator_h) / 2.0))
                    .w(px(3.0))
                    .h(px(indicator_h))
                    .rounded_full()
                    .bg(theme.accent)
                    .opacity(sc),
            )
            .when_some(self.icon, |el, path| el.child(icon(path).size(px(18.0)).text_color(fg)))
            .when_some(self.leading, |el, leading| el.child(leading))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(text::SMALL))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(label_color)
                            .truncate()
                            .child(self.label),
                    )
                    .when_some(self.sublabel, |el, (sub, color)| {
                        el.child(
                            div()
                                .text_size(px(text::CAPTION))
                                .text_color(color.unwrap_or(theme.muted))
                                .truncate()
                                .child(sub),
                        )
                    }),
            )
            .when_some(badge(self.badge, cx), |el, b| el.child(b))
    }
}

// -----------------------------------------------------------------------------------------------------------------
// Section header
// -----------------------------------------------------------------------------------------------------------------

/// A section title (the web's `h2.text-lg.font-semibold`), with an optional count and trailing action.
#[derive(IntoElement)]
pub struct SectionHeader {
    title: SharedString,
    count: Option<usize>,
    action: Option<AnyElement>,
}

impl SectionHeader {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self { title: title.into(), count: None, action: None }
    }
    pub fn count(mut self, n: usize) -> Self {
        self.count = Some(n);
        self
    }
    pub fn action(mut self, element: impl IntoElement) -> Self {
        self.action = Some(element.into_any_element());
        self
    }
}

impl RenderOnce for SectionHeader {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(text::TITLE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.ink)
                    .child(self.title),
            )
            .when_some(self.count, |el, n| {
                el.child(div().text_size(px(text::SMALL)).text_color(theme.muted).child(SharedString::from(n.to_string())))
            })
            .child(div().flex_1())
            .when_some(self.action, |el, a| el.child(a))
    }
}

/// The small muted group label in the sidebar ("Teammates", "Recent chats").
pub fn group_label(label: impl Into<SharedString>, cx: &App) -> gpui::Div {
    div()
        .px(px(12.0))
        .text_size(px(text::CAPTION))
        .font_weight(FontWeight::MEDIUM)
        .text_color(Theme::of(cx).muted)
        .child(label.into())
}

// -----------------------------------------------------------------------------------------------------------------
// Skeleton
// -----------------------------------------------------------------------------------------------------------------

/// Shimmer sweep period.
pub const SHIMMER: MotionSpec = MotionSpec::new(1600, motion::EASE_IN_OUT);

/// A loading placeholder: a sunken block with a soft highlight sweeping across it.
#[derive(IntoElement)]
pub struct Skeleton {
    width: Option<f32>,
    height: f32,
    radius: f32,
}

impl Skeleton {
    pub fn new(height: f32) -> Self {
        Self { width: None, height, radius: 8.0 }
    }
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }
    pub fn radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self
    }
}

impl RenderOnce for Skeleton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let phase = anim::loop_phase(&SHIMMER, window, cx);
        let reduced = motion::reduced_motion(cx);
        // The band travels −45% → 145% of the width; a short rest at each end keeps it calm.
        let x = motion::lerp(-0.45, 1.45, motion::EASE_IN_OUT.eval((phase * 1.25).min(1.0)));
        let band = |from: Hsla, to: Hsla| linear_gradient(90.0, linear_color_stop(from, 0.0), linear_color_stop(to, 1.0));
        let clear = theme.shimmer.opacity(0.0);
        div()
            .relative()
            .overflow_hidden()
            .h(px(self.height))
            .map(|el| match self.width {
                Some(w) => el.w(px(w)),
                None => el.w_full(),
            })
            .rounded(px(self.radius))
            .bg(theme.sunken)
            .when(!reduced, |el| {
                el.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(relative(x))
                        .w(relative(0.45))
                        .flex()
                        .child(div().h_full().w(relative(0.5)).bg(band(clear, theme.shimmer)))
                        .child(div().h_full().w(relative(0.5)).bg(band(theme.shimmer, clear))),
                )
            })
    }
}

// -----------------------------------------------------------------------------------------------------------------
// Tooltip
// -----------------------------------------------------------------------------------------------------------------

/// A themed text tooltip for gpui's `.tooltip(...)`.
pub fn tooltip_text(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| TooltipView { text: text.clone() }).into()
}

pub struct TooltipView {
    text: SharedString,
}

impl Render for TooltipView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        // gpui positions the tooltip; we only style it and fade it in.
        div().pl(px(4.0)).pt(px(6.0)).child(motion::fade_quick(
            "tooltip",
            div()
                .px(px(9.0))
                .py(px(5.0))
                .rounded(px(8.0))
                .bg(theme.tooltip_bg)
                .text_color(theme.tooltip_ink)
                .text_size(px(text::CAPTION))
                .font_family(theme.font_sans.clone())
                .shadow(theme.float_shadow())
                .child(self.text.clone()),
        ))
    }
}

// -----------------------------------------------------------------------------------------------------------------
// Segmented control + switch
// -----------------------------------------------------------------------------------------------------------------

/// A segmented control whose selection pill slides between options on a spring.
#[derive(IntoElement)]
pub struct Segmented {
    id: ElementId,
    options: Vec<(SharedString, Option<&'static str>)>,
    selected: usize,
    segment_width: f32,
    on_select: Option<Rc<dyn Fn(usize, &mut Window, &mut App)>>,
}

impl Segmented {
    pub fn new(id: impl Into<ElementId>, options: Vec<(SharedString, Option<&'static str>)>, selected: usize) -> Self {
        Self { id: id.into(), options, selected, segment_width: 78.0, on_select: None }
    }
    pub fn segment_width(mut self, w: f32) -> Self {
        self.segment_width = w;
        self
    }
    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Segmented {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let key = key_of(&self.id);
        let pos = anim::spring(format!("{key}/pos"), self.selected as f32, SPRING_SELECT, window, cx);
        let w = self.segment_width;
        let inset = 3.0;
        let mut root = div()
            .id(self.id)
            .relative()
            .flex()
            .flex_none()
            .h(px(32.0))
            .p(px(inset))
            .rounded(px(RADIUS_CONTROL))
            .bg(theme.sunken)
            .border_1()
            .border_color(theme.line)
            .child(
                div()
                    .absolute()
                    .top(px(inset - 1.0))
                    .left(px(inset - 1.0 + pos * w))
                    .w(px(w))
                    .h(px(32.0 - 2.0 * inset))
                    .rounded(px(RADIUS_CONTROL - 3.0))
                    .bg(theme.surface)
                    .shadow(theme.card_shadow(0.0)),
            );
        for (i, (label, glyph)) in self.options.into_iter().enumerate() {
            let closeness = (1.0 - (pos - i as f32).abs()).clamp(0.0, 1.0);
            let fg = anim::blend(theme.muted, theme.ink, closeness);
            let on_select = self.on_select.clone();
            root = root.child(
                div()
                    .id(("seg", i))
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(6.0))
                    .w(px(w))
                    .h_full()
                    .cursor_pointer()
                    .text_size(px(text::CAPTION))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(fg)
                    .when_some(on_select, |el, f| el.on_click(move |_, window, cx| f(i, window, cx)))
                    .when_some(glyph, |el, g| el.child(icon(g).size(px(14.0)).text_color(fg)))
                    .child(label),
            );
        }
        root
    }
}

/// An on/off switch with a springy knob.
#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    on: bool,
    on_toggle: Option<Rc<dyn Fn(bool, &mut Window, &mut App)>>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>, on: bool) -> Self {
        Self { id: id.into(), on, on_toggle: None }
    }
    pub fn on_toggle(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let key = key_of(&self.id);
        let p = anim::spring(format!("{key}/knob"), if self.on { 1.0 } else { 0.0 }, SPRING_HOVER, window, cx);
        let on = self.on;
        let toggle = self.on_toggle.clone();
        let track = anim::blend(theme.line, theme.accent, p.clamp(0.0, 1.0));
        div()
            .id(self.id)
            .relative()
            .flex_none()
            .w(px(36.0))
            .h(px(20.0))
            .rounded_full()
            .bg(track)
            .cursor_pointer()
            .when_some(toggle, |el, f| el.on_click(move |_, window, cx| f(!on, window, cx)))
            .child(
                div()
                    .absolute()
                    .top(px(2.0))
                    .left(px(2.0 + 16.0 * p))
                    .size(px(16.0))
                    .rounded_full()
                    .bg(gpui::white())
                    .shadow(theme.card_shadow(-0.3)),
            )
    }
}

// -----------------------------------------------------------------------------------------------------------------
// Empty state, divider
// -----------------------------------------------------------------------------------------------------------------

/// The web's `Empty`: a dashed panel with a title and hint.
pub fn empty(title: impl Into<SharedString>, hint: Option<SharedString>, cx: &App) -> gpui::Div {
    let theme = Theme::of(cx);
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(4.0))
        .px(px(20.0))
        .py(px(28.0))
        .rounded(px(RADIUS_CARD))
        .border_1()
        .border_dashed()
        .border_color(theme.line)
        .child(div().font_weight(FontWeight::MEDIUM).text_color(theme.ink).child(title.into()))
        .when_some(hint, |el, hint| el.child(div().text_size(px(text::SMALL)).text_color(theme.muted).child(hint)))
}

pub fn divider(cx: &App) -> gpui::Div {
    div().h(px(1.0)).w_full().bg(Theme::of(cx).line)
}

/// A muted inline icon + text line ("Computer online").
pub fn icon_line(path: &'static str, label: impl Into<SharedString>, cx: &App) -> gpui::Div {
    let theme = Theme::of(cx);
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .text_size(px(text::SMALL))
        .text_color(theme.muted)
        .child(icon(path).size(px(16.0)).text_color(theme.muted))
        .child(label.into())
}

