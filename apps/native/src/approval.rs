//! The approval card (the web's `components/ApprovalCard.tsx`): a tool call waiting for a decision, or an `ask_user`
//! question waiting for an answer. Shared by Today and the chat's run card.

use std::rc::Rc;

use familiar_client::Approval;
use familiar_ui::components::{Button, card, chip};
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::theme::{RADIUS_CHIP, Theme, Tone, text};
use gpui::{
    AnyElement, App, Entity, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_base::input::TextareaState;
use serde_json::Value;

use crate::data::{Teammate, ago};
use crate::text_input;

pub type Decide = Rc<dyn Fn(bool, &mut Window, &mut App)>;

pub fn is_ask(a: &Approval) -> bool {
    a.tool_name == "ask_user"
}

pub fn question(a: &Approval) -> Option<String> {
    let input = a.input.as_ref()?;
    ["question", "prompt"].iter().find_map(|k| input.get(*k).and_then(Value::as_str)).map(str::to_owned)
}

/// The web's `riskOf`.
pub fn risk_of(tool: &str) -> (&'static str, Tone) {
    let t = tool.to_lowercase();
    if matches!(tool, "Bash" | "PowerShell") {
        ("high risk", Tone::Bad)
    } else if matches!(tool, "Write" | "Edit" | "MultiEdit" | "NotebookEdit")
        || ["click", "type", "fill", "select", "upload", "evaluate", "press"].iter().any(|k| t.contains(k))
        || ["create", "delete", "send", "post", "push", "merge", "update", "write"].iter().any(|k| t.contains(k))
    {
        ("medium risk", Tone::Warn)
    } else {
        ("low risk", Tone::Muted)
    }
}

/// The one-line input summary (command, path, url or query).
pub fn input_summary(tool: &str, input: Option<&Value>) -> Option<String> {
    let input = input?;
    let get = |k: &str| input.get(k).and_then(Value::as_str).map(str::to_owned);
    if tool == "Bash"
        && let Some(c) = get("command")
    {
        return Some(c);
    }
    ["file_path", "path", "notebook_path", "url", "query"].iter().find_map(|k| get(k))
}

pub fn approval_card(
    a: &Approval,
    bot: Option<&Teammate>,
    answer: Option<&Entity<TextareaState>>,
    decide: Decide,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let theme = Theme::of(cx).clone();
    let ask = is_ask(a);
    let question = question(a).filter(|_| ask);
    let summary = input_summary(&a.tool_name, a.input.as_ref());
    let name: SharedString = a.bot_name.clone().map(Into::into).or_else(|| bot.map(|b| b.name.clone())).unwrap_or_default();
    let answer_blank = answer.is_some_and(|s| s.read(cx).value().trim().is_empty());
    let id = a.id;

    let header = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .flex_wrap()
        .child(div().font_weight(FontWeight::MEDIUM).child(name))
        .child(chip(Tone::Warn, if ask { "question" } else { "needs approval" }, cx))
        .when(!ask, |el| {
            let (label, tone) = risk_of(&a.tool_name);
            el.child(chip(tone, label, cx))
        })
        .when(!ask, |el| {
            el.child(
                div()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(text::CAPTION))
                    .text_color(theme.muted)
                    .child(SharedString::from(a.tool_name.clone())),
            )
        })
        .child(div().flex_1())
        .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(ago(Some(a.created_at))));

    let mut body = div().flex().flex_col().flex_1().min_w_0().gap(px(10.0)).child(header);
    if let Some(q) = question {
        body = body.child(div().text_color(theme.ink).child(q));
    }
    if let Some(s) = summary.filter(|_| !ask) {
        body = body.child(
            div()
                .px(px(10.0))
                .py(px(7.0))
                .rounded(px(RADIUS_CHIP))
                .bg(theme.sunken)
                .border_l_2()
                .border_color(theme.warn)
                .font_family(theme.font_mono.clone())
                .text_size(px(text::CAPTION))
                .text_color(theme.ink)
                .child(crate::data::excerpt(&s, 400)),
        );
    }
    if let Some(r) = a.reason.clone().filter(|r| !r.trim().is_empty()) {
        body = body.child(div().text_size(px(text::SMALL)).text_color(theme.muted).child(r));
    }
    if let Some(state) = answer.filter(|_| ask) {
        body = body.child(text_input::field(("answer", id.as_u128() as u64), state, 60.0, window, cx));
    }
    let (d1, d2) = (decide.clone(), decide);
    let buttons = if ask {
        div()
            .flex()
            .gap(px(8.0))
            .child(div().flex_1().child(Button::new(("skip", id.as_u128() as u64), "Skip").full_width().on_click(
                move |_, w, cx| d1(false, w, cx),
            )))
            .child(
                div().flex_1().child(
                    Button::new(("send-answer", id.as_u128() as u64), "Send answer")
                        .primary()
                        .full_width()
                        .disabled(answer_blank)
                        .on_click(move |_, w, cx| d2(true, w, cx)),
                ),
            )
    } else {
        div()
            .flex()
            .gap(px(8.0))
            .child(div().flex_1().child(
                Button::new(("decline", id.as_u128() as u64), "Decline").danger().full_width().on_click(
                    move |_, w, cx| d1(false, w, cx),
                ),
            ))
            .child(div().flex_1().child(
                Button::new(("approve", id.as_u128() as u64), "Approve").primary().full_width().on_click(
                    move |_, w, cx| d2(true, w, cx),
                ),
            ))
    };
    body = body.child(buttons);

    card(cx)
        .p(px(16.0))
        .flex()
        .gap(px(14.0))
        .items_start()
        .when_some(bot, |el, b| {
            el.child(Mascot::new(format!("approval-{}", a.id), b.avatar, MascotState::NeedsYou, 40.0))
        })
        .child(body)
        .into_any_element()
}
