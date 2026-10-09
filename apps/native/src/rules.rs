//! The Rules page (the web's `pages/Rules.tsx`, for every teammate at once): what teammates may do without asking.
//! Every rule (for every teammate, and each teammate's own) in plain words, with its tier (do it, let auto-review
//! decide, ask me, hand it to me) changeable in place, its note and delete (confirmed); adding one with a live
//! explanation of what the pattern covers, examples, who it is for, and a warning on rules that let a teammate run
//! anything; and the actions that always need you, whatever the rules say. A teammate's own "Allowed without asking"
//! stays on its Settings tab.
//!
//! The API has no "change": a new tier is a new rule, then the old one goes. Patterns are shown with hidden characters
//! written out. Rules send no notices: the list reloads when the page is shown and after each change.

use std::collections::HashSet;

use familiar_client::{NewRule, Rule, RuleDecision};
use familiar_ui::anim;
use familiar_ui::components::{Button, ButtonSize, Segmented, Skeleton, card, divider};
use familiar_ui::icons::{self, icon};
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CONTROL, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::approval::{reveal, strip_hidden};
use crate::data::{AppData, avatar_of};
use crate::menu::{self, MenuItem};
use crate::text_input;

/// The tiers, in order: (decision, short label, what it means).
const TIERS: [(RuleDecision, &str, &str); 4] = [
    (RuleDecision::Allow, "Do it", "It goes ahead without asking."),
    (RuleDecision::Review, "Auto-review", "A quick reviewer lets safe calls through and sends the rest to you."),
    (RuleDecision::Ask, "Ask me", "It waits for your Approve or Decline in Needs you."),
    (RuleDecision::Deny, "Hand it to me", "It stops and tells you to do it yourself."),
];

/// What always needs you, whatever the rules say.
const LOCKED: [&str; 6] = [
    "Deleting files recursively (rm -rf, Remove-Item -Recurse)",
    "Installing software (npm -g, pip, winget, choco)",
    "Running as administrator (sudo, runas)",
    "Force-pushing or hard-resetting git history",
    "Downloading a script and running it",
    "Editing the registry or scheduled tasks",
];

/// Patterns to start from.
const EXAMPLES: [&str; 4] = ["Bash(git status*)", "Edit", "mcp__github", "WebFetch"];

pub struct RulesPage {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    rules: Option<Vec<Rule>>,
    error: Option<String>,
    /// The Add form: the pattern, the note, who it's for (`None`: every teammate), the tier.
    pattern: Option<Entity<InputState>>,
    note: Option<Entity<InputState>>,
    who: Option<Uuid>,
    tier: usize,
    adding: bool,
    /// The Add form is open.
    form_open: bool,
    add_error: Option<String>,
    /// The "Who" menu, with its highlighted row.
    menu: Option<usize>,
    menu_focus: FocusHandle,
    confirm_delete: Option<Uuid>,
    busy: HashSet<Uuid>,
    shown: bool,
}

