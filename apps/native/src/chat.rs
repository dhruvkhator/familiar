//! A teammate's page (the web's `pages/Chat.tsx`): a hero header with the mascot's live state, the thread list, and
//! the selected thread's transcript (messages oldest → newest, then the latest run's card).


use familiar_client::{Message, Role, Run, RunKind, Thread};
use familiar_ui::anim;
use familiar_ui::components::{Button, ButtonSize, SidebarItem, Skeleton, StatusChip, card, chip, empty, group_label};
use familiar_ui::edge_fade::edge_faded;
use familiar_ui::icons;
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::theme::{RADIUS_CARD, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _,
    px,
};
use uuid::Uuid;

use crate::data::{self, AppData, DataEvent, ago, excerpt, run_status, swr};
use crate::markdown;
use crate::shell::state_tone;

pub struct BotPage {
    data: Entity<AppData>,
    #[allow(dead_code)]
    toasts: Entity<ToastStack>,
    bot: Uuid,
    threads: Vec<Thread>,
    threads_loaded: bool,
    selected: Option<Uuid>,
    messages: Vec<Message>,
    messages_loaded: bool,
    /// The selected thread's runs, newest first (only the first is shown).
    runs: Vec<Run>,
    scroll: ScrollHandle,
    thread_scroll: ScrollHandle,
    /// Follow new content while the transcript is scrolled to (near) the bottom.
    stick: bool,
}

