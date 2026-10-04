//! The approval card (the web's `components/ApprovalCard.tsx`): a tool call waiting for a decision, or an `ask_user`
//! question waiting for an answer. Shared by Today and the chat's run card.

use std::collections::HashMap;
use std::rc::Rc;

use familiar_client::Approval;
use familiar_ui::anim;
use familiar_ui::icons;
use familiar_ui::components::{Button, ButtonSize, card, chip};
use familiar_ui::toast::ToastStack;
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::theme::{RADIUS_CHIP, Theme, Tone, text};
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, TextareaState};
use uuid::Uuid;
use serde_json::Value;

use crate::data::{AppData, Teammate, ago};
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

/// What the tool would do, in plain words (the inbox's risk hint).
pub fn action_of(tool: &str) -> &'static str {
    let t = tool.to_lowercase();
    match tool {
        "Bash" | "PowerShell" => "Runs a command on your computer",
        "Write" => "Creates or overwrites a file",
        "Edit" | "MultiEdit" | "NotebookEdit" => "Changes a file",
        "WebFetch" | "WebSearch" => "Reads from the web",
        _ if ["click", "type", "fill", "select", "press", "upload", "evaluate", "navigate"].iter().any(|k| t.contains(k)) => {
            "Acts in its browser"
        }
        _ if ["send", "post", "push", "merge"].iter().any(|k| t.contains(k)) => "Sends something on your behalf",
        _ if ["create", "delete", "update", "write"].iter().any(|k| t.contains(k)) => "Changes something in a connected app",
        _ => "Uses a tool",
    }
}

/// The detail block of the inbox card: a label and the readable input (command, path, address, or what changes).
fn detail_of(input: Option<&Value>) -> Option<(&'static str, String)> {
    let input = input?;
    let get = |k: &str| input.get(k).and_then(Value::as_str).map(str::to_owned);
    if let Some(c) = get("command") {
        return Some(("Command", c));
    }
    if let Some(p) = get("file_path").or_else(|| get("notebook_path")).or_else(|| get("path")) {
        let change = get("content").or_else(|| get("new_string")).or_else(|| get("new_source"));
        return Some(match change {
            Some(c) => ("File", format!("{p}\n\n{}", lines(&c, 14))),
            None => ("File", p),
        });
    }
    if let Some(u) = get("url") {
        return Some(("Address", u));
    }
    if let Some(q) = get("query") {
        return Some(("Search", q));
    }
    match input {
        Value::Object(m) if !m.is_empty() => Some(("Input", lines(&serde_json::to_string_pretty(input).unwrap_or_default(), 14))),
        _ => None,
    }
}