impl RulesPage {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, cx: &mut Context<Self>) -> Self {
        // The teammates' names and faces.
        cx.subscribe(&data, |_, _, ev: &crate::data::DataEvent, cx| {
            if matches!(ev, crate::data::DataEvent::Updated(crate::data::Part::Overview)) {
                cx.notify()
            }
        })
        .detach();
        let mut this = Self {
            data,
            toasts,
            rules: None,
            error: None,
            pattern: None,
            note: None,
            who: None,
            tier: 2,
            adding: false,
            form_open: false,
            add_error: None,
            menu: None,
            menu_focus: cx.focus_handle(),
            confirm_delete: None,
            busy: HashSet::new(),
            shown: true,
        };
        this.reload(cx);
        this
    }

    fn client(&self, cx: &App) -> familiar_client::Client {
        self.data.read(cx).client.clone()
    }

    fn toast(&self, tone: Tone, title: impl Into<SharedString>, body: Option<String>, cx: &mut Context<Self>) {
        let title = title.into();
        self.toasts.update(cx, |t, cx| t.push(tone, title, body.map(Into::into), cx));
    }

    /// The shell shows or hides the page: shown again, the list is read afresh (rules send no notices).
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if shown && !self.shown {
            self.reload(cx);
        }
        if !shown {
            self.menu = None;
            self.confirm_delete = None;
        }
        self.shown = shown;
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move { client.rules(None, true).await });
        cx.spawn(async move |this, cx| {
            let Ok(r) = task.await else { return };
            let _ = this.update(cx, |p, cx| {
                match r {
                    Ok(list) => {
                        p.rules = Some(list);
                        p.error = None;
                    }
                    Err(e) => p.error = Some(e.message()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// After a change: other views' cached rules (a teammate's "Allowed without asking") are read afresh too.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.data.read(cx).client.invalidate("/api/rules");
        self.reload(cx);
    }

    fn inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) -> (Entity<InputState>, Entity<InputState>) {
        if self.pattern.is_none() {
            let pattern = text_input::new_line("Bash(git status*)", false, window, cx);
            let note = text_input::new_line("Why this rule exists (optional)", false, window, cx);
            for input in [&pattern, &note] {
                cx.subscribe_in(input, window, |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
                    InputEvent::PressEnter { .. } => this.add(window, cx),
                    InputEvent::Change => {
                        this.add_error = None;
                        cx.notify()
                    }
                    _ => {}
                })
                .detach();
            }
            self.pattern = Some(pattern);
            self.note = Some(note);
        }
        (self.pattern.clone().expect("made above"), self.note.clone().expect("made above"))
    }

    /// Fill in the Add form (the bench's shot).
    pub fn fill(&mut self, pattern: &str, tier: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (p, _) = self.inputs(window, cx);
        p.update(cx, |s, cx| s.set_value(pattern.to_owned(), window, cx));
        self.tier = tier.min(TIERS.len() - 1);
        self.form_open = true;
        cx.notify();
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(p), Some(n)) = (self.pattern.clone(), self.note.clone()) else { return };
        if self.adding {
            return;
        }
        let pattern = p.read(cx).value().trim().to_owned();
        if pattern.is_empty() {
            self.add_error = Some("Say what it applies to, for example Bash(git status*).".into());
            cx.notify();
            return;
        }
        let note = n.read(cx).value().trim().to_owned();
        let body = NewRule {
            bot_id: self.who,
            pattern: Some(pattern),
            decision: Some(TIERS[self.tier].0.as_str().to_owned()),
            note: (!note.is_empty()).then_some(note),
        };
        self.adding = true;
        cx.notify();
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move { client.create_rule(&body).await });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update_in(cx, |p, window, cx| {
                p.adding = false;
                match r {
                    Ok(_) => {
                        for input in [&p.pattern, &p.note].into_iter().flatten() {
                            input.update(cx, |s, cx| s.set_value("", window, cx));
                        }
                        p.form_open = false;
                        p.toast(Tone::Ok, "Rule added", Some("It counts from the teammate's next step.".into()), cx);
                        p.changed(cx);
                    }
                    Err(e) => p.add_error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A rule's new tier: a new rule with it, then the old one goes. Shown at once.
    fn set_tier(&mut self, r: &Rule, decision: RuleDecision, cx: &mut Context<Self>) {
        if r.decision == decision || !self.busy.insert(r.id) {
            return;
        }
        if let Some(x) = self.rules.as_mut().and_then(|l| l.iter_mut().find(|x| x.id == r.id)) {
            x.decision = decision;
        }
        cx.notify();
        let client = self.client(cx);
        let (old, body) = (r.id, NewRule { bot_id: r.bot_id, pattern: Some(r.pattern.clone()), decision: Some(decision.as_str().into()), note: r.note.clone() });
        let task = Tokio::spawn(cx, async move {
            client.create_rule(&body).await?;
            client.delete_rule(old).await
        });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                p.busy.remove(&old);
                if let Err(e) = r {
                    p.toast(Tone::Bad, "Couldn't change that rule", Some(e), cx);
                }
                p.changed(cx);
            });
        })
        .detach();
    }

    fn delete(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.confirm_delete = None;
        if !self.busy.insert(id) {
            return;
        }
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move { client.delete_rule(id).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                p.busy.remove(&id);
                match r {
                    Ok(()) => {
                        p.rules.iter_mut().for_each(|l| l.retain(|r| r.id != id));
                        p.toast(Tone::Ok, "Rule removed", None, cx);
                    }
                    Err(e) => p.toast(Tone::Bad, "Couldn't remove it", Some(e), cx),
                }
                p.changed(cx);
            });
        })
        .detach();
    }

    // ---- drawing ------------------------------------------------------------------------------------------------

    fn teammates(&self, cx: &App) -> Vec<(Uuid, String)> {
        self.data.read(cx).bots().iter().map(|b| (b.id, strip_hidden(&b.name, false))).collect()
    }

    /// "Who it's for": every teammate, or one.
    fn who_select(&self, cx: &mut Context<Self>) -> AnyElement {
        let list = self.teammates(cx);
        let label = match self.who.and_then(|id| list.iter().find(|(b, _)| *b == id)) {
            Some((_, n)) => n.clone(),
            None => "Every teammate".to_owned(),
        };
        let mut items = vec![MenuItem::new("Every teammate").checked(self.who.is_none())];
        items.extend(list.iter().map(|(id, n)| MenuItem::new(n.clone()).checked(self.who == Some(*id))));
        let popover = self.menu.map(|cursor| {
            let (pick, cur, close) = (cx.entity(), cx.entity(), cx.entity());
            let ids: Vec<Uuid> = list.iter().map(|(id, _)| *id).collect();
            menu::popover(
                "rule-who",
                &self.menu_focus,
                items,
                cursor,
                240.0,
                false,
                move |i, _, cx| {
                    let who = if i == 0 { None } else { ids.get(i - 1).copied() };
                    pick.update(cx, |p, cx| {
                        p.who = who;
                        p.menu = None;
                        cx.notify()
                    })
                },
                move |i, _, cx| {
                    cur.update(cx, |p, cx| {
                        p.menu = Some(i);
                        cx.notify()
                    })
                },
                move |_, cx| {
                    close.update(cx, |p, cx| {
                        p.menu = None;
                        cx.notify()
                    })
                },
                cx,
            )
        });
        let this = cx.entity();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .child(menu::trigger("rule-who", label, self.who.is_some(), cx).on_click(move |_, window, cx| {
                this.update(cx, |p, cx| {
                    if menu::closed_just_now("rule-who") {
                        return;
                    }
                    if p.menu.is_some() {
                        p.menu = None;
                    } else {
                        let list = p.teammates(cx);
                        let at = p.who.and_then(|id| list.iter().position(|(b, _)| *b == id)).map(|i| i + 1).unwrap_or(0);
                        p.menu = Some(at);
                        p.menu_focus.focus(window, cx);
                    }
                    cx.notify()
                })
            }))
            .children(popover)
            .into_any_element()
    }

    fn add_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let (pattern, note) = self.inputs(window, cx);
        let typed = pattern.read(cx).value().trim().to_owned();
        let label = |s: &'static str| div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child(s);
        let mut examples = div().flex().flex_wrap().items_center().gap(px(6.0)).child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child("For example"));
        for (i, ex) in EXAMPLES.iter().enumerate() {
            let input = pattern.clone();
            examples = examples.child(
                div()
                    .id(("rule-ex", i))
                    .px(px(8.0))
                    .py(px(2.0))
                    .rounded(px(6.0))
                    .bg(theme.sunken)
                    .border_1()
                    .border_color(theme.line)
                    .font_family(theme.font_mono.clone())
                    .text_size(px(text::CAPTION))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.hover))
                    .on_click(move |_, window, cx| input.update(cx, |s, cx| s.set_value((*ex).to_owned(), window, cx)))
                    .child(*ex),
            );
        }
        let explained = (!typed.is_empty()).then(|| {
            let (shown, hidden) = reveal(&typed, false);
            if hidden {
                (format!("It has hidden characters: {shown}"), true)
            } else {
                (format!("When a teammate wants to {}.", pattern_words(&typed)), false)
            }
        });
        let this = cx.entity();
        let tier_hint = TIERS[self.tier].2;
        let danger = dangerous(&typed, TIERS[self.tier].0);
        let go = cx.entity();
        let who = self.who_select(cx);
        card(cx)
            .p(px(18.0))
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(div().font_weight(FontWeight::MEDIUM).child("Add a rule"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(label("When a teammate wants to use"))
                    .child(div().font_family(theme.font_mono.clone()).child(text_input::field("rule-pattern", &pattern, 38.0, window, cx)))
                    .child(match explained {
                        Some((words, bad)) => div().text_size(px(text::CAPTION)).text_color(if bad { theme.bad } else { theme.ink }).child(words),
                        None => div().text_size(px(text::CAPTION)).text_color(theme.muted).child(
                            "A tool name (Edit, WebFetch), a command pattern (Bash(git status*): * matches anything), or a connector (mcp__github, or one of its tools).",
                        ),
                    })
                    .child(examples),
            )
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(16.0))
                    .child(div().flex().flex_col().gap(px(6.0)).child(label("For")).child(who))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.0))
                            .child(label("What happens"))
                            .child(div().flex().child(
                                Segmented::new("rule-tier", TIERS.iter().map(|(_, l, _)| ((*l).into(), None)).collect(), self.tier)
                                    .segment_width(112.0)
                                    .on_select(move |i, _, cx| {
                                        this.update(cx, |p, cx| {
                                            p.tier = i;
                                            cx.notify()
                                        })
                                    }),
                            ))
                            .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(tier_hint)),
                    ),
            )
            .child(div().flex().flex_col().gap(px(6.0)).child(label("Note")).child(text_input::field("rule-note", &note, 38.0, window, cx)))
            .when(danger, |el| el.child(danger_note(&theme)))
            .when_some(self.add_error.clone(), |el, e| el.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e)))
            .child({
                let cancel = cx.entity();
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(
                        Button::new("rule-add", if self.adding { "Adding…" } else { "Add rule" })
                            .primary()
                            .icon(icons::PLUS)
                            .disabled(self.adding || typed.is_empty())
                            .on_click(move |_, window, cx| go.update(cx, |p, cx| p.add(window, cx))),
                    )
                    .child(Button::new("rule-cancel", "Cancel").ghost().on_click(move |_, _, cx| {
                        cancel.update(cx, |p, cx| {
                            p.form_open = false;
                            p.add_error = None;
                            cx.notify()
                        })
                    }))
            })
            .into_any_element()
    }

    fn rule_row(&self, r: &Rule, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let id = r.id;
        let key = id.as_u128() as u64;
        let (shown, hidden) = reveal(&r.pattern, false);
        let tier = TIERS.iter().position(|(d, _, _)| *d == r.decision).unwrap_or(2);
        let rule = r.clone();
        let this = cx.entity();
        let del = cx.entity();
        let busy = self.busy.contains(&id);
        let mut row = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .px(px(16.0))
            .py(px(12.0))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.0))
                            .child(div().font_family(theme.font_mono.clone()).text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).text_color(if hidden { theme.bad } else { theme.ink }).child(shown))
                            .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(format!("When it wants to {}: {}.", pattern_words(&r.pattern), TIERS[tier].1.to_lowercase())))
                            .when_some(r.note.clone().filter(|n| !n.trim().is_empty()), |el, n| {
                                el.child(div().text_size(px(text::CAPTION)).text_color(theme.muted).italic().child(strip_hidden(&n, true)))
                            }),
                    )
                    .child(
                        Segmented::new(("rule-row-tier", key), TIERS.iter().map(|(_, l, _)| ((*l).into(), None)).collect(), tier)
                            .segment_width(98.0)
                            .on_select(move |i, _, cx| this.update(cx, |p, cx| p.set_tier(&rule, TIERS[i].0, cx))),
                    )
                    .child(
                        Button::icon_only(("rule-del", key), icons::TRASH)
                            .size(ButtonSize::Small)
                            .disabled(busy)
                            .tooltip("Remove")
                            .on_click(move |_, _, cx| {
                                del.update(cx, |p, cx| {
                                    p.confirm_delete = Some(id);
                                    cx.notify()
                                })
                            }),
                    ),
            )
            .when(dangerous(&r.pattern, r.decision), |el| el.child(danger_note(&theme)));
        if self.confirm_delete == Some(id) {
            let (yes, no) = (cx.entity(), cx.entity());
            let after = match r.decision {
                RuleDecision::Allow => "It asks you again from then on.",
                _ => "The usual behaviour applies again.",
            };
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().flex_1().min_w_0().text_size(px(text::SMALL)).text_color(theme.bad).child(format!("Remove this rule? {after}")))
                    .child(Button::new(("rule-del-no", key), "Cancel").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| {
                        no.update(cx, |p, cx| {
                            p.confirm_delete = None;
                            cx.notify()
                        })
                    }))
                    .child(Button::new(("rule-del-yes", key), "Remove").danger().size(ButtonSize::Small).on_click(move |_, _, cx| yes.update(cx, |p, cx| p.delete(id, cx)))),
            );
        }
        row.into_any_element()
    }

    /// One group of rules (every teammate's, or one teammate's), with its heading.
    fn group(&self, key: &str, head: AnyElement, rules: &[&Rule], i: usize, cx: &mut Context<Self>) -> AnyElement {
        let mut c = card(cx).flex().flex_col().overflow_hidden();
        for (k, r) in rules.iter().enumerate() {
            if k > 0 {
                c = c.child(divider(cx));
            }
            c = c.child(self.rule_row(r, cx));
        }
        anim::stagger(SharedString::from(format!("rules-{key}")), i, div().flex().flex_col().gap(px(10.0)).child(head).child(c)).into_any_element()
    }
}

