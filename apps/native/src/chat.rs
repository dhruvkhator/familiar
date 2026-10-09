//! A teammate's page (the web's `pages/Chat.tsx`): a hero header with the mascot's live state, the thread list, the
//! selected thread's transcript (messages oldest → newest, the pending optimistic bubble, then the latest run's card
//! with its live events, streamed text and approvals) and the composer. A teammate hired from a template shows its
//! Set up checklist above the chat until that is done.
//!
//! The transcript is a virtual list (only the rows in view are built and laid out), messages are parsed once (a
//! [`markdown::DocCache`] in step with the loaded list) and the streaming reply re-parses only its last block.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use familiar_client::{Event, Message, Role, Run, RunKind, RunStatus, Thread};
use familiar_ui::anim::{self, SPRING_SELECT};
use familiar_ui::components::{Button, ButtonSize, Segmented, SidebarItem, Skeleton, StatusChip, card, chip, empty, group_label};
use familiar_ui::edge_fade::edge_faded;
use familiar_ui::icons::{self, icon};
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::motion::{EASE, MotionSpec};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CHIP, SIDEBAR_WIDTH, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ListAlignment,
    ListState, ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, list, prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, TextareaState};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::activity::{ActivityTab, OpenThread};
use crate::approval::ApprovalCards;
use crate::bot_settings::BotSettings;
use crate::computer::{Browsing, ComputerPanel};
use crate::events::EventRows;
use crate::memory::MemoryTab;
use crate::setup::{OpenLogin, SetupCard};
use crate::data::{self, AppData, DataEvent, ago, excerpt, run_status, swr};
use crate::markdown;
use crate::shell::state_tone;
use crate::text_input;

/// The caret of the streaming text.
const CARET_BLINK: MotionSpec = MotionSpec::new(1000, EASE);

/// Entrances play for rows this new; a row scrolled back into view (built again) doesn't replay its entrance.
const FRESH: Duration = Duration::from_millis(480);

/// One row of the transcript list.
#[derive(Debug, Clone, PartialEq)]
enum Row {
    /// No thread yet: "Chat with …".
    NoThread,
    Loading,
    /// A thread with nothing in it: "Say hello".
    Empty,
    /// `messages[i]`, with its id.
    Message(usize, Uuid),
    /// `outgoing[i]`, with its length (its key).
    Outgoing(usize, usize),
    /// The latest run, working.
    Run(Uuid),
    /// The latest run failed or was stopped.
    Failed(Uuid),
    /// The space under the last row.
    End,
}

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
    rows: EventRows,
    approvals: ApprovalCards,
    composer: Entity<TextareaState>,
    outgoing: Vec<Outgoing>,
    sending: bool,
    /// The transcript: a virtual list of [`Row`]s.
    list: ListState,
    shown: Vec<Row>,
    /// When each row was first built (entrances play once).
    appeared: HashMap<SharedString, Instant>,
    /// The loaded messages, parsed.
    docs: markdown::DocCache,
    /// The streaming reply, parsed as it grows.
    live_doc: markdown::Streamed,
    thread_scroll: ScrollHandle,
    /// What the transcript showed last frame; a change while at the bottom scrolls to the new bottom.
    content_rev: (usize, usize, usize, usize, bool),
    force_bottom: bool,
    /// 0: chat, 1: what it learned, 2: activity, 3: settings.
    tab: usize,
    settings: Option<Entity<BotSettings>>,
    memory: Option<Entity<MemoryTab>>,
    activity: Option<Entity<ActivityTab>>,
    tab_scroll: ScrollHandle,
    computer: Entity<ComputerPanel>,
    computer_open: bool,
    /// You closed the computer: it doesn't open by itself again on this page.
    computer_dismissed: bool,
    /// The live reply's caret: its run and when it first showed (the blink's phase).
    caret: Option<(Uuid, Instant)>,
    setup: Entity<SetupCard>,
}

pub const TAB_SETTINGS: usize = 3;

