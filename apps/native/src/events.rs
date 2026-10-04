//! A run's persisted events as rows (the web's `components/EventList.tsx`): text, thinking, tool calls (expandable,
//! with their result folded in), approvals, files, errors and the final result. Shared by the chat's live run card and
//! the Activity tab's run timeline.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use familiar_client::{Event, TypedEvent};
use familiar_ui::anim::{self, Expand};
use familiar_ui::components::chip;
use familiar_ui::icons::{self, icon};
use familiar_ui::theme::{RADIUS_CHIP, Theme, Tone, text};
use gpui::{
    AnyElement, App, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use serde_json::Value;

use crate::data::excerpt;
use crate::markdown;

/// Open/closed state of the tool-call rows, keyed by event id. Cheap to clone (shared).
#[derive(Clone, Default)]
pub struct EventRows {
    open: Rc<RefCell<HashMap<i64, Expand>>>,
}

/// How a tool call ended, from its paired result.
#[derive(Clone)]
struct Outcome {
    content: String,
    is_error: bool,
}

impl EventRows {
    /// Rows for `events` (in seq order). `timeline`: a time column on the left (the Activity view); `live`: calls
    /// without a result yet show as running.
    pub fn render(&self, events: &[Event], timeline: bool, live: bool, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
        // Results fold into their call; a result whose call isn't listed keeps its own row.
        let mut outcomes: HashMap<String, Outcome> = HashMap::new();
        let mut calls: HashSet<String> = HashSet::new();
        for e in events {
            match e.typed() {
                TypedEvent::ToolCall(c) => {
                    if let Some(id) = c.id {
                        calls.insert(id);
                    }
                }
                TypedEvent::ToolResult(r) => {
                    if let Some(id) = r.tool_use_id {
                        outcomes.insert(id, Outcome { content: r.content, is_error: r.is_error });
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for e in events {
            let Some(row) = self.row(e, &outcomes, &calls, live, window, cx) else { continue };
            let row = if timeline {
                let theme = Theme::of(cx);
                let at = e.created_at.with_timezone(&chrono::Local).format("%H:%M:%S").to_string();
                div()
                    .flex()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex_none()
                            .w(px(64.0))
                            .whitespace_nowrap()
                            .pt(px(1.0))
                            .font_family(theme.font_mono.clone())
                            .text_size(px(text::CAPTION))
                            .text_color(theme.muted)
                            .child(at),
                    )
                    .child(div().flex_1().min_w_0().child(row))
                    .into_any_element()
            } else {
                row
            };
            out.push(anim::appear(SharedString::from(format!("ev-{}", e.id)), div().child(row)).into_any_element());
        }
        out
    }

    fn dot(color: Hsla) -> gpui::Div {
        div().flex_none().mt(px(7.0)).size(px(6.0)).rounded_full().bg(color)
    }

    fn row(
        &self,
        e: &Event,
        outcomes: &HashMap<String, Outcome>,
        calls: &HashSet<String>,
        live: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).clone();
        let mono = theme.font_mono.clone();
        let row = |dot: Hsla, body: AnyElement| {
            div().flex().gap(px(10.0)).child(Self::dot(dot)).child(div().flex_1().min_w_0().child(body)).into_any_element()
        };
        let block = |content: String, error: bool| {
            div()
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(RADIUS_CHIP))
                .bg(theme.sunken)
                .when(error, |el| el.border_l_2().border_color(theme.bad))
                .font_family(mono.clone())
                .text_size(px(text::CAPTION))
                .text_color(if error { theme.bad } else { theme.ink })
                .child(content)
        };
        Some(match e.typed() {
            TypedEvent::Text(t) if !t.trim().is_empty() => row(theme.ink, markdown::render(&t, &theme).into_any_element()),
            TypedEvent::Thinking(t) if !t.trim().is_empty() => row(
                theme.line,
                div()
                    .text_size(px(text::SMALL))
                    .text_color(theme.muted)
                    .italic()
                    .line_clamp(4)
                    .child(t.trim().to_owned())
                    .into_any_element(),
            ),
            TypedEvent::ToolCall(c) => {
                let preview = ["command", "file_path", "path", "url", "query", "pattern", "question"]
                    .iter()
                    .find_map(|k| c.input.get(*k).and_then(Value::as_str))
                    .map(|s| excerpt(s, 90));
                let outcome = c.id.as_ref().and_then(|id| outcomes.get(id)).cloned();
                let (mark, mark_color) = match &outcome {
                    Some(o) if o.is_error => (Some(icons::CLOSE_CIRCLE), theme.bad),
                    Some(_) => (Some(icons::CHECK), theme.ok),
                    None => (None, theme.muted),
                };
                let mut open = self.open.borrow_mut();
                let exp = open.entry(e.id).or_insert_with(|| Expand::new(false));
                let openness = exp.openness();
                let json = serde_json::to_string_pretty(&c.input).unwrap_or_default();
                let mut detail = div().mt(px(6.0)).flex().flex_col().gap(px(6.0)).child(block(excerpt_lines(&json, 40), false));
                if let Some(o) = &outcome
                    && !o.content.trim().is_empty()
                {
                    detail = detail.child(block(excerpt_lines(o.content.trim(), 30), o.is_error));
                }
                let detail = exp.render(SharedString::from(format!("tool-detail-{}", e.id)), window, cx, detail);
                drop(open);
                let toggle = self.open.clone();
                let eid = e.id;
                row(
                    theme.accent,
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .id(SharedString::from(format!("tool-{}", e.id)))
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .cursor_pointer()
                                .rounded(px(RADIUS_CHIP))
                                .hover(|s| s.bg(theme.hover))
                                .on_click(move |_, window, _| {
                                    if let Some(x) = toggle.borrow_mut().get_mut(&eid) {
                                        x.toggle();
                                    }
                                    window.refresh();
                                })
                                .child(
                                    icon(icons::ALT_ARROW_RIGHT)
                                        .size(px(12.0))
                                        .text_color(theme.muted)
                                        .with_transformation(gpui::Transformation::rotate(gpui::radians(
                                            openness * std::f32::consts::FRAC_PI_2,
                                        ))),
                                )
                                .child(
                                    div()
                                        .font_family(mono.clone())
                                        .text_size(px(text::CAPTION))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.ink)
                                        .child(c.name.clone()),
                                )
                                .when_some(preview, |el, p| {
                                    el.child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_family(mono.clone())
                                            .text_size(px(text::CAPTION))
                                            .text_color(theme.muted)
                                            .child(p),
                                    )
                                })
                                .when_some(mark, |el, m| el.child(icon(m).size(px(12.0)).text_color(mark_color)))
                                .when(outcome.is_none() && live, |el| {
                                    el.child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child("running…"))
                                }),
                        )
                        .child(detail)
                        .into_any_element(),
                )
            }
            TypedEvent::ToolResult(r) => {
                // Shown with its call.
                if r.tool_use_id.as_ref().is_some_and(|id| calls.contains(id)) {
                    return None;
                }
                let body = r.content.trim();
                if body.is_empty() && !r.is_error {
                    return None;
                }
                row(
                    if r.is_error { theme.bad } else { theme.line },
                    div()
                        .px(px(10.0))
                        .py(px(6.0))
                        .rounded(px(RADIUS_CHIP))
                        .bg(theme.sunken)
                        .when(r.is_error, |el| el.border_l_2().border_color(theme.bad))
                        .font_family(mono.clone())
                        .text_size(px(text::CAPTION))
                        .text_color(theme.muted)
                        .line_clamp(3)
                        .child(excerpt(body, 600))
                        .into_any_element(),
                )
            }
            TypedEvent::Approval(a) => {
                let status = a.status.clone().unwrap_or_else(|| "approval".into());
                let tone = match status.as_str() {
                    "approved" | "approve" | "allow" => Tone::Ok,
                    "pending" | "approval" => Tone::Warn,
                    _ => Tone::Bad,
                };
                row(
                    theme.warn,
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(chip(tone, status, cx))
                        .when_some(a.tool_name.clone(), |el, t| {
                            el.child(div().font_family(mono.clone()).text_size(px(text::CAPTION)).child(t))
                        })
                        .when_some(a.decided_by.clone(), |el, by| {
                            el.child(
                                div().text_size(px(text::CAPTION)).text_color(theme.muted).child(SharedString::from(format!("by {by}"))),
                            )
                        })
                        .into_any_element(),
                )
            }
            TypedEvent::Artifact(a) => row(
                theme.ok,
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(text::SMALL))
                    .child(div().text_color(theme.muted).child("Saved a file"))
                    .child(div().font_family(mono.clone()).text_size(px(text::CAPTION)).child(a.name))
                    .into_any_element(),
            ),
            TypedEvent::Error(m) => {
                row(theme.bad, div().text_size(px(text::SMALL)).text_color(theme.bad).child(m).into_any_element())
            }
            TypedEvent::Result(r) => {
                let mut parts = vec!["Finished".to_owned()];
                if let Some(n) = r.num_turns {
                    parts.push(format!("{n} turn{}", if n == 1 { "" } else { "s" }));
                }
                if let Some(ms) = r.duration_ms {
                    parts.push(secs_label(ms / 1000));
                }
                if let Some(c) = r.cost_usd.filter(|c| *c > 0.0) {
                    parts.push(format!("≈ {} at API prices", money(c)));
                }
                row(
                    theme.ok,
                    div().text_size(px(text::CAPTION)).text_color(theme.muted).child(parts.join(" · ")).into_any_element(),
                )
            }
            _ => return None,
        })
    }
}

/// The first `n` lines of `s` (an ellipsis line when cut).
pub fn excerpt_lines(s: &str, n: usize) -> String {
    let mut lines: Vec<&str> = s.lines().take(n + 1).collect();
    if lines.len() > n {
        lines.truncate(n);
        lines.push("…");
    }
    lines.join("\n")
}

/// The web's `money`: four decimals under a dollar.
pub fn money(v: f64) -> String {
    if v < 1.0 { format!("${v:.4}") } else { format!("${v:.2}") }
}

/// "42s", "3m 5s", "1h 4m" (the web's `duration`).
pub fn secs_label(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    }
}