impl Render for RulesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::count("RulesPage");
        let theme = Theme::of(cx).clone();
        let form = self.form_open.then(|| anim::appear("rules-form", div().child(self.add_form(window, cx))));
        let open = cx.entity();
        let head = div()
            .flex()
            .items_end()
            .gap(px(16.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.0))
                    .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).text_color(theme.ink).child("Rules"))
                    .child(div().text_size(px(text::LEAD)).text_color(theme.muted).child(
                        "What your teammates may do on their own. Without a rule, commands, file changes and browser or connector actions ask you first.",
                    )),
            )
            .when(!self.form_open, |el| {
                el.child(Button::new("rule-new", "Add a rule").primary().icon(icons::PLUS).on_click(move |_, _, cx| {
                    open.update(cx, |p, cx| {
                        p.form_open = true;
                        cx.notify()
                    })
                }))
            });
        let mut page = div().flex().flex_col().gap(px(28.0)).child(anim::appear("rules-head", head)).children(form);
        match (self.rules.clone(), self.error.clone()) {
            (None, Some(e)) => page = page.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e)),
            (None, None) => page = page.child(div().flex().flex_col().gap(px(10.0)).children((0..2).map(|_| Skeleton::new(72.0).radius(RADIUS_CARD)))),
            (Some(list), _) if list.is_empty() => {
                page = page.child(
                    div()
                        .px(px(16.0))
                        .py(px(16.0))
                        .rounded(px(RADIUS_CARD))
                        .border_1()
                        .border_dashed()
                        .border_color(theme.line)
                        .text_size(px(text::SMALL))
                        .text_color(theme.muted)
                        .child("No rules yet: everything that matters asks you first. \"Always allow\" on an approval adds one, or use Add a rule."),
                )
            }
            (Some(list), _) => {
                let mut i = 0;
                let every: Vec<&Rule> = list.iter().filter(|r| r.bot_id.is_none()).collect();
                if !every.is_empty() {
                    let head = group_head(None, "Every teammate", &theme);
                    page = page.child(self.group("every", head, &every, i, cx));
                    i += 1;
                }
                let bots = self.data.read(cx).bots().to_vec();
                // Each teammate's own, in the sidebar's order; rules of a teammate that is gone last.
                let mut seen: HashSet<Uuid> = HashSet::new();
                for b in &bots {
                    let own: Vec<&Rule> = list.iter().filter(|r| r.bot_id == Some(b.id)).collect();
                    seen.insert(b.id);
                    if own.is_empty() {
                        continue;
                    }
                    let head = group_head(Some((b.id, avatar_of(b))), &strip_hidden(&b.name, false), &theme);
                    page = page.child(self.group(&b.id.to_string(), head, &own, i, cx));
                    i += 1;
                }
                let orphans: Vec<&Rule> = list.iter().filter(|r| r.bot_id.is_some_and(|b| !seen.contains(&b))).collect();
                if !orphans.is_empty() {
                    let head = group_head(None, "A teammate no longer here", &theme);
                    page = page.child(self.group("gone", head, &orphans, i, cx));
                }
            }
        }
        let mut locked = div().flex().flex_col().gap(px(6.0));
        for l in LOCKED {
            locked = locked.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(text::SMALL))
                    .text_color(theme.ink)
                    .child(div().size(px(5.0)).flex_none().rounded_full().bg(theme.muted))
                    .child(l),
            );
        }
        page.child(
            card(cx)
                .p(px(18.0))
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(icon(icons::LOCK).size(px(16.0)).text_color(theme.accent))
                        .child(div().font_weight(FontWeight::MEDIUM).child("Always your call")),
                )
                .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child("These never run without you, whatever the rules say."))
                .child(locked),
        )
    }
}