impl BotPage {
    pub fn new(
        data: Entity<AppData>,
        toasts: Entity<ToastStack>,
        bot: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.subscribe(&data, Self::on_data).detach();
        let name = data.read(cx).bot(bot).map(|b| b.name.clone()).unwrap_or_else(|| "your teammate".into());
        let composer = text_input::new_field(format!("Message {name}"), true, 6, window, cx);
        cx.subscribe_in(&composer, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { shift: false, .. } => this.send(window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        })
        .detach();
        let computer = cx.new(|cx| ComputerPanel::new(data.clone(), toasts.clone(), bot, window, cx));
        cx.subscribe(&computer, |this: &mut Self, _, _: &Browsing, cx| {
            if !this.computer_dismissed {
                this.set_computer(true, cx);
            }
        })
        .detach();
        let setup = cx.new(|cx| SetupCard::new(data.clone(), toasts.clone(), bot, cx));
        cx.subscribe_in(&setup, window, |this: &mut Self, _, ev: &OpenLogin, window, cx| this.open_login(ev.0.clone(), window, cx))
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
            rows: EventRows::default(),
            approvals: ApprovalCards::default(),
            composer,
            outgoing: Vec::new(),
            sending: false,
            list: ListState::new(0, ListAlignment::Top, px(600.0)),
            shown: Vec::new(),
            appeared: HashMap::new(),
            docs: markdown::DocCache::default(),
            live_doc: markdown::Streamed::default(),
            thread_scroll: ScrollHandle::new(),
            content_rev: Default::default(),
            force_bottom: true,
            tab: 0,
            settings: None,
            memory: None,
            activity: None,
            tab_scroll: ScrollHandle::new(),
            computer,
            computer_open: false,
            computer_dismissed: false,
            caret: None,
            setup,
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
            // The header (status, model), the run card's approvals and their pictures.
            DataEvent::Updated(_) => cx.notify(),
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
            crate::perf::begin("thread_open");
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
        self.appeared.clear();
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
            this.docs.sync(list.iter().filter(|m| m.role != Role::User).map(|m| (m.id, m.content.as_str())));
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
    pub fn post(&mut self, content: String, window: &mut Window, cx: &mut Context<Self>) {
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
                    // The persona's prose, without markdown headings (a template's instructions start with one).
                    .when_some(bot.persona.as_deref().map(prose).filter(|p| !p.is_empty()), |el, p| {
                        el.child(
                            div().text_size(px(text::SMALL)).text_color(theme.muted).line_clamp(1).child(excerpt(&p, 160)),
                        )
                    }),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        Segmented::new(
                            SharedString::from(format!("bot-tabs-{}", self.bot)),
                            vec![
                                ("Chat".into(), Some(icons::CHAT_ROUND_LINE)),
                                ("Learned".into(), Some(icons::STAR)),
                                ("Activity".into(), Some(icons::LIST)),
                                ("Settings".into(), Some(icons::SETTINGS)),
                            ],
                            self.tab,
                        )
                        .segment_width(96.0)
                        .on_select(move |i, window, cx| this.update(cx, |p, cx| p.set_tab(i, window, cx))),
                    )
                    .child({
                        let this = cx.entity();
                        let open = self.computer_open;
                        let live = self.computer.read(cx).browsing();
                        div()
                            .relative()
                            .child(
                                Button::icon_only("computer-toggle", icons::MONITOR)
                                    .when(open, |b| b.secondary())
                                    .tooltip(if open { "Hide the computer" } else { "Show the computer" })
                                    .on_click(move |_, _, cx| {
                                        this.update(cx, |p, cx| {
                                            let open = !p.computer_open;
                                            p.computer_dismissed = !open;
                                            p.set_computer(open, cx);
                                        })
                                    }),
                            )
                            .when(live, |el| {
                                el.child(div().absolute().top(px(4.0)).right(px(4.0)).size(px(7.0)).rounded_full().bg(theme.ok))
                            })
                    }),
            )
            .into_any_element()
    }

    pub fn set_computer(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.computer_open != open {
            self.computer_open = open;
            self.computer.update(cx, |c, cx| c.set_shown(open, cx));
            cx.notify();
        }
    }

    /// The Set up checklist's "Log in to …": show the computer on that page, with you in control.
    fn open_login(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        self.computer_dismissed = false;
        self.set_computer(true, cx);
        self.computer.update(cx, |c, cx| c.take_over_at(url, window, cx));
    }

    pub fn set_tab(&mut self, tab: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.tab_scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        let (data, toasts, bot) = (self.data.clone(), self.toasts.clone(), self.bot);
        // Settings is fresh from the current bot every time it opens; the others keep their state.
        self.settings = (tab == TAB_SETTINGS).then(|| cx.new(|cx| BotSettings::new(data.clone(), toasts.clone(), bot, window, cx)));
        match tab {
            0 => self.force_bottom = true,
            1 if self.memory.is_none() => self.memory = Some(cx.new(|cx| MemoryTab::new(data, toasts, bot, window, cx))),
            2 if self.activity.is_none() => {
                let activity = cx.new(|cx| ActivityTab::new(data, bot, cx));
                cx.subscribe_in(&activity, window, |this: &mut Self, _, ev: &OpenThread, window, cx| {
                    this.select(ev.0, cx);
                    this.set_tab(0, window, cx);
                })
                .detach();
                self.activity = Some(activity);
            }
            _ => {}
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

    /// `el` with its entrance while the row is fresh (see [`FRESH`]); after that, as the entrance leaves it.
    fn entrance<E: gpui::Styled + IntoElement + 'static>(&mut self, id: SharedString, el: E) -> AnyElement {
        let first = *self.appeared.entry(id.clone()).or_insert_with(Instant::now);
        if first.elapsed() < FRESH { anim::appear(id, el).into_any_element() } else { el.into_any_element() }
    }

    fn user_bubble(&mut self, id: SharedString, content: String, pending: bool, theme: &Theme) -> AnyElement {
        self.entrance(
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
    }

    /// The assistant side: mascot, "name · when", and a bubble.
    fn assistant_row(
        &mut self,
        id: SharedString,
        header: String,
        avatar: familiar_ui::mascot::Avatar,
        state: MascotState,
        theme: &Theme,
        bubble: impl IntoElement,
    ) -> AnyElement {
        self.entrance(
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
    }

    fn message(&mut self, ix: usize, name: &SharedString, avatar: familiar_ui::mascot::Avatar, theme: &Theme) -> AnyElement {
        let Some(m) = self.messages.get(ix) else { return div().into_any_element() };
        let id = SharedString::from(format!("msg-{}", m.id));
        if m.role == Role::User {
            let content = m.content.clone();
            return self.user_bubble(id, content, false, theme);
        }
        let who: SharedString = if m.role == Role::System { "System".into() } else { name.clone() };
        let header = format!("{who} · {}", ago(Some(m.created_at)));
        let body = match self.docs.get(m.id) {
            Some(doc) => doc.render(theme),
            None => markdown::render(&m.content, theme),
        };
        self.assistant_row(id, header, avatar, MascotState::Idle, theme, body)
    }

    /// The live bubble: streamed thinking (italic) and text with a blinking caret, or "Working…".
    fn live_bubble(&mut self, run: Uuid, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (working, thinking, text) = {
            let buf = self.data.read(cx).live.get(&run);
            let text = buf.map(|b| b.text.as_str()).unwrap_or_default();
            let thinking = buf.map(|b| b.thinking.trim()).unwrap_or_default();
            if !text.is_empty() {
                self.live_doc.update(text);
            }
            (text.is_empty() && buf.is_none_or(|b| b.thinking.is_empty()), data::tail(thinking, 600), !text.is_empty())
        };
        let mut col = div().flex().flex_col().gap(px(6.0));
        if working {
            col = col.child(div().text_size(px(text::SMALL)).text_color(theme.muted).child("Working…"));
        }
        if !thinking.is_empty() {
            col = col.child(
                div()
                    .text_size(px(text::SMALL))
                    .text_color(theme.muted)
                    .italic()
                    .line_clamp(3)
                    .child(thinking),
            );
        }
        if text {
            crate::perf::painted("delta");
            col = col.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_end()
                    .child(self.live_doc.render(theme))
                    .child(
                        div()
                            .w(px(7.0))
                            .h(px(15.0))
                            .ml(px(2.0))
                            .mb(px(2.0))
                            .bg(theme.accent)
                            .opacity(self.caret_opacity(run, cx)),
                    ),
            );
        }
        col.into_any_element()
    }

    /// The caret blinks on for the first half of each [`CARET_BLINK`] and dim for the second. It only changes at
    /// those edges, so the page wakes for them instead of drawing every frame (it rests on under reduced motion).
    fn caret_opacity(&mut self, run: Uuid, cx: &mut Context<Self>) -> f32 {
        if familiar_ui::motion::reduced_motion(cx) {
            return 1.0;
        }
        let since = match self.caret {
            Some((r, at)) if r == run => at,
            _ => self.caret.insert((run, Instant::now())).1,
        };
        let period = CARET_BLINK.total().as_millis().max(2);
        let t = since.elapsed().as_millis() % period;
        let (on, edge) = if t < period / 2 { (true, period / 2 - t) } else { (false, period - t) };
        anim::wake_at(cx.entity_id(), Instant::now() + Duration::from_millis(edge as u64), cx);
        if on { 1.0 } else { 0.15 }
    }

    fn run_card(&mut self, run: &Run, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let kind = match run.kind {
            RunKind::Handoff => "handoff".to_owned(),
            RunKind::Followup => "your decision on a draft".to_owned(),
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
            let mut list = div().flex().flex_col().gap(px(8.0)).children(self.rows.render(&self.events, false, true, window, cx));
            list = list.child(
                div()
                    .flex()
                    .gap(px(10.0))
                    .child(div().flex_none().mt(px(7.0)).size(px(6.0)).rounded_full().bg(theme.accent))
                    .child(div().flex_1().min_w_0().child(self.live_bubble(rid, &theme, cx))),
            );
            body = body.child(list);
        }
        let pending: Vec<_> = self.data.read(cx).pending.iter().filter(|a| a.run_id == rid).cloned().collect();
        if !pending.is_empty() {
            let (data, toasts) = (self.data.clone(), self.toasts.clone());
            body = body.children(self.approvals.render(&pending, &data, &toasts, window, cx));
        }
        let card = card(cx).px(px(14.0)).py(px(12.0)).child(body);
        self.entrance(SharedString::from(format!("run-{}", run.id)), card)
    }

    /// A failed or stopped run: one compact line under the message, with Retry (posts the same message again).
    fn failed_notice(&mut self, run: &Run, cx: &mut Context<Self>) -> AnyElement {
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
        let sending = self.sending;
        self.entrance(
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
                                .disabled(sending)
                                .on_click(move |_, window, cx| this.update(cx, |p, cx| p.post(content.clone(), window, cx))),
                        )
                    }),
            ),
        )
    }

    /// The transcript's rows, as of now.
    fn rows_now(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        if self.selected.is_none() && self.threads_loaded {
            rows.push(Row::NoThread);
        } else if !self.messages_loaded && self.selected.is_some() {
            rows.push(Row::Loading);
        } else {
            if self.messages.is_empty() && self.runs.is_empty() && self.outgoing.is_empty() {
                rows.push(Row::Empty);
            }
            rows.extend(self.messages.iter().enumerate().map(|(i, m)| Row::Message(i, m.id)));
            rows.extend(self.outgoing.iter().enumerate().map(|(i, o)| Row::Outgoing(i, o.content.len())));
            // Like a normal chat: the live card only while the run works; a finished run leaves just its reply, a
            // failed or stopped one a one-line notice with Retry. Run details live in the teammate's activity.
            if let Some(run) = self.runs.first() {
                if run.status.is_active() {
                    rows.push(Row::Run(run.id));
                } else if matches!(run.status, RunStatus::Failed | RunStatus::Cancelled) {
                    rows.push(Row::Failed(run.id));
                }
            }
        }
        rows.push(Row::End);
        rows
    }

    /// The transcript's furthest scroll offset, and how far down it is.
    fn list_offsets(list: &ListState) -> (f32, f32) {
        (f32::from(list.max_offset_for_scrollbar().y), -f32::from(list.scroll_px_offset_for_scrollbar().y))
    }

    /// Row `ix` of the transcript (the list asks only for the rows in view).
    fn row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.shown.get(ix).cloned() else { return div().into_any_element() };
        if row == Row::End {
            return div().h(px(24.0)).into_any_element();
        }
        let theme = Theme::of(cx).clone();
        let (name, avatar) = {
            let d = self.data.read(cx);
            match d.bot(self.bot) {
                Some(b) => (SharedString::from(b.name.clone()), data::avatar_of(b)),
                None => ("".into(), familiar_ui::mascot::default_avatar(&self.bot.to_string())),
            }
        };
        let body = match row {
            Row::NoThread => anim::appear(
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
            )
            .into_any_element(),
            Row::Loading => {
                let mut col = div().flex().flex_col().gap(px(18.0));
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
                col.into_any_element()
            }
            Row::Empty => anim::appear(
                "chat-empty",
                empty(
                    SharedString::from(format!("Say hello to {name}")),
                    Some("Ask for something, or tell it what to keep an eye on.".into()),
                    cx,
                ),
            )
            .into_any_element(),
            Row::Message(i, _) => self.message(i, &name, avatar, &theme),
            Row::Outgoing(i, _) => match self.outgoing.get(i).map(|o| o.content.clone()) {
                Some(content) => {
                    let id = SharedString::from(format!("out-{i}-{}", content.len()));
                    self.user_bubble(id, content, true, &theme)
                }
                None => div().into_any_element(),
            },
            Row::Run(id) | Row::Failed(id) => match self.runs.first().cloned().filter(|r| r.id == id) {
                Some(run) if run.status.is_active() => self.run_card(&run, window, cx),
                Some(run) => self.failed_notice(&run, cx),
                None => div().into_any_element(),
            },
            Row::End => div().into_any_element(),
        };
        // The column the rows sit in: centred, at most 720 wide, 18 apart, 24 from the top.
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(px(28.0))
            .pt(px(if ix == 0 { 24.0 } else { 18.0 }))
            .child(div().w_full().max_w(px(720.0)).child(body))
            .into_any_element()
    }

    fn transcript(&mut self, cx: &mut Context<Self>) -> AnyElement {
        // Rows that changed identity are spliced into the list; the rows in view are re-measured every frame.
        let rows = self.rows_now();
        if rows != self.shown {
            let same = self.shown.iter().zip(&rows).take_while(|(a, b)| a == b).count();
            self.list.splice(same..self.shown.len(), rows.len() - same);
            self.shown = rows;
        }

        // What a click on this teammate or thread waited for is on screen.
        if self.messages_loaded || (self.threads_loaded && self.selected.is_none()) {
            crate::perf::painted("switch");
            crate::perf::painted("thread_open");
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
        let (max, at) = Self::list_offsets(&self.list);
        if self.force_bottom || (rev != self.content_rev && max - at <= 80.0) {
            self.list.scroll_to_end();
            self.force_bottom = false;
        }
        self.content_rev = rev;

        let page = cx.entity();
        let state = self.list.clone();
        edge_faded(
            20.0,
            true,
            true,
            list(self.list.clone(), move |ix, window, cx| page.update(cx, |p, cx| p.row(ix, window, cx))).size_full(),
        )
        // Read at paint, once the list has clamped its offset for this frame.
        .fade_overflow_y_with(move |_| {
            let (max, at) = Self::list_offsets(&state);
            (at > 0.5, max - at > 0.5)
        })
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

/// Text lines without markdown headings, joined.
fn prose(s: &str) -> String {
    s.lines().filter(|l| !l.trim_start().starts_with('#')).collect::<Vec<_>>().join(" ").trim().to_owned()
}

impl Render for BotPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::count("BotPage");
        let header = self.header(cx);
        // The computer slides in from the right (its width springs); its content keeps a fixed width meanwhile.
        let main_w = f32::from(window.viewport_size().width) - SIDEBAR_WIDTH;
        let panel_w = (main_w * 0.42).clamp(340.0, 600.0);
        let shown_w = anim::spring(
            SharedString::from(format!("computer-w-{}", self.bot)),
            if self.computer_open { panel_w } else { 0.0 },
            SPRING_SELECT,
            window,
            cx,
        );
        let computer = (shown_w > 0.5).then(|| {
            let theme = Theme::of(cx);
            div()
                .flex_none()
                .h_full()
                .w(px(shown_w))
                .overflow_hidden()
                .border_l_1()
                .border_color(theme.line)
                .bg(theme.surface)
                .child(div().w(px(panel_w)).h_full().child(self.computer.clone()))
        });
        let tab: Option<AnyElement> = match self.tab {
            1 => self.memory.clone().map(|e| e.into_any_element()),
            2 => self.activity.clone().map(|e| e.into_any_element()),
            TAB_SETTINGS => self.settings.clone().map(|e| e.into_any_element()),
            _ => None,
        };
        let body = match tab {
            Some(content) => div().flex_1().min_w_0().h_full().child(
                edge_faded(
                    24.0,
                    true,
                    true,
                    div()
                        .id(SharedString::from(format!("bot-tab-scroll-{}", self.tab)))
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.tab_scroll)
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .justify_center()
                                .px(px(40.0))
                                .py(px(28.0))
                                .child(div().w_full().max_w(px(720.0)).child(content)),
                        ),
                )
                .fade_overflow_y(&self.tab_scroll),
            ),
            None => {
                // With the computer open on a narrow window, the thread list makes room.
                let threads = (main_w - shown_w - 236.0 >= 460.0).then(|| self.thread_list(cx));
                let transcript = self.transcript(cx);
                let jump = self.jump_button(cx);
                let composer = self.composer(window, cx);
                div().flex().flex_1().min_w_0().h_full().children(threads).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .flex_col()
                        .child(self.setup.clone())
                        .child(div().relative().flex_1().min_h_0().child(transcript).when_some(jump, |el, j| el.child(j)))
                        .child(composer),
                )
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(div().flex().flex_1().min_h_0().child(body).children(computer))
    }
}

impl BotPage {
    /// "Latest" pill, shown while the reader is scrolled well above the bottom.
    fn jump_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (max, at) = Self::list_offsets(&self.list);
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
