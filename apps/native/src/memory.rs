//! A teammate's "What I learned" tab (the web's `pages/Memory.tsx`): memories it proposed (accept, edit inline, or
//! reject), what it remembers (with forget), teaching it something, and "Dream now" (review recent chats) with when
//! it last did, and the skills it wrote ([`crate::skills`]). Rows that leave fold away; new ones rise in.

use std::collections::HashMap;
use std::time::Duration;

use familiar_client::{Memory, MemoryPatch};
use familiar_ui::anim::{self, Expand};
use familiar_ui::components::{Button, ButtonSize, SectionHeader, Skeleton, card, chip, divider, empty};
use familiar_ui::icons;
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::theme::{RADIUS_CARD, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Window,
    div, prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, TextareaState};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::data::{AppData, DataEvent, ago, avatar_of, swr};
use crate::text_input;

/// How long a leaving row takes to fold away before it is dropped.
const LEAVE: Duration = Duration::from_millis(300);

pub struct MemoryTab {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    bot: Uuid,
    list: Vec<Memory>,
    loaded: bool,
    /// Rows folding away (accepted, rejected or forgotten), until the list no longer has them.
    /// (The status it left from: an accepted memory comes back as active and must not fold there.)
    leaving: HashMap<Uuid, (Expand, Option<String>)>,
    /// The proposed memory being edited, with its own field.
    editing: Option<(Uuid, Entity<TextareaState>)>,
    teach: Entity<TextareaState>,
    dreaming: bool,
    /// "Skills it wrote".
    skills: Entity<crate::skills::SkillsSection>,
}

