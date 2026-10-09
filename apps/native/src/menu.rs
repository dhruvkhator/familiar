//! A small popover menu: a floating panel of choices under (or beside) its trigger. The owning view keeps which menu
//! is open and the highlighted row; the panel takes the keyboard while it is open (up and down, Home and End, a digit,
//! Enter or Space to pick, Escape to close) and closes on a click outside it.

use std::rc::Rc;

use familiar_ui::icons::{self, icon};
use familiar_ui::motion;
use familiar_ui::theme::{RADIUS_CONTROL, RADIUS_DIALOG, Theme, text};
use gpui::{
    Anchor, App, FocusHandle, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, anchored, deferred, div,
    prelude::FluentBuilder as _, px,
};

use crate::crm_model::menu_step;

/// One row of a menu.
#[derive(Clone)]
pub struct MenuItem {
    pub label: SharedString,
    /// A muted note on the right ("12", "current").
    pub detail: Option<SharedString>,
    /// Ticked: the current choice.
    pub checked: bool,
}

impl MenuItem {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self { label: label.into(), detail: None, checked: false }
    }
    pub fn detail(mut self, d: impl Into<SharedString>) -> Self {
        self.detail = Some(d.into());
        self
    }
    pub fn checked(mut self, on: bool) -> Self {
        self.checked = on;
        self
    }
}

/// The width of an icon-only small button (a `right` menu's trigger).
const ICON_TRIGGER: f32 = 28.0;

thread_local! {
    /// The menu a click outside just closed, and when: that click may be on its own trigger, which must not reopen it.
    static CLOSED: std::cell::RefCell<Option<(SharedString, std::time::Instant)>> = const { std::cell::RefCell::new(None) };
}

/// Menu `id` was closed by the press of the click now landing (on its trigger): leave it closed.
pub fn closed_just_now(id: &str) -> bool {
    CLOSED.with(|c| c.borrow().as_ref().is_some_and(|(m, at)| m.as_ref() == id && at.elapsed() < std::time::Duration::from_millis(400)))
}

type Pick = Rc<dyn Fn(usize, &mut Window, &mut App)>;
type Close = Rc<dyn Fn(&mut Window, &mut App)>;

/// The open menu's panel, drawn above everything (deferred) at the place it sits in its parent: put it right after the
/// trigger in a `flex_col` wrapper. `right`: its right edge lines up with a small icon trigger's (for triggers near the
/// right side). `cursor`: the highlighted row; `on_cursor` moves it.
#[allow(clippy::too_many_arguments)]
pub fn popover(
    id: impl Into<SharedString>,
    focus: &FocusHandle,
    items: Vec<MenuItem>,
    cursor: usize,
    width: f32,
    right: bool,
    on_pick: impl Fn(usize, &mut Window, &mut App) + 'static,
    on_cursor: impl Fn(usize, &mut Window, &mut App) + 'static,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let theme = Theme::of(cx).clone();
    let id: SharedString = id.into();
    let (pick, close): (Pick, Close) = (Rc::new(on_pick), Rc::new(on_close));
    let on_cursor = Rc::new(on_cursor);
    let len = items.len();
    let mut list = div().flex().flex_col().p(px(4.0)).gap(px(1.0));
    for (i, item) in items.into_iter().enumerate() {
        let pick = pick.clone();
        let hover_cursor = on_cursor.clone();
        let lit = i == cursor;
        list = list.child(
            div()
                .id(SharedString::from(format!("{id}-row-{i}")))
                .flex()
                .items_center()
                .gap(px(8.0))
                .h(px(32.0))
                .px(px(10.0))
                .rounded(px(RADIUS_CONTROL - 3.0))
                .cursor_pointer()
                .text_size(px(text::SMALL))
                .text_color(theme.ink)
                .when(lit, |el| el.bg(theme.hover))
                .on_hover(move |hovered, window, cx| {
                    if *hovered {
                        hover_cursor(i, window, cx)
                    }
                })
                .on_click(move |_, window, cx| pick(i, window, cx))
                .child(
                    div()
                        .w(px(14.0))
                        .flex_none()
                        .when(item.checked, |el| el.child(icon(icons::CHECK).size(px(14.0)).text_color(theme.accent))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(item.checked, |el| el.font_weight(FontWeight::MEDIUM))
                        .child(item.label),
                )
                .when_some(item.detail, |el, d| {
                    el.child(div().flex_none().text_size(px(text::CAPTION)).text_color(theme.muted).child(d))
                }),
        );
    }
    let (key_pick, key_close, key_cursor) = (pick.clone(), close.clone(), on_cursor.clone());
    let out_close = close.clone();
    let out_id = id.clone();
    let panel = div()
        .id(SharedString::from(format!("{id}-panel")))
        .track_focus(focus)
        .occlude()
        .w(px(width))
        .max_h(px(360.0))
        .overflow_y_scroll()
        .rounded(px(RADIUS_DIALOG - 4.0))
        .border_1()
        .border_color(theme.line)
        .bg(theme.surface)
        .shadow(theme.float_shadow())
        .on_key_down(move |ev: &KeyDownEvent, window, cx| {
            let key = ev.keystroke.key.as_str();
            match key {
                "escape" => key_close(window, cx),
                "enter" | "space" => key_pick(cursor, window, cx),
                _ => match menu_step(cursor, len, key) {
                    Some(n) => key_cursor(n, window, cx),
                    None => return,
                },
            }
            cx.stop_propagation();
        })
        .on_mouse_down_out(move |_, window, cx| {
            CLOSED.with(|c| *c.borrow_mut() = Some((out_id.clone(), std::time::Instant::now())));
            out_close(window, cx)
        })
        .child(list);
    deferred(
        anchored()
            .anchor(Anchor::TopLeft)
            .when(right, |a| a.offset(gpui::point(px(ICON_TRIGGER - width), px(0.0))))
            .snap_to_window_with_margin(px(8.0))
            .child(div().pt(px(6.0)).child(motion::menu_in(SharedString::from(format!("{id}-in")), panel))),
    )
    .with_priority(2)
}

/// A trigger that looks like a quiet select: "Tag: any ⌄". `active`: a filter is set (accent tint).
pub fn trigger(id: impl Into<SharedString>, label: impl Into<SharedString>, active: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
    let theme = Theme::of(cx).clone();
    div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.0))
        .h(px(32.0))
        .px(px(10.0))
        .rounded(px(RADIUS_CONTROL))
        .border_1()
        .border_color(if active { theme.accent.opacity(0.5) } else { theme.line })
        .bg(if active { theme.accent_soft } else { theme.surface })
        .text_size(px(text::SMALL))
        .text_color(if active { theme.accent } else { theme.ink })
        .cursor_pointer()
        .hover(|s| s.bg(theme.hover))
        .child(label.into())
        .child(icon(icons::ALT_ARROW_DOWN).size(px(14.0)).text_color(if active { theme.accent } else { theme.muted }))
}