/// A group's heading: a teammate's face and name, or a plain label.
fn group_head(who: Option<(Uuid, familiar_ui::mascot::Avatar)>, label: &str, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(match who {
            Some((id, avatar)) => Mascot::new(format!("rules-face-{id}"), avatar, MascotState::Idle, 28.0).into_any_element(),
            None => div()
                .size(px(28.0))
                .rounded_full()
                .bg(theme.accent_soft)
                .flex()
                .items_center()
                .justify_center()
                .child(icon(icons::SHIELD).size(px(15.0)).text_color(theme.accent))
                .into_any_element(),
        })
        .child(div().font_weight(FontWeight::MEDIUM).child(label.to_owned()))
        .into_any_element()
}

fn danger_note(theme: &Theme) -> AnyElement {
    div()
        .flex()
        .items_start()
        .gap(px(8.0))
        .px(px(12.0))
        .py(px(8.0))
        .rounded(px(RADIUS_CONTROL))
        .bg(theme.bad_soft)
        .child(div().pt(px(1.0)).child(icon(icons::DANGER_TRIANGLE).size(px(14.0)).text_color(theme.bad)))
        .child(div().flex_1().min_w_0().text_size(px(text::SMALL)).text_color(theme.bad).child(
            "This lets it run any command on your PC without asking (only the actions below still need you). Prefer narrow patterns like Bash(git status*).",
        ))
        .into_any_element()
}