impl MemoryTab {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, bot: Uuid, window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::data::redraw_on_updates(&data, cx);
        cx.subscribe(&data, |this: &mut Self, _, ev: &DataEvent, cx| match ev {
            DataEvent::Changed(None) => this.reload(cx),
            DataEvent::Changed(Some(n)) if n.t == "memories" && n.bot.as_deref().is_none_or(|b| b == this.bot.to_string()) => {
                this.reload(cx)
            }
            _ => {}
        })
        .detach();
        let teach = text_input::new_field("Teach it something: I prefer short answers. My timezone is Europe/Berlin.", true, 4, window, cx);
        cx.subscribe_in(&teach, window, |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { shift: false, .. } => this.remember(window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        })
        .detach();
        let mut this =
            Self { data: data.clone(), toasts, bot, list: Vec::new(), loaded: false, leaving: HashMap::new(), editing: None, teach, dreaming: false, skills: cx.new(|cx| crate::skills::SkillsSection::new(data, bot, cx)) };
        this.reload(cx);
        this
    }

    fn client(&self, cx: &Context<Self>) -> familiar_client::Client {
        self.data.read(cx).client.clone()
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let client = self.client(cx);
        swr(self, &client, format!("/api/bots/{}/memories", self.bot), cx, |this, list: Vec<Memory>, _| {
            this.list = list;
            this.loaded = true;
            // Rows that are really gone (or moved to another status) stop folding.
            let list = &this.list;
            this.leaving.retain(|id, (_, from)| list.iter().any(|m| m.id == *id && m.status == *from));
        });
    }

    fn toast(&self, tone: Tone, title: impl Into<SharedString>, body: Option<String>, cx: &mut Context<Self>) {
        let title = title.into();
        self.toasts.update(cx, |t, cx| t.push(tone, title, body.map(Into::into), cx));
    }

    /// Fold the row away, then drop it from the list (the reload confirms or brings it back).
    fn leave(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let mut e = Expand::new(true);
        e.set_open(false);
        let from = self.list.iter().find(|m| m.id == id).and_then(|m| m.status.clone());
        self.leaving.insert(id, (e, from.clone()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LEAVE).await;
            let _ = this.update(cx, |this, cx| {
                if this.leaving.contains_key(&id) {
                    this.list.retain(|m| !(m.id == id && m.status == from));
                    this.leaving.remove(&id);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Run a call; on failure put the list back and say so; reload either way.
    fn call(
        &mut self,
        ok: Option<&'static str>,
        cx: &mut Context<Self>,
        f: impl FnOnce(familiar_client::Client) -> futures::future::BoxFuture<'static, Result<(), familiar_client::ApiError>>
        + Send
        + 'static,
    ) {
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move { f(client).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |this, cx| {
                match r {
                    Ok(()) => {
                        if let Some(ok) = ok {
                            this.toast(Tone::Ok, ok, None, cx);
                        }
                    }
                    Err(e) => {
                        this.leaving.clear();
                        this.toast(Tone::Bad, "Couldn't save that", Some(e), cx);
                    }
                }
                this.reload(cx);
            });
        })
        .detach();
    }

    fn decide(&mut self, id: Uuid, accept: bool, content: Option<String>, cx: &mut Context<Self>) {
        self.editing = None;
        self.leave(id, cx);
        let patch = MemoryPatch { content: content.clone(), status: Some(if accept { "active" } else { "rejected" }.into()) };
        // Accepted: it shows up under Remembered at once.
        if accept && let Some(m) = self.list.iter().find(|m| m.id == id).cloned() {
            let fresh = Memory { id: Uuid::new_v4(), status: Some("active".into()), content: content.unwrap_or(m.content), ..m };
            self.list.push(fresh);
        }
        self.call(Some(if accept { "Remembered" } else { "Dismissed" }), cx, move |c| {
            Box::pin(async move { c.update_memory(id, &patch).await.map(|_| ()) })
        });
    }

    fn forget(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.leave(id, cx);
        self.call(None, cx, move |c| Box::pin(async move { c.delete_memory(id).await }));
    }

    fn remember(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.teach.read(cx).value().trim().to_owned();
        if content.is_empty() {
            return;
        }
        self.teach.update(cx, |s, cx| s.set_value("", window, cx));
        let bot = self.bot;
        self.list.insert(0, Memory { id: Uuid::new_v4(), bot_id: bot, content: content.clone(), source: "user".into(), status: Some("active".into()), created_at: chrono::Utc::now() });
        cx.notify();
        self.call(None, cx, move |c| Box::pin(async move { c.create_memory(bot, &content).await.map(|_| ()) }));
    }

    fn edit(&mut self, m: &Memory, window: &mut Window, cx: &mut Context<Self>) {
        let field = text_input::new_field("", false, 8, window, cx);
        field.update(cx, |s, cx| {
            s.set_value(m.content.clone(), window, cx);
            s.focus(window, cx);
        });
        cx.subscribe(&field, |_, _, _: &InputEvent, cx| cx.notify()).detach();
        self.editing = Some((m.id, field));
        cx.notify();
    }

    fn dream(&mut self, cx: &mut Context<Self>) {
        if self.dreaming {
            return;
        }
        self.dreaming = true;
        cx.notify();
        let client = self.client(cx);
        let bot = self.bot;
        let name = self.data.read(cx).bot(bot).map(|b| b.name.clone()).unwrap_or_else(|| "It".into());
        let task = Tokio::spawn(cx, async move { client.dream(bot).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |this, cx| {
                this.dreaming = false;
                match r {
                    Ok(_) => this.toast(Tone::Ok, format!("{name} is going over recent chats"), None, cx),
                    Err(e) => this.toast(Tone::Bad, "Couldn't start a dream", Some(e), cx),
                }
                this.data.update(cx, |d, cx| d.reload_runs(cx));
                cx.notify();
            });
        })
        .detach();
    }

    /// A row that folds away while it is leaving.
    fn wrap(&self, id: Uuid, index: usize, row: impl IntoElement, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let row = anim::stagger(SharedString::from(format!("mem-in-{id}")), index, div().child(row));
        match self.leaving.get(&id) {
            Some((e, _)) => e.render(SharedString::from(format!("mem-leave-{id}")), window, cx, row).into_any_element(),
            None => row.into_any_element(),
        }
    }

    fn proposed_row(&self, m: &Memory, index: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let id = m.id;
        let this = cx.entity();
        let editing = self.editing.as_ref().filter(|(e, _)| *e == id).map(|(_, f)| f.clone());
        let mut body = div().flex().flex_col().gap(px(10.0)).px(px(16.0)).py(px(14.0));
        let buttons = match &editing {
            Some(field) => {
                body = body.child(text_input::field(("mem-edit", id.as_u128() as u64), field, 70.0, window, cx));
                let blank = field.read(cx).value().trim().is_empty();
                let (t1, t2, f) = (this.clone(), this.clone(), field.clone());
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(
                        Button::new(("mem-save", id.as_u128() as u64), "Save and accept")
                            .primary()
                            .size(ButtonSize::Small)
                            .icon(icons::CHECK)
                            .disabled(blank)
                            .on_click(move |_, _, cx| {
                                let text = f.read(cx).value().trim().to_owned();
                                t1.update(cx, |p, cx| p.decide(id, true, Some(text), cx))
                            }),
                    )
                    .child(Button::new(("mem-cancel", id.as_u128() as u64), "Cancel").size(ButtonSize::Small).ghost().on_click(
                        move |_, _, cx| {
                            t2.update(cx, |p, cx| {
                                p.editing = None;
                                cx.notify()
                            })
                        },
                    ))
            }
            None => {
                body = body.child(div().text_color(theme.ink).child(m.content.clone()));
                let (t1, t2, t3) = (this.clone(), this.clone(), this.clone());
                let mem = m.clone();
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(
                        Button::new(("mem-accept", id.as_u128() as u64), "Accept")
                            .primary()
                            .size(ButtonSize::Small)
                            .icon(icons::CHECK)
                            .on_click(move |_, _, cx| t1.update(cx, |p, cx| p.decide(id, true, None, cx))),
                    )
                    .child(
                        Button::new(("mem-edit-btn", id.as_u128() as u64), "Edit")
                            .size(ButtonSize::Small)
                            .icon(icons::PEN)
                            .on_click(move |_, window, cx| t2.update(cx, |p, cx| p.edit(&mem, window, cx))),
                    )
                    .child(
                        Button::new(("mem-reject", id.as_u128() as u64), "Reject")
                            .size(ButtonSize::Small)
                            .ghost()
                            .on_click(move |_, _, cx| t3.update(cx, |p, cx| p.decide(id, false, None, cx))),
                    )
            }
        };
        body = body.child(buttons);
        self.wrap(id, index, body, window, cx)
    }

    fn active_row(&self, m: &Memory, index: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let id = m.id;
        let this = cx.entity();
        let learned = m.source == "bot";
        let row = div()
            .flex()
            .items_start()
            .gap(px(12.0))
            .px(px(16.0))
            .py(px(12.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.0))
                    .child(div().text_color(theme.ink).child(m.content.clone()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(chip(if learned { Tone::Accent } else { Tone::Muted }, if learned { "learned" } else { "you taught" }, cx))
                            .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(ago(Some(m.created_at)))),
                    ),
            )
            .child(
                Button::new(("mem-forget", id.as_u128() as u64), "Forget")
                    .size(ButtonSize::Small)
                    .ghost()
                    .on_click(move |_, _, cx| this.update(cx, |p, cx| p.forget(id, cx))),
            );
        self.wrap(id, index, row, window, cx)
    }

    /// A card of rows with hairlines between them.
    fn list_card(rows: Vec<AnyElement>, accent: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let mut c = card(cx).flex().flex_col().overflow_hidden().when(accent, |el| el.border_color(theme.accent.opacity(0.45)));
        for (i, r) in rows.into_iter().enumerate() {
            if i > 0 {
                c = c.child(divider(cx));
            }
            c = c.child(r);
        }
        c.into_any_element()
    }
}