/// The first `n` lines (an ellipsis line when cut), each at most 400 chars.
fn lines(s: &str, n: usize) -> String {
    let mut out: Vec<String> = s.lines().take(n).map(|l| crate::data::excerpt(l, 400)).collect();
    if s.lines().count() > n {
        out.push("…".into());
    }
    out.join("\n")
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

/// `big`: the inbox's card (plain-language action, the whole command or path, large buttons).
pub fn approval_card(
    a: &Approval,
    bot: Option<&Teammate>,
    answer: Option<&Entity<TextareaState>>,
    decide: Decide,
    big: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let theme = Theme::of(cx).clone();
    let ask = is_ask(a);
    let question = question(a).filter(|_| ask);
    let summary = input_summary(&a.tool_name, a.input.as_ref());
    let name: SharedString = a.bot_name.clone().map(Into::into).or_else(|| bot.map(|b| b.name.clone())).unwrap_or_default();
    let name_plain = name.to_string();
    let size = if big { ButtonSize::Large } else { ButtonSize::Medium };
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
    if big && !ask {
        let who = if name_plain.is_empty() { "A teammate".to_owned() } else { name_plain.clone() };
        body = body.child(
            div()
                .text_size(px(text::LEAD))
                .text_color(theme.ink)
                .child(SharedString::from(format!("{who} wants to: {}", action_of(&a.tool_name).to_lowercase()))),
        );
    }
    if let Some(q) = question {
        body = body.child(div().text_color(theme.ink).when(big, |el| el.text_size(px(text::LEAD))).child(q));
    }
    let detail = if big && !ask { detail_of(a.input.as_ref()) } else { None };
    if let Some((label, d)) = detail {
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(div().text_size(px(text::CAPTION)).font_weight(FontWeight::MEDIUM).text_color(theme.muted).child(label))
                .child(
                    div()
                        .px(px(12.0))
                        .py(px(9.0))
                        .rounded(px(RADIUS_CHIP))
                        .bg(theme.sunken)
                        .border_l_2()
                        .border_color(if risk_of(&a.tool_name).1 == Tone::Bad { theme.bad } else { theme.warn })
                        .font_family(theme.font_mono.clone())
                        .text_size(px(text::SMALL))
                        .text_color(theme.ink)
                        .child(d),
                ),
        );
    } else if let Some(s) = summary.filter(|_| !ask && !big) {
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
            .child(div().flex_1().child(Button::new(("skip", id.as_u128() as u64), "Skip").size(size).full_width().on_click(
                move |_, w, cx| d1(false, w, cx),
            )))
            .child(
                div().flex_1().child(
                    Button::new(("send-answer", id.as_u128() as u64), "Send answer")
                        .primary()
                        .size(size)
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
                Button::new(("decline", id.as_u128() as u64), if big { "Deny" } else { "Decline" }).danger().size(size).full_width().on_click(
                    move |_, w, cx| d1(false, w, cx),
                ),
            ))
            .child(div().flex_1().child(
                Button::new(("approve", id.as_u128() as u64), "Approve").primary().size(size).icon(icons::CHECK).full_width().on_click(
                    move |_, w, cx| d2(true, w, cx),
                ),
            ))
    };
    body = body.child(buttons);

    card(cx)
        .p(px(if big { 20.0 } else { 16.0 }))
        .flex()
        .gap(px(14.0))
        .items_start()
        .when_some(bot, |el, b| {
            el.child(Mascot::new(format!("approval-{}", a.id), b.avatar, MascotState::NeedsYou, if big { 52.0 } else { 40.0 }))
        })
        .child(body)
        .into_any_element()
}

/// Approval cards with their answer boxes, for any view that lists approvals.
#[derive(Default)]
pub struct ApprovalCards {
    answers: HashMap<Uuid, Entity<TextareaState>>,
    big: bool,
}

impl ApprovalCards {
    /// The inbox's large cards.
    pub fn big() -> Self {
        Self { big: true, ..Default::default() }
    }

    pub fn render<V: 'static>(
        &mut self,
        list: &[Approval],
        data: &Entity<AppData>,
        toasts: &Entity<ToastStack>,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Vec<AnyElement> {
        let teammates = data.read(cx).teammates();
        let mut out = Vec::new();
        for (i, a) in list.iter().enumerate() {
            if is_ask(a) && !self.answers.contains_key(&a.id) {
                let state = text_input::new_field("Your answer", false, 4, window, cx);
                cx.subscribe(&state, |_, _, _: &InputEvent, cx| cx.notify()).detach();
                self.answers.insert(a.id, state);
            }
            let bot = teammates.iter().find(|t| t.uuid == a.bot_id);
            let decide = decider(data.clone(), toasts.clone(), a, self.answers.get(&a.id).cloned());
            let card = approval_card(a, bot, self.answers.get(&a.id), decide, self.big, window, cx);
            out.push(anim::stagger(SharedString::from(format!("approval-in-{}", a.id)), i, div().child(card)).into_any_element());
        }
        out
    }
}

/// The decide handler of a card: reads the answer box, calls the API, toasts the outcome.
pub fn decider(
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    a: &Approval,
    answer: Option<Entity<TextareaState>>,
) -> Decide {
    let ask = is_ask(a);
    let id = a.id;
    Rc::new(move |approve, _window, cx| {
        let response = if ask && approve { answer.as_ref().map(|s| s.read(cx).value().to_string()) } else { None };
        let task = data.update(cx, |d, cx| d.decide(id, approve, response, cx));
        let toasts = toasts.clone();
        cx.spawn(async move |cx| {
            let r = task.await;
            let _ = toasts.update(cx, |t, cx| match r {
                Ok(()) => {
                    let title = match (ask, approve) {
                        (true, true) => "Answer sent",
                        (true, false) => "Skipped",
                        (false, true) => "Approved",
                        (false, false) => "Declined",
                    };
                    t.push(if approve { Tone::Ok } else { Tone::Muted }, title, None, cx)
                }
                Err(e) => t.push(Tone::Bad, "Couldn't send that", Some(e.into()), cx),
            });
        })
        .detach();
    })
}