// ---- pure ---------------------------------------------------------------------------------------------------------

/// A rule that lets a teammate run any command without asking: it matches Bash with any command, the way the engine
/// matches rules (`*` in the tool name or the argument matches anything: `*`, `Bash`, `Bash(*)`, `B*`, `*(*)`…).
pub fn dangerous(pattern: &str, decision: RuleDecision) -> bool {
    decision == RuleDecision::Allow && any_command(pattern)
}

/// The pattern covers Bash with any command at all.
pub fn any_command(pattern: &str) -> bool {
    let p = pattern.trim();
    if p == "*" {
        return true;
    }
    let (name, arg) = match p.split_once('(') {
        Some((n, rest)) => (n, rest.strip_suffix(')')),
        None => (p, None),
    };
    // Two unrelated commands: a pattern that matches both matches any command.
    glob(name, "Bash") && arg.is_none_or(|a| glob(a, "x7q; rm -rf zz") && glob(a, "git status"))
}

/// The engine's glob: `*` matches any run of characters; everything else is literal.
fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut rest = text;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(at) => rest = &rest[at + part.len()..],
                None => return false,
            }
        }
    }
    true
}

/// What a rule's pattern covers, after "When a teammate wants to …".
pub fn pattern_words(pattern: &str) -> String {
    let p = strip_hidden(pattern.trim(), false);
    let tick = |s: &str| format!("“{s}”");
    if p == "*" {
        return "use any tool".into();
    }
    let (name, arg) = match p.split_once('(') {
        Some((n, rest)) => (n.to_owned(), rest.strip_suffix(')').map(str::to_owned)),
        None => (p.clone(), None),
    };
    if name == "Bash" {
        return match arg.as_deref() {
            None | Some("*") => "run any command".into(),
            Some(a) if a.ends_with('*') && !a[..a.len() - 1].contains('*') => format!("run commands starting with {}", tick(a.trim_end_matches('*').trim_end())),
            Some(a) if a.contains('*') => format!("run commands like {}", tick(a)),
            Some(a) => format!("run exactly {}", tick(a)),
        };
    }
    if let Some(rest) = name.strip_prefix("mcp__") {
        let mut parts = rest.splitn(2, "__");
        let (server, tool) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
        let words = tool.replace(['_', '-'], " ");
        return match (server, tool) {
            ("browser", "" | "*") => "use the browser".into(),
            ("browser", t) => format!("use the browser to {}", t.trim_start_matches("browser_").replace('_', " ")),
            ("desktop", "" | "*") => "use your desktop".into(),
            ("desktop", _) => format!("use your desktop ({words})"),
            ("familiar", "" | "*") => "use Familiar's own tools".into(),
            (s, "" | "*") => format!("use any of {s}'s tools"),
            (s, t) if t.contains('*') => format!("use {s}'s tools like {}", tick(t)),
            (s, _) => format!("use {s}'s {words}"),
        };
    }
    if name.contains('*') {
        return if any_command(&p) {
            format!("run any command, and use any tool named like {}", tick(&p))
        } else {
            format!("use tools named like {}", tick(&p))
        };
    }
    let base = match name.as_str() {
        "Edit" | "MultiEdit" => "change files",
        "Write" => "create or overwrite files",
        "Read" => "read files",
        "NotebookEdit" => "change notebooks",
        "WebFetch" => "fetch web pages",
        "WebSearch" => "search the web",
        "Glob" | "Grep" => "search its files",
        "TodoWrite" => "keep its to-do list",
        _ => return format!("use {}", tick(&p)),
    };
    match arg.as_deref() {
        None | Some("*") => base.into(),
        Some(a) => format!("{base} matching {}", tick(a)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_in_words() {
        assert_eq!(pattern_words("*"), "use any tool");
        assert_eq!(pattern_words("Bash"), "run any command");
        assert_eq!(pattern_words("Bash(*)"), "run any command");
        assert_eq!(pattern_words("Bash(git status*)"), "run commands starting with “git status”");
        assert_eq!(pattern_words("Bash(npm run test)"), "run exactly “npm run test”");
        assert_eq!(pattern_words("Bash(*deploy*)"), "run commands like “*deploy*”");
        assert_eq!(pattern_words("Edit"), "change files");
        assert_eq!(pattern_words("Edit(docs/*)"), "change files matching “docs/*”");
        assert_eq!(pattern_words("WebFetch"), "fetch web pages");
        assert_eq!(pattern_words("mcp__github"), "use any of github's tools");
        assert_eq!(pattern_words("mcp__github__*"), "use any of github's tools");
        assert_eq!(pattern_words("mcp__github__list_issues"), "use github's list issues");
        assert_eq!(pattern_words("mcp__browser__browser_click"), "use the browser to click");
        assert_eq!(pattern_words("mcp__desktop__screenshot"), "use your desktop (screenshot)");
        assert_eq!(pattern_words("SomethingElse"), "use “SomethingElse”");
        // Hidden characters don't make it into the words.
        assert_eq!(pattern_words("Bash(ls\u{202E}*)"), "run commands starting with “ls”");
    }

    #[test]
    fn what_counts_as_dangerous() {
        for p in ["*", "Bash", " Bash(*) "] {
            assert!(dangerous(p, RuleDecision::Allow), "{p}");
            assert!(!dangerous(p, RuleDecision::Ask), "{p}");
        }
        assert!(!dangerous("Bash(git status*)", RuleDecision::Allow));
        // Patterns the engine reads as "any command" too.
        for p in ["B*", "Ba*", "Bash(**)", "*(*)", "**", "*sh"] {
            assert!(dangerous(p, RuleDecision::Allow), "{p}");
        }
        for p in ["Bash(git *)", "Edit", "W*", "mcp__github", "Bash(*status)"] {
            assert!(!dangerous(p, RuleDecision::Allow), "{p}");
        }
        assert_eq!(pattern_words("B*"), "run any command, and use any tool named like “B*”");
        assert_eq!(pattern_words("Web*"), "use tools named like “Web*”");
        assert_eq!(TIERS.iter().map(|(d, _, _)| d.as_str()).collect::<Vec<_>>(), ["allow", "review", "ask", "deny"]);
    }
}