impl BotPage {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, bot: Uuid, cx: &mut Context<Self>) -> Self {
        cx.observe(&data, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&data, Self::on_data).detach();
        let mut this = Self {
            data,
            toasts,
            bot,
            threads: Vec::new(),
            threads_loaded: false,
            selected: None,
            messages: Vec::new(),
            messages_loaded: false,
            runs: Vec::new(),
            scroll: ScrollHandle::new(),
            thread_scroll: ScrollHandle::new(),
            stick: true,
        };
        this.reload_threads(cx);
        this
    }

    fn client(&self, cx: &Context<Self>) -> familiar_client::Client {
        self.data.read(cx).client.clone()
    }

    fn on_data(&mut self, _: Entity<AppData>, ev: &DataEvent, cx: &mut Context<Self>) {
        match ev {
            DataEvent::Changed(None) => {
                self.reload_threads(cx);
                self.reload_thread(cx);
            }
            DataEvent::Changed(Some(n)) => {
                let mine = n.bot.as_deref().is_none_or(|b| b == self.bot.to_string());
                match n.t.as_str() {
                    "threads" if mine => self.reload_threads(cx),
                    "messages" => self.reload_messages(cx),
                    "runs" if mine => {
                        self.reload_runs(cx);
                        self.reload_threads(cx);
                    }
                    _ => {}
                }
            }
            DataEvent::Delta(_) => {}
        }
    }

    // ---- loading ----------------------------------------------------------------------------------------------

    fn reload_threads(&mut self, cx: &mut Context<Self>) {
        let client = self.client(cx);
        swr(self, &client, format!("/api/bots/{}/threads", self.bot), cx, |this, threads: Vec<Thread>, cx| {
            this.threads = threads;
            this.threads_loaded = true;
            if this.selected.is_none_or(|s| !this.threads.iter().any(|t| t.id == s)) {
                // The newest conversation that isn't a scheduled/dream log, else the newest thread.
                let pick = this
                    .threads
                    .iter()
                    .find(|t| t.source.as_deref() != Some("schedule") && t.schedule_id.is_none())
                    .or(this.threads.first())
                    .map(|t| t.id);
                if pick != this.selected {
                    this.selected = pick;
                    this.reset_thread(cx);
                }
            }
        });
    }

    pub fn select(&mut self, thread: Uuid, cx: &mut Context<Self>) {
        if self.selected != Some(thread) {
            self.selected = Some(thread);
            self.reset_thread(cx);
            cx.notify();
        }
    }

    fn reset_thread(&mut self, cx: &mut Context<Self>) {
        self.messages.clear();
        self.messages_loaded = false;
        self.runs.clear();
        self.stick = true;
        self.scroll.scroll_to_bottom();
        self.reload_thread(cx);
    }

    fn reload_thread(&mut self, cx: &mut Context<Self>) {
        self.reload_messages(cx);
        self.reload_runs(cx);
    }

    fn reload_messages(&mut self, cx: &mut Context<Self>) {
        let Some(tid) = self.selected else { return };
        let client = self.client(cx);
        swr(self, &client, format!("/api/threads/{tid}/messages?limit=200"), cx, move |this, mut list: Vec<Message>, _| {
            if this.selected != Some(tid) {
                return;
            }
            list.sort_by_key(|m| m.created_at);
            if list.len() != this.messages.len() && this.stick {
                this.scroll.scroll_to_bottom();
            }
            this.messages = list;
            this.messages_loaded = true;
        });
    }

    fn reload_runs(&mut self, cx: &mut Context<Self>) {
        let Some(tid) = self.selected else { return };
        let client = self.client(cx);
        swr(self, &client, format!("/api/threads/{tid}/runs?limit=5"), cx, move |this, runs: Vec<Run>, _| {
            if this.selected == Some(tid) {
                this.runs = runs;
            }
        });
    }

    /// Track whether the reader left the bottom (more than 80 px up) so new content doesn't yank them back.
    fn update_stick(&mut self) {
        let max = f32::from(self.scroll.max_offset().y);
        let at = -f32::from(self.scroll.offset().y);
        self.stick = max - at <= 80.0;
    }

    // ---- rendering ------------------------------------------------------------------------------------------------

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let d = self.data.read(cx);
        let Some(bot) = d.bots().iter().find(|b| b.id == self.bot).cloned() else {
            return div().into_any_element();
        };
        let t = d.teammate(&bot);
        let active = d.runs.iter().find(|r| r.bot_id == self.bot && r.status.is_active()).cloned();
        let status_line = match &active {
            Some(r) => format!("Working on “{}”", excerpt(r.prompt.as_deref().unwrap_or("a task"), 70)),
            None if t.state == MascotState::NeedsYou => "Waiting on you".to_owned(),
            None if t.state == MascotState::Paused => "Paused".to_owned(),
            None => format!("Last active {}", ago(bot.last_run_at)),
        };
        div()
            .flex()
            .items_center()
            .gap(px(20.0))
            .px(px(32.0))
            .pt(px(28.0))
            .pb(px(20.0))
            .border_b_1()
            .border_color(theme.line)
            .child(anim::appear(
                SharedString::from(format!("hero-{}", self.bot)),
                div().child(Mascot::new(format!("hero-{}", self.bot), t.avatar, t.state, 96.0)),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .text_size(px(text::DISPLAY))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.ink)
                                    .child(t.name.clone()),
                            )
                            .child(chip(state_tone(t.state), t.state.label(), cx))
                            .when_some(t.model.clone(), |el, m| el.child(chip(Tone::Muted, m, cx))),
                    )
                    .child(
                        div()
                            .text_size(px(text::LEAD))
                            .text_color(if active.is_some() { theme.accent } else { theme.muted })
                            .truncate()
                            .child(status_line),
                    )
                    .when_some(bot.persona.clone().filter(|p| !p.trim().is_empty()), |el, p| {
                        el.child(
                            div().text_size(px(text::SMALL)).text_color(theme.muted).line_clamp(1).child(excerpt(&p, 160)),
                        )
                    }),
            )
            .into_any_element()
    }

    fn thread_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        let mut list = div().flex().flex_col().gap(px(2.0));
        if !self.threads_loaded {
            for i in 0..4 {
                list = list.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .h(px(48.0))
                        .px(px(12.0))
                        .justify_center()
                        .child(Skeleton::new(10.0).width(120.0 - i as f32 * 14.0))
                        .child(Skeleton::new(8.0).width(60.0)),
                );
            }
        } else if self.threads.is_empty() {
            list = list.child(
                div().px(px(12.0)).py(px(8.0)).text_size(px(text::SMALL)).text_color(theme.muted).child("No threads yet."),
            );
        }
        for (i, t) in self.threads.iter().enumerate() {
            let source = t.source.clone().or_else(|| t.schedule_id.map(|_| "schedule".into()));
            let sub = match source.as_deref() {
                Some(s) if s != "web" && s != "app" => format!("{s} · {}", ago(Some(t.updated_at))),
                _ => ago(Some(t.updated_at)),
            };
            let id = t.id;
            let this = this.clone();
            list = list.child(anim::stagger(
                SharedString::from(format!("thread-in-{}", t.id)),
                i,
                div().child(
                    SidebarItem::new(
                        SharedString::from(format!("thread-{}", t.id)),
                        t.title.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "Untitled".into()),
                    )
                    .icon(icons::CHAT_ROUND_LINE)
                    .sublabel(sub, None)
                    .selected(self.selected == Some(t.id))
                    .on_click(move |_, _, cx| this.update(cx, |p, cx| p.select(id, cx))),
                ),
            ));
        }
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(236.0))
            .h_full()
            .border_r_1()
            .border_color(theme.line)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(8.0))
                    .pt(px(14.0))
                    .pb(px(8.0))
                    .child(group_label("Threads", cx))
                    .child(Button::icon_only("new-thread", icons::PLUS).size(ButtonSize::Small).tooltip("New thread")),
            )
            .child(
                edge_faded(
                    14.0,
                    true,
                    true,
                    div()
                        .id("thread-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.thread_scroll)
                        .px(px(8.0))
                        .pb(px(12.0))
                        .child(list),
                )
                .fade_overflow_y(&self.thread_scroll),
            )
            .into_any_element()
    }

    fn message(&self, m: &Message, name: &SharedString, avatar: familiar_ui::mascot::Avatar, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let id = SharedString::from(format!("msg-{}", m.id));
        if m.role == Role::User {
            return anim::appear(
                id,
                div().flex().justify_end().child(
                    div()
                        .max_w(px(520.0))
                        .px(px(14.0))
                        .py(px(9.0))
                        .rounded(px(16.0))
                        .rounded_br(px(6.0))
                        .bg(theme.accent_soft)
                        .text_color(theme.ink)
                        .child(m.content.clone()),
                ),
            )
            .into_any_element();
        }
        let who: SharedString = if m.role == Role::System { "System".into() } else { name.clone() };
        anim::appear(
            id,
            div()
                .flex()
                .gap(px(12.0))
                .items_start()
                .child(Mascot::new(format!("msg-{}", m.id), avatar, MascotState::Idle, 30.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(text::CAPTION))
                                .text_color(theme.muted)
                                .child(SharedString::from(format!("{who} · {}", ago(Some(m.created_at))))),
                        )
                        .child(
                            div()
                                .max_w(px(600.0))
                                .px(px(14.0))
                                .py(px(10.0))
                                .rounded(px(16.0))
                                .rounded_tl(px(6.0))
                                .bg(theme.surface)
                                .border_1()
                                .border_color(theme.line)
                                .child(markdown::render(&m.content, &theme)),
                        ),
                ),
        )
        .into_any_element()
    }

    fn run_card(&self, run: &Run, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let kind = match run.kind {
            RunKind::Handoff => "handoff".to_owned(),
            RunKind::Unknown => "run".to_owned(),
            k => format!("{} run", k.as_str()),
        };
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(StatusChip::new(run_status(run.status)))
                    .child(chip(Tone::Muted, kind, cx)),
            );
        if let Some(e) = run.error.clone().filter(|e| !e.trim().is_empty()) {
            body = body.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e));
        } else if !run.status.is_active() {
            body = body.child(
                div()
                    .text_size(px(text::SMALL))
                    .text_color(theme.muted)
                    .child(SharedString::from(format!("Finished {}.", ago(run.finished_at)))),
            );
        }
        anim::appear(SharedString::from(format!("run-{}", run.id)), card(cx).px(px(14.0)).py(px(12.0)).child(body))
            .into_any_element()
    }

    fn transcript(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let (name, avatar) = {
            let d = self.data.read(cx);
            match d.bots().iter().find(|b| b.id == self.bot) {
                Some(b) => (SharedString::from(b.name.clone()), data::avatar_of(b)),
                None => ("".into(), familiar_ui::mascot::default_avatar(&self.bot.to_string())),
            }
        };
        let mut col = div().flex().flex_col().gap(px(18.0));
        if self.selected.is_none() && self.threads_loaded {
            col = col.child(anim::appear(
                "chat-none",
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.0))
                    .py(px(48.0))
                    .child(Mascot::new("chat-none", avatar, MascotState::Idle, 88.0))
                    .child(
                        div()
                            .text_size(px(text::TITLE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from(format!("Chat with {name}"))),
                    )
                    .child(div().text_color(theme.muted).child("Start a thread and say hello.")),
            ));
        } else if !self.messages_loaded {
            for (i, w) in [(true, 220.0), (false, 420.0), (true, 160.0), (false, 360.0)].into_iter().enumerate() {
                let (user, width) = w;
                col = col.child(
                    div()
                        .flex()
                        .when(user, |el| el.justify_end())
                        .gap(px(12.0))
                        .when(!user, |el| el.child(Skeleton::new(30.0).width(30.0).radius(15.0)))
                        .child(Skeleton::new(if i % 2 == 0 { 40.0 } else { 64.0 }).width(width).radius(RADIUS_CARD)),
                );
            }
        } else {
            if self.messages.is_empty() && self.runs.is_empty() {
                col = col.child(anim::appear(
                    "chat-empty",
                    empty(
                        SharedString::from(format!("Say hello to {name}")),
                        Some("Ask for something, or tell it what to keep an eye on.".into()),
                        cx,
                    ),
                ));
            }
            for m in self.messages.clone() {
                col = col.child(self.message(&m, &name, avatar, cx));
            }
            if let Some(run) = self.runs.first().cloned() {
                col = col.child(self.run_card(&run, cx));
            }
        }
        let this = cx.entity();
        edge_faded(
            20.0,
            true,
            true,
            div()
                .id("transcript")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .on_scroll_wheel(move |_, _, cx| this.update(cx, |p, _| p.update_stick()))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .justify_center()
                        .px(px(28.0))
                        .py(px(24.0))
                        .child(div().w_full().max_w(px(720.0)).child(col)),
                ),
        )
        .fade_overflow_y(&self.scroll)
        .into_any_element()
    }
}

impl Render for BotPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.header(cx);
        let threads = self.thread_list(cx);
        let transcript = self.transcript(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(threads)
                    .child(div().flex_1().min_w_0().h_full().flex().flex_col().child(div().flex_1().min_h_0().child(transcript))),
            )
    }
}