impl Render for MemoryTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let (name, avatar, last_dreamed) = {
            let d = self.data.read(cx);
            match d.bot(self.bot) {
                Some(b) => (b.name.clone(), avatar_of(b), b.last_dreamed_at),
                None => ("Your teammate".into(), familiar_ui::mascot::default_avatar(&self.bot.to_string()), None),
            }
        };
        let this = cx.entity();
        let head = card(cx)
            .p(px(16.0))
            .flex()
            .items_center()
            .gap(px(14.0))
            .child(Mascot::new(
                format!("mem-head-{}", self.bot),
                avatar,
                if self.dreaming { MascotState::Working } else { MascotState::Idle },
                48.0,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(div().font_weight(FontWeight::MEDIUM).child(format!("What {name} has learned")))
                    .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child(format!(
                        "{} Each night it looks back over recent chats and proposes what to remember.",
                        match last_dreamed {
                            Some(t) => format!("Last dreamed {}.", ago(Some(t))),
                            None => "Hasn't dreamed yet.".to_owned(),
                        }
                    ))),
            )
            .child(
                Button::new("dream-now", if self.dreaming { "Starting…" } else { "Dream now" })
                    .icon(icons::MAGIC_STICK_3)
                    .disabled(self.dreaming)
                    .on_click(move |_, _, cx| this.update(cx, |p, cx| p.dream(cx))),
            );

        let mut page = div().flex().flex_col().gap(px(28.0)).child(anim::appear("mem-head", head));

        if !self.loaded {
            return page.child(
                div().flex().flex_col().gap(px(10.0)).children((0..3).map(|_| Skeleton::new(56.0).radius(RADIUS_CARD))),
            );
        }

        let proposed: Vec<Memory> = self.list.iter().filter(|m| m.status.as_deref() == Some("proposed")).cloned().collect();
        let active: Vec<Memory> =
            self.list.iter().filter(|m| m.status.as_deref().is_none_or(|s| s == "active")).cloned().collect();

        if !proposed.is_empty() {
            let rows = proposed.iter().enumerate().map(|(i, m)| self.proposed_row(m, i, window, cx)).collect();
            let shown = proposed.iter().filter(|m| !self.leaving.contains_key(&m.id)).count();
            page = page.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(SectionHeader::new("Proposed").count(shown))
                    .child(
                        div()
                            .mt(px(-4.0))
                            .text_size(px(text::SMALL))
                            .text_color(theme.muted)
                            .child("Nothing here is used until you accept it."),
                    )
                    .child(Self::list_card(rows, true, cx)),
            );
        }

        let blank = self.teach.read(cx).value().trim().is_empty();
        let this = cx.entity();
        let teach = div()
            .flex()
            .items_end()
            .gap(px(10.0))
            .child(div().flex_1().min_w_0().child(text_input::field("mem-teach", &self.teach, 44.0, window, cx)))
            .child(
                Button::new("mem-remember", "Remember")
                    .primary()
                    .icon(icons::PLUS)
                    .disabled(blank)
                    .on_click(move |_, window, cx| this.update(cx, |p, cx| p.remember(window, cx))),
            );
        let remembered = if active.is_empty() {
            anim::appear(
                "mem-empty",
                empty(
                    "Nothing remembered yet",
                    Some("Tell it something worth keeping, or let it suggest things after a few chats.".into()),
                    cx,
                ),
            )
            .into_any_element()
        } else {
            let rows = active.iter().enumerate().map(|(i, m)| self.active_row(m, i, window, cx)).collect();
            Self::list_card(rows, false, cx)
        };
        page.child(
            div()
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(SectionHeader::new("Remembered").count(active.iter().filter(|m| !self.leaving.contains_key(&m.id)).count()))
                .child(teach)
                .child(remembered),
        )
        .child(self.skills.clone())
    }
}

impl MemoryTab {
    /// "Skills it wrote" (the bench's shot opens one).
    pub fn skills(&self) -> Entity<crate::skills::SkillsSection> {
        self.skills.clone()
    }
}
