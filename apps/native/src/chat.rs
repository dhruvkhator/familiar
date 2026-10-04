//! A teammate's page (the web's `pages/Chat.tsx`): a hero header with the mascot's live state, the thread list, the
//! selected thread's transcript (messages oldest → newest, the pending optimistic bubble, then the latest run's card
//! with its live events, streamed text and approvals) and the composer.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use familiar_client::{Event, Message, Role, Run, RunKind, RunStatus, Thread, TypedEvent};
use familiar_ui::anim::{self, Expand};
use familiar_ui::components::{Button, ButtonSize, Segmented, SidebarItem, Skeleton, StatusChip, card, chip, empty, group_label};
use familiar_ui::edge_fade::edge_faded;
use familiar_ui::icons::{self, icon};
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::motion::{AnimationExt as _, EASE, MotionSpec};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CHIP, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, TextareaState};
use gpui_tokio::Tokio;
use serde_json::Value;
use uuid::Uuid;

use crate::approval::ApprovalCards;
use crate::bot_settings::BotSettings;
use crate::data::{self, AppData, DataEvent, ago, excerpt, run_status, swr};
use crate::markdown;
use crate::shell::state_tone;
use crate::text_input;

/// The caret of the streaming text.
const CARET_BLINK: MotionSpec = MotionSpec::new(1000, EASE);

/// A message posted but not yet seen back from the server.
struct Outgoing {
    content: String,
    known: Vec<Uuid>,
    at: Instant,
}

pub struct BotPage {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    bot: Uuid,
    threads: Vec<Thread>,
    threads_loaded: bool,
    selected: Option<Uuid>,
    messages: Vec<Message>,
    messages_loaded: bool,
    /// The selected thread's runs, newest first (only the first is shown).
    runs: Vec<Run>,
    /// Persisted events of the shown run while it is active, by seq.
    events: Vec<Event>,
    events_run: Option<Uuid>,
    tools: HashMap<i64, Expand>,
    approvals: ApprovalCards,
    composer: Entity<TextareaState>,
    outgoing: Vec<Outgoing>,
    sending: bool,
    scroll: ScrollHandle,
    thread_scroll: ScrollHandle,
    /// What the transcript showed last frame; a change while at the bottom scrolls to the new bottom.
    content_rev: (usize, usize, usize, usize, bool),
    force_bottom: bool,
    /// 0: chat, 1: settings.
    tab: usize,
    settings: Option<Entity<BotSettings>>,
    settings_scroll: ScrollHandle,
}

