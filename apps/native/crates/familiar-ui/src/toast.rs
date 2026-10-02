//! Toasts: a bottom-right stack of brief confirmations. They arrive with the web's `toast-in` (fade + 8 px rise,
//! 180 ms), stay ~4 s, and leave with a quicker fade and slight drop; hovering never matters because they never ask
//! for anything. Keep one [`ToastStack`] entity per window and render it last in the root, absolutely positioned.

use std::time::{Duration, Instant};

use gpui::{
    Context, FontWeight, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    InteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};

use crate::icons::{self, icon};
use crate::motion::{self, AnimationExt as _, CubicBezier, MotionSpec};
use crate::theme::{RADIUS_DIALOG, Theme, Tone, text};

pub const TOAST_IN: MotionSpec = MotionSpec::new(180, CubicBezier::new(0.0, 0.0, 0.58, 1.0));
pub const TOAST_OUT: Duration = Duration::from_millis(160);
pub const TOAST_LIFETIME: Duration = Duration::from_millis(4200);

struct Toast {
    id: u64,
    tone: Tone,
    title: SharedString,
    body: Option<SharedString>,
    leaving: Option<Instant>,
}

#[derive(Default)]
pub struct ToastStack {
    toasts: Vec<Toast>,
    next: u64,
}

impl ToastStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Show a toast. `tone` picks the glyph colour (Ok / Bad / Warn / Accent).
    pub fn push(&mut self, tone: Tone, title: impl Into<SharedString>, body: Option<SharedString>, cx: &mut Context<Self>) {
        let id = self.next;
        self.next += 1;
        self.toasts.push(Toast { id, tone, title: title.into(), body, leaving: None });
        // At most four on screen: the oldest leaves early.
        let live: Vec<u64> = self.toasts.iter().filter(|t| t.leaving.is_none()).map(|t| t.id).collect();
        if live.len() > 4 {
            self.dismiss(live[0], cx);
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_LIFETIME).await;
            let _ = this.update(cx, |this, cx| this.dismiss(id, cx));
        })
        .detach();
        cx.notify();
    }

    pub fn dismiss(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(toast) = self.toasts.iter_mut().find(|t| t.id == id && t.leaving.is_none()) else {
            return;
        };
        toast.leaving = Some(Instant::now());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_OUT.mul_f32(motion::speed_scale())).await;
            let _ = this.update(cx, |this, cx| {
                this.toasts.retain(|t| t.id != id);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for ToastStack {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let reduced = motion::reduced_motion(cx);
        let mut stack = div().absolute().right(px(20.0)).bottom(px(20.0)).flex().flex_col().gap(px(8.0)).w(px(340.0));
        for toast in &self.toasts {
            let (fg, wash) = theme.tone(toast.tone);
            let glyph = match toast.tone {
                Tone::Ok => icons::CHECK,
                Tone::Bad | Tone::Warn => icons::DANGER_TRIANGLE,
                Tone::Accent | Tone::Muted => icons::INFO_CIRCLE,
            };
            let out = toast.leaving.map(|at| {
                let t = at.elapsed().as_secs_f32() / TOAST_OUT.mul_f32(motion::speed_scale()).as_secs_f32();
                if reduced { 1.0 } else { motion::EASE.eval(t.min(1.0)) }
            });
            if out.is_some() {
                window.request_animation_frame();
            }
            let id = toast.id;
            let card = div()
                .id(("toast", toast.id as usize))
                .flex()
                .items_start()
                .gap(px(10.0))
                .p(px(12.0))
                .rounded(px(RADIUS_DIALOG))
                .bg(theme.surface)
                .border_1()
                .border_color(theme.line)
                .shadow(theme.float_shadow())
                .child(
                    div()
                        .flex_none()
                        .size(px(26.0))
                        .rounded_full()
                        .bg(wash)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(glyph).size(px(15.0)).text_color(fg)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .pt(px(3.0))
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(px(text::SMALL))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.ink)
                                .child(toast.title.clone()),
                        )
                        .when_some(toast.body.clone(), |el, body| {
                            el.child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(body))
                        }),
                )
                .child(
                    div()
                        .id(("toast-close", toast.id as usize))
                        .flex_none()
                        .size(px(22.0))
                        .rounded(px(6.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.hover))
                        .on_click(cx.listener(move |this, _, _, cx| this.dismiss(id, cx)))
                        .child(icon(icons::CLOSE).size(px(12.0)).text_color(theme.muted)),
                );
            let element = match out {
                Some(t) => div().relative().opacity(1.0 - t).top(px(6.0 * t)).child(card).into_any_element(),
                None => div()
                    .child(card)
                    .with_animation(("toast-in", toast.id as usize), TOAST_IN.animation(), |el, t| {
                        el.relative().opacity(t).top(px(8.0 * (1.0 - t)))
                    })
                    .into_any_element(),
            };
            stack = stack.child(element);
        }
        stack
    }
}