impl BotPage {
    pub fn new(
        data: Entity<AppData>,
        toasts: Entity<ToastStack>,
        bot: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&data, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&data, Self::on_data).detach();
        let name = data.read(cx).bot(bot).map(|b| b.name.clone()).unwrap_or_else(|| "your teammate".into());
        let composer = text_input::new_field(format!("Message {name}"), true, 6, window, cx);
        cx.subscribe_in(&composer, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { shift: false, .. } => this.send(window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        })
        .detach();
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
            events: Vec::new(),
            events_run: None,
            tools: HashMap::new(),
            approvals: ApprovalCards::default(),
            composer,
            outgoing: Vec::new(),
            sending: false,
            scroll: ScrollHandle::new(),
            thread_scroll: ScrollHandle::new(),
            content_rev: Default::default(),
            force_bottom: true,
            tab: 0,
            settings: None,
            settings_scroll: ScrollHandle::new(),
        };
        this.reload_threads(cx);
        // Opening a teammate puts the cursor in the composer.
        cx.defer_in(window, |this, window, cx| this.composer.update(cx, |s, cx| s.focus(window, cx)));
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
                    "events" if self.events_run.is_some_and(|r| n.run.as_deref() == Some(&r.to_string())) => {
                        self.reload_events(cx)
                    }
                    _ => {}
                }
            }
            DataEvent::Delta(run) if self.events_run == Some(*run) => cx.notify(),
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
        self.events.clear();
        self.events_run = None;
        self.outgoing.clear();
        self.force_bottom = true;
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
            this.messages = list;
            this.messages_loaded = true;
            this.prune_outgoing();
        });
    }

    /// Drop optimistic bubbles whose message came back (same content, new id), or that are older than 6 s.
    fn prune_outgoing(&mut self) {
        let messages = &self.messages;
        self.outgoing.retain(|o| {
            let echoed = messages
                .iter()
                .any(|m| m.role == Role::User && m.content.trim() == o.content.trim() && !o.known.contains(&m.id));
            !echoed && o.at.elapsed() < Duration::from_secs(6)
        });
    }

    fn reload_runs(&mut self, cx: &mut Context<Self>) {
        let Some(tid) = self.selected else { return };
        let client = self.client(cx);
        swr(self, &client, format!("/api/threads/{tid}/runs?limit=5"), cx, move |this, runs: Vec<Run>, cx| {
            if this.selected != Some(tid) {
                return;
            }
            this.runs = runs;
            let live = this.runs.first().filter(|r| r.status.is_active()).map(|r| r.id);
            if live != this.events_run {
                this.events_run = live;
                this.events.clear();
                if live.is_some() {
                    this.reload_events(cx);
                }
            } else if live.is_some() {
                this.reload_events(cx);
            }
        });
    }

    /// Incremental: only events after the last seq we have.
    fn reload_events(&mut self, cx: &mut Context<Self>) {
        let Some(run) = self.events_run else { return };
        let after = self.events.last().map(|e| format!("?after_seq={}", e.seq)).unwrap_or_default();
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move { client.get::<Vec<Event>>(&format!("/api/runs/{run}/events{after}")).await });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(new)) = task.await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.events_run != Some(run) || new.is_empty() {
                    return;
                }
                let count = |evs: &[Event], kind| evs.iter().filter(|e| e.kind == kind).count();
                let (text0, think0) = (count(&this.events, familiar_client::EventKind::Text), count(&this.events, familiar_client::EventKind::Thinking));
                for e in new {
                    if !this.events.iter().any(|x| x.seq == e.seq) {
                        this.events.push(e);
                    }
                }
                this.events.sort_by_key(|e| e.seq);
                let (text1, think1) = (count(&this.events, familiar_client::EventKind::Text), count(&this.events, familiar_client::EventKind::Thinking));
                // A persisted text/thinking event replaces what the deltas streamed so far.
                if text1 != text0 || think1 != think0 {
                    this.data.update(cx, |d, _| {
                        if let Some(b) = d.live.get_mut(&run) {
                            if text1 != text0 {
                                b.text.clear();
                            }
                            if think1 != think0 {
                                b.thinking.clear();
                            }
                        }
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- actions ----------------------------------------------------------------------------------------------

    fn toast_error(&self, title: &'static str, e: String, cx: &mut Context<Self>) {
        self.toasts.update(cx, |t, cx| t.push(Tone::Bad, title, Some(e.into()), cx));
    }

    fn new_thread(&mut self, cx: &mut Context<Self>) {
        let client = self.client(cx);
        let bot = self.bot;
        let task = Tokio::spawn(cx, async move { client.create_thread(bot, Some("New chat")).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |this, cx| match r {
                Ok(t) => {
                    this.threads.insert(0, t.clone());
                    this.select(t.id, cx);
                    this.reload_threads(cx);
                }
                Err(e) => this.toast_error("Couldn't start a thread", e, cx),
            });
        })
        .detach();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.composer.read(cx).value().trim().to_string();
        if content.is_empty() || self.sending {
            return;
        }
        self.composer.update(cx, |s, cx| s.set_value("", window, cx));
        self.post(content, window, cx);
    }

    /// Post `content` into the selected thread (the composer, or Retry on a failed run).
    fn post(&mut self, content: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        self.sending = true;
        self.outgoing.push(Outgoing { content: content.clone(), known: self.messages.iter().map(|m| m.id).collect(), at: Instant::now() });
        self.force_bottom = true;
        cx.notify();
        let client = self.client(cx);
        let bot = self.bot;
        let thread = self.selected.and_then(|id| self.threads.iter().find(|t| t.id == id).cloned());
        let body = content.clone();
        // Post into the selected thread (creating one first if there is none); name an unnamed thread after it.
        let task = Tokio::spawn(cx, async move {
            let thread = match thread {
                Some(t) => t,
                None => client.create_thread(bot, Some("New chat")).await?,
            };
            client.post_message(thread.id, &body).await?;
            let unnamed = thread.title.as_deref().is_none_or(|t| t.trim().is_empty() || t == "New chat");
            if unnamed {
                let _ = client.rename_thread(thread.id, &excerpt(&body, 48)).await;
            }
            Ok::<_, familiar_client::ApiError>(thread.id)
        });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match r {
                    Ok(tid) => {
                        if this.selected != Some(tid) {
                            this.selected = Some(tid);
                        }
                        this.reload_messages(cx);
                        this.reload_runs(cx);
                        this.reload_threads(cx);
                        // Re-check the bubble once its 6 s grace is over.
                        cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(Duration::from_millis(6100)).await;
                            let _ = this.update(cx, |this, cx| {
                                this.prune_outgoing();
                                cx.notify();
                            });
                        })
                        .detach();
                    }
                    Err(e) => {
                        this.outgoing.retain(|o| o.content != content);
                        if this.composer.read(cx).value().trim().is_empty() {
                            this.composer.update(cx, |s, cx| s.set_value(content.clone(), window, cx));
                        }
                        this.toast_error("Couldn't send that", e, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cancel(&mut self, run: Uuid, cx: &mut Context<Self>) {
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move { client.cancel_run(run).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |this, cx| match r {
                Ok(_) => this.reload_runs(cx),
                Err(e) => this.toast_error("Couldn't stop it", e, cx),
            });
        })
        .detach();
    }

    // ---- rendering ------------------------------------------------------------------------------------------------

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let d = self.data.read(cx);
        let Some(bot) = d.bot(self.bot).cloned() else {
            return div().into_any_element();
        };
        let t = d.teammate(&bot);
        let this = cx.entity();
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
            .child(div().flex_1())
            .child(
                Segmented::new(
                    SharedString::from(format!("bot-tabs-{}", self.bot)),
                    vec![("Chat".into(), Some(icons::CHAT_ROUND_LINE)), ("Settings".into(), Some(icons::SETTINGS))],
                    self.tab,
                )
                .segment_width(104.0)
                .on_select(move |i, window, cx| this.update(cx, |p, cx| p.set_tab(i, window, cx))),
            )
            .into_any_element()
    }

    pub fn set_tab(&mut self, tab: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        if tab == 1 {
            // Fresh from the current bot every time the tab opens.
            let (data, toasts, bot) = (self.data.clone(), self.toasts.clone(), self.bot);
            self.settings = Some(cx.new(|cx| BotSettings::new(data, toasts, bot, window, cx)));
            self.settings_scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        } else {
            self.settings = None;
            self.force_bottom = true;
        }
        cx.notify();
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
        let this_new = cx.entity();
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
                    .child(
                        Button::icon_only("new-thread", icons::PLUS)
                            .size(ButtonSize::Small)
                            .tooltip("New thread")
                            .on_click(move |_, _, cx| this_new.update(cx, |p, cx| p.new_thread(cx))),
                    ),
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

    fn user_bubble(id: SharedString, content: String, pending: bool, theme: &Theme) -> AnyElement {
        anim::appear(
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
                    .when(pending, |el| el.opacity(0.6))
                    .child(content),
            ),
        )
        .into_any_element()
    }

    /// The assistant side: mascot, "name · when", and a bubble.
    fn assistant_row(
        id: SharedString,
        header: String,
        avatar: familiar_ui::mascot::Avatar,
        state: MascotState,
        theme: &Theme,
        bubble: impl IntoElement,
    ) -> AnyElement {
        anim::appear(
            id.clone(),
            div()
                .flex()
                .gap(px(12.0))
                .items_start()
                .child(Mascot::new(id, avatar, state, 30.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(px(4.0))
                        .flex_1()
                        .min_w_0()
                        .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(header))
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
                                .child(bubble),
                        ),
                ),
        )
        .into_any_element()
    }

    fn message(&self, m: &Message, name: &SharedString, avatar: familiar_ui::mascot::Avatar, theme: &Theme) -> AnyElement {
        let id = SharedString::from(format!("msg-{}", m.id));
        if m.role == Role::User {
            return Self::user_bubble(id, m.content.clone(), false, theme);
        }
        let who: SharedString = if m.role == Role::System { "System".into() } else { name.clone() };
        Self::assistant_row(
            id,
            format!("{who} · {}", ago(Some(m.created_at))),
            avatar,
            MascotState::Idle,
            theme,
            markdown::render(&m.content, theme),
        )
    }

    fn dot(color: Hsla) -> gpui::Div {
        div().flex_none().mt(px(7.0)).size(px(6.0)).rounded_full().bg(color)
    }

    /// One persisted event (the web's `EventList` row).
    fn event_row(&mut self, e: &Event, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = Theme::of(cx).clone();
        let mono = theme.font_mono.clone();
        let row = |dot: Hsla, body: AnyElement| div().flex().gap(px(10.0)).child(Self::dot(dot)).child(div().flex_1().min_w_0().child(body));
        let el = match e.typed() {
            TypedEvent::Text(t) if !t.trim().is_empty() => row(theme.ink, markdown::render(&t, &theme).into_any_element()),
            TypedEvent::Thinking(t) if !t.trim().is_empty() => row(
                theme.line,
                div()
                    .text_size(px(text::SMALL))
                    .text_color(theme.muted)
                    .italic()
                    .line_clamp(3)
                    .child(t.trim().to_owned())
                    .into_any_element(),
            ),
            TypedEvent::ToolCall(c) => {
                let preview = ["command", "file_path", "path", "url", "query", "pattern"]
                    .iter()
                    .find_map(|k| c.input.get(*k).and_then(Value::as_str))
                    .map(|s| excerpt(s, 90));
                let exp = self.tools.entry(e.id).or_insert_with(|| Expand::new(false));
                let openness = exp.openness();
                let eid = e.id;
                let this = cx.entity();
                let json = serde_json::to_string_pretty(&c.input).unwrap_or_default();
                let detail = exp.render(
                    SharedString::from(format!("tool-detail-{}", e.id)),
                    window,
                    cx,
                    div()
                        .mt(px(6.0))
                        .px(px(10.0))
                        .py(px(8.0))
                        .rounded(px(RADIUS_CHIP))
                        .bg(theme.sunken)
                        .font_family(mono.clone())
                        .text_size(px(text::CAPTION))
                        .text_color(theme.ink)
                        .child(excerpt_lines(&json, 40)),
                );
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
                                .on_click(move |_, _, cx| {
                                    this.update(cx, |p, cx| {
                                        if let Some(x) = p.tools.get_mut(&eid) {
                                            x.toggle();
                                        }
                                        cx.notify();
                                    })
                                })
                                .child(
                                    icon(icons::ALT_ARROW_RIGHT)
                                        .size(px(12.0))
                                        .text_color(theme.muted)
                                        .with_transformation(gpui::Transformation::rotate(gpui::radians(openness * std::f32::consts::FRAC_PI_2))),
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
                                            .min_w_0()
                                            .truncate()
                                            .font_family(mono.clone())
                                            .text_size(px(text::CAPTION))
                                            .text_color(theme.muted)
                                            .child(p),
                                    )
                                }),
                        )
                        .child(detail)
                        .into_any_element(),
                )
            }
            TypedEvent::ToolResult(r) => {
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
                                div()
                                    .text_size(px(text::CAPTION))
                                    .text_color(theme.muted)
                                    .child(SharedString::from(format!("by {by}"))),
                            )
                        })
                        .into_any_element(),
                )
            }
            TypedEvent::Error(m) => row(
                theme.bad,
                div().text_size(px(text::SMALL)).text_color(theme.bad).child(m).into_any_element(),
            ),
            _ => return None,
        };
        Some(el.into_any_element())
    }

    /// The live bubble: streamed thinking (italic) and text with a blinking caret, or "Working…".
    fn live_bubble(&self, run: Uuid, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let buf = self.data.read(cx).live.get(&run).cloned().unwrap_or_default();
        let mut col = div().flex().flex_col().gap(px(6.0));
        if buf.text.is_empty() && buf.thinking.is_empty() {
            col = col.child(div().text_size(px(text::SMALL)).text_color(theme.muted).child("Working…"));
        }
        if !buf.thinking.trim().is_empty() {
            col = col.child(
                div()
                    .text_size(px(text::SMALL))
                    .text_color(theme.muted)
                    .italic()
                    .line_clamp(3)
                    .child(data::tail(buf.thinking.trim(), 600)),
            );
        }
        if !buf.text.is_empty() {
            col = col.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_end()
                    .child(markdown::render(&buf.text, theme))
                    .child(
                        div()
                            .w(px(7.0))
                            .h(px(15.0))
                            .ml(px(2.0))
                            .mb(px(2.0))
                            .bg(theme.accent)
                            .with_animation(SharedString::from(format!("caret-{run}")), CARET_BLINK.repeating(), |el, t| {
                                el.opacity(if t < 0.5 { 1.0 } else { 0.15 })
                            }),
                    ),
            );
        }
        col.into_any_element()
    }

    fn run_card(&mut self, run: &Run, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let kind = match run.kind {
            RunKind::Handoff => "handoff".to_owned(),
            RunKind::Unknown => "run".to_owned(),
            k => format!("{} run", k.as_str()),
        };
        let active = run.status.is_active();
        let rid = run.id;
        let this = cx.entity();
        let mut body = div().flex().flex_col().gap(px(10.0)).child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(StatusChip::new(run_status(run.status)))
                .child(chip(Tone::Muted, kind, cx))
                .child(div().flex_1())
                .when(active, |el| {
                    el.child(
                        Button::new(SharedString::from(format!("cancel-{rid}")), "Stop")
                            .size(ButtonSize::Small)
                            .ghost()
                            .on_click(move |_, _, cx| this.update(cx, |p, cx| p.cancel(rid, cx))),
                    )
                }),
        );
        if let Some(e) = run.error.clone().filter(|e| !e.trim().is_empty()) {
            body = body.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e));
        }
        if active {
            let mut list = div().flex().flex_col().gap(px(8.0));
            for e in self.events.clone() {
                if let Some(row) = self.event_row(&e, window, cx) {
                    list = list.child(anim::appear(SharedString::from(format!("ev-{}", e.id)), div().child(row)));
                }
            }
            list = list.child(
                div().flex().gap(px(10.0)).child(Self::dot(theme.accent)).child(div().flex_1().min_w_0().child(self.live_bubble(rid, &theme, cx))),
            );
            body = body.child(list);
        }
        let pending: Vec<_> = self.data.read(cx).pending.iter().filter(|a| a.run_id == rid).cloned().collect();
        if !pending.is_empty() {
            let (data, toasts) = (self.data.clone(), self.toasts.clone());
            body = body.children(self.approvals.render(&pending, &data, &toasts, window, cx));
        }
        anim::appear(SharedString::from(format!("run-{}", run.id)), card(cx).px(px(14.0)).py(px(12.0)).child(body))
            .into_any_element()
    }

    /// A failed or stopped run: one compact line under the message, with Retry (posts the same message again).
    fn failed_notice(&self, run: &Run, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let stopped = run.status == RunStatus::Cancelled;
        let summary = match run.error.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
            Some(e) if !stopped => format!("Didn't finish: {}", excerpt(e, 120)),
            _ if stopped => "Stopped before it finished.".to_owned(),
            _ => "Didn't finish.".to_owned(),
        };
        let content = run
            .prompt
            .clone()
            .filter(|p| !p.trim().is_empty())
            .or_else(|| self.messages.iter().rev().find(|m| m.role == Role::User).map(|m| m.content.clone()));
        let this = cx.entity();
        anim::appear(
            SharedString::from(format!("failed-{}", run.id)),
            div().flex().justify_end().child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .max_w(px(560.0))
                    .pl(px(10.0))
                    .pr(px(4.0))
                    .py(px(4.0))
                    .rounded(px(RADIUS_CHIP))
                    .bg(if stopped { theme.sunken } else { theme.bad_soft })
                    .child(
                        icon(if stopped { icons::INFO_CIRCLE } else { icons::DANGER_TRIANGLE })
                            .size(px(14.0))
                            .text_color(if stopped { theme.muted } else { theme.bad }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(text::SMALL))
                            .text_color(if stopped { theme.muted } else { theme.ink })
                            .child(summary),
                    )
                    .when_some(content, |el, content| {
                        el.child(
                            Button::new(SharedString::from(format!("retry-{}", run.id)), "Retry")
                                .size(ButtonSize::Small)
                                .ghost()
                                .icon(icons::REFRESH)
                                .disabled(self.sending)
                                .on_click(move |_, window, cx| this.update(cx, |p, cx| p.post(content.clone(), window, cx))),
                        )
                    }),
            ),
        )
        .into_any_element()
    }

    fn transcript(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let (name, avatar) = {
            let d = self.data.read(cx);
            match d.bot(self.bot) {
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
                    .child(div().text_color(theme.muted).child("Say hello below to start a thread.")),
            ));
        } else if !self.messages_loaded && self.selected.is_some() {
            for (i, (user, width)) in [(true, 220.0), (false, 420.0), (true, 160.0), (false, 360.0)].into_iter().enumerate() {
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
            if self.messages.is_empty() && self.runs.is_empty() && self.outgoing.is_empty() {
                col = col.child(anim::appear(
                    "chat-empty",
                    empty(
                        SharedString::from(format!("Say hello to {name}")),
                        Some("Ask for something, or tell it what to keep an eye on.".into()),
                        cx,
                    ),
                ));
            }
            for m in &self.messages {
                col = col.child(self.message(m, &name, avatar, &theme));
            }
            for (i, o) in self.outgoing.iter().enumerate() {
                col = col.child(Self::user_bubble(
                    SharedString::from(format!("out-{i}-{}", o.content.len())),
                    o.content.clone(),
                    true,
                    &theme,
                ));
            }
            // Like a normal chat: the live card only while the run works; a finished run leaves just its reply, a
            // failed or stopped one a one-line notice with Retry. Run details live in the teammate's activity.
            if let Some(run) = self.runs.first().cloned() {
                if run.status.is_active() {
                    col = col.child(self.run_card(&run, window, cx));
                } else if matches!(run.status, RunStatus::Failed | RunStatus::Cancelled) {
                    col = col.child(self.failed_notice(&run, cx));
                }
            }
        }

        // Stick to the bottom: when what's shown changed and the reader was at (or within 80 px of) the bottom.
        let live_len = self.runs.first().and_then(|r| self.data.read(cx).live.get(&r.id)).map(|b| b.text.len() + b.thinking.len()).unwrap_or(0);
        let rev = (
            self.messages.len(),
            self.outgoing.len(),
            self.events.len(),
            live_len,
            self.runs.first().is_some_and(|r| r.status.is_active()),
        );
        let max = f32::from(self.scroll.max_offset().y);
        let at = -f32::from(self.scroll.offset().y);
        if self.force_bottom || (rev != self.content_rev && max - at <= 80.0) {
            self.scroll.scroll_to_bottom();
            self.force_bottom = false;
        }
        self.content_rev = rev;

        edge_faded(
            20.0,
            true,
            true,
            div()
                .id("transcript")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
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

    fn composer(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let blank = self.composer.read(cx).value().trim().is_empty();
        let this = cx.entity();
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(px(28.0))
            .pt(px(8.0))
            .pb(px(20.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(720.0))
                    .flex()
                    .items_end()
                    .gap(px(10.0))
                    .child(div().flex_1().min_w_0().child(text_input::field("composer", &self.composer, 44.0, window, cx)))
                    .child(
                        Button::new("send", "Send")
                            .primary()
                            .icon(icons::ARROW_RIGHT)
                            .disabled(blank || self.sending)
                            .on_click(move |_, window, cx| this.update(cx, |p, cx| p.send(window, cx))),
                    ),
            )
            .into_any_element()
    }
}

/// The first `n` lines of `s` (an ellipsis line when cut).
fn excerpt_lines(s: &str, n: usize) -> String {
    let mut lines: Vec<&str> = s.lines().take(n + 1).collect();
    if lines.len() > n {
        lines.truncate(n);
        lines.push("…");
    }
    lines.join("\n")
}

impl Render for BotPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.header(cx);
        if let Some(settings) = self.settings.clone().filter(|_| self.tab == 1) {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .child(header)
                .child(
                    div().flex_1().min_h_0().child(
                        edge_faded(
                            24.0,
                            true,
                            true,
                            div()
                                .id("bot-settings-scroll")
                                .size_full()
                                .overflow_y_scroll()
                                .track_scroll(&self.settings_scroll)
                                .child(
                                    div()
                                        .w_full()
                                        .flex()
                                        .justify_center()
                                        .px(px(40.0))
                                        .py(px(28.0))
                                        .child(div().w_full().max_w(px(720.0)).child(settings)),
                                ),
                        )
                        .fade_overflow_y(&self.settings_scroll),
                    ),
                );
        }
        let threads = self.thread_list(cx);
        let transcript = self.transcript(window, cx);
        let jump = self.jump_button(cx);
        let composer = self.composer(window, cx);
        div().size_full().flex().flex_col().child(header).child(
            div().flex().flex_1().min_h_0().child(threads).child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(div().relative().flex_1().min_h_0().child(transcript).when_some(jump, |el, j| el.child(j)))
                    .child(composer),
            ),
        )
    }
}

impl BotPage {
    /// "Latest" pill, shown while the reader is scrolled well above the bottom.
    fn jump_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let max = f32::from(self.scroll.max_offset().y);
        let at = -f32::from(self.scroll.offset().y);
        if max - at <= 240.0 {
            return None;
        }
        let this = cx.entity();
        Some(
            div()
                .absolute()
                .bottom(px(14.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(anim::appear(
                    "jump-latest",
                    div().rounded_full().shadow(Theme::of(cx).float_shadow()).child(
                        Button::new("jump", "Latest")
                            .size(ButtonSize::Small)
                            .icon(icons::ALT_ARROW_DOWN)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |p, cx| {
                                    p.force_bottom = true;
                                    cx.notify();
                                })
                            }),
                    ),
                ))
                .into_any_element(),
        )
    }
}
