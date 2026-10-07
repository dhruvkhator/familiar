//! A teammate's Activity tab (the web's `pages/Activity.tsx`): its runs (status, kind, prompt, how long, what it would
//! have cost at API prices) and, opened, a run's whole event timeline. Live: the list follows `runs` notices, an open
//! active run follows its `events`.

use familiar_client::{Event, Run, RunKind};
use familiar_ui::anim;
use familiar_ui::components::{Button, ButtonSize, HoverCard, Skeleton, StatusChip, card, chip, empty};
use familiar_ui::icons::{self, icon};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CHIP, Theme, Tone, text};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::data::{AppData, DataEvent, excerpt, run_status, swr};
use crate::events::{EventRows, money, secs_label};

/// Asks the teammate page to open a thread in its chat.
pub struct OpenThread(pub Uuid);

pub struct ActivityTab {
    data: Entity<AppData>,
    bot: Uuid,
    runs: Vec<Run>,
    loaded: bool,
    /// `None`: the list; `Some(run)`: that run's timeline.
    view: Option<Uuid>,
    run: Option<Run>,
    events: Vec<Event>,
    events_loaded: bool,
    rows: EventRows,
}

impl EventEmitter<OpenThread> for ActivityTab {}

impl ActivityTab {
    pub fn new(data: Entity<AppData>, bot: Uuid, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&data, |this: &mut Self, _, ev: &DataEvent, cx| {
            let bot = this.bot.to_string();
            match ev {
                DataEvent::Changed(None) => {
                    this.reload(cx);
                    this.reload_run(cx);
                }
                DataEvent::Changed(Some(n)) if n.t == "runs" && n.bot.as_deref().is_none_or(|b| b == bot) => {
                    this.reload(cx);
                    this.reload_run(cx);
                }
                DataEvent::Changed(Some(n))
                    if n.t == "events" && this.view.is_some_and(|r| n.run.as_deref() == Some(&r.to_string())) =>
                {
                    this.reload_events(cx)
                }
                _ => {}
            }
        })
        .detach();
        let mut this = Self {
            data,
            bot,
            runs: Vec::new(),
            loaded: false,
            view: None,
            run: None,
            events: Vec::new(),
            events_loaded: false,
            rows: EventRows::default(),
        };
        this.reload(cx);
        this
    }

    fn client(&self, cx: &Context<Self>) -> familiar_client::Client {
        self.data.read(cx).client.clone()
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let client = self.client(cx);
        swr(self, &client, format!("/api/bots/{}/runs?limit=100", self.bot), cx, |this, runs: Vec<Run>, _| {
            this.runs = runs;
            this.loaded = true;
        });
    }

    pub fn open(&mut self, run: Option<Uuid>, cx: &mut Context<Self>) {
        if self.view == run {
            return;
        }
        self.view = run;
        self.run = run.and_then(|id| self.runs.iter().find(|r| r.id == id).cloned());
        self.events.clear();
        self.events_loaded = false;
        self.rows = EventRows::default();
        if run.is_some() {
            self.reload_run(cx);
            self.reload_events(cx);
        }
        cx.notify();
    }

    fn reload_run(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.view else { return };
        let client = self.client(cx);
        swr(self, &client, format!("/api/runs/{id}"), cx, move |this, run: Run, cx| {
            if this.view == Some(id) {
                let was_active = this.run.as_ref().is_some_and(|r| r.status.is_active());
                this.run = Some(run);
                // The last events land with the finish.
                if was_active {
                    this.reload_events(cx);
                }
            }
        });
    }

    /// Incremental after the first load: only events past the last seq.
    fn reload_events(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.view else { return };
        let after = self.events.last().map(|e| format!("&after_seq={}", e.seq)).unwrap_or_default();
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move { client.get::<Vec<Event>>(&format!("/api/runs/{id}/events?limit=1000{after}")).await });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.view != Some(id) {
                    return;
                }
                this.events_loaded = true;
                if let Ok(Ok(new)) = r {
                    for e in new {
                        if !this.events.iter().any(|x| x.seq == e.seq) {
                            this.events.push(e);
                        }
                    }
                    this.events.sort_by_key(|e| e.seq);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        if !self.loaded {
            return div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .children((0..5).map(|_| Skeleton::new(64.0).radius(RADIUS_CARD)))
                .into_any_element();
        }
        if self.runs.is_empty() {
            return anim::appear(
                "act-empty",
                empty("No activity yet", Some("Runs from chats and schedules will be listed here.".into()), cx),
            )
            .into_any_element();
        }
        let this = cx.entity();
        let mut list = div().flex().flex_col().gap(px(8.0));
        for (i, r) in self.runs.iter().enumerate() {
            let id = r.id;
            let this = this.clone();
            list = list.child(anim::stagger(
                SharedString::from(format!("act-in-{id}")),
                i,
                div().child(
                    HoverCard::new(SharedString::from(format!("act-{id}")))
                        .padding(14.0)
                        .on_click(move |_, _, cx| this.update(cx, |p, cx| p.open(Some(id), cx)))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(8.0))
                                        .child(StatusChip::new(run_status(r.status)))
                                        .child(chip(kind_tone(r), kind_label(r), cx))
                                        .when(r.parent_run_id.is_some(), |el| {
                                            el.child(
                                                div().text_size(px(text::CAPTION)).text_color(theme.muted).child("from another run"),
                                            )
                                        })
                                        .child(div().flex_1())
                                        .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(when(r))),
                                )
                                .child(div().truncate().text_color(theme.ink).child(match excerpt(r.prompt.as_deref().unwrap_or(""), 140) {
                                    s if s.is_empty() || s == "(dream)" => placeholder_prompt(r),
                                    s => s,
                                }))
                                .child(
                                    div()
                                        .font_family(theme.font_mono.clone())
                                        .text_size(px(text::CAPTION))
                                        .text_color(theme.muted)
                                        .child(cost_line(r)),
                                ),
                        ),
                ),
            ));
        }
        list.into_any_element()
    }

    fn detail(&self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        let back = div().flex().child(
            div()
                .id("act-back")
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(8.0))
                .py(px(4.0))
                .ml(px(-8.0))
                .rounded(px(RADIUS_CHIP))
                .cursor_pointer()
                .text_size(px(text::SMALL))
                .text_color(theme.muted)
                .hover(|s| s.bg(theme.hover).text_color(theme.ink))
                .on_click(move |_, _, cx| this.update(cx, |p, cx| p.open(None, cx)))
                .child(
                    icon(icons::ALT_ARROW_RIGHT)
                        .size(px(14.0))
                        .text_color(theme.muted)
                        .with_transformation(gpui::Transformation::rotate(gpui::radians(std::f32::consts::PI))),
                )
                .child("All runs"),
        );
        let Some(r) = self.run.clone().filter(|r| r.id == id) else {
            return div()
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(back)
                .child(Skeleton::new(28.0).width(260.0))
                .child(Skeleton::new(200.0).radius(RADIUS_CARD))
                .into_any_element();
        };
        let live = r.status.is_active();
        let this_open = cx.entity();
        let thread = r.thread_id;
        let head = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .flex_wrap()
                    .child(StatusChip::new(run_status(r.status)))
                    .child(chip(kind_tone(&r), kind_label(&r), cx))
                    .child(
                        div()
                            .font_family(theme.font_mono.clone())
                            .text_size(px(text::CAPTION))
                            .text_color(theme.muted)
                            .child(cost_line(&r)),
                    )
                    .child(div().flex_1())
                    .when(!thread.is_nil(), |el| {
                        el.child(
                            Button::new("act-open-thread", "Open thread")
                                .size(ButtonSize::Small)
                                .icon(icons::CHAT_ROUND_LINE)
                                .on_click(move |_, _, cx| this_open.update(cx, |_, cx| cx.emit(OpenThread(thread)))),
                        )
                    }),
            )
            .child(div().text_color(theme.ink).child(match r.prompt.as_deref().map(str::trim) {
                Some(p) if !p.is_empty() && p != "(dream)" => p.to_owned(),
                _ => placeholder_prompt(&r),
            }))
            .when_some(r.error.clone().filter(|e| !e.trim().is_empty()), |el, e| {
                el.child(
                    div()
                        .px(px(12.0))
                        .py(px(8.0))
                        .rounded(px(RADIUS_CHIP))
                        .bg(theme.bad_soft)
                        .text_size(px(text::SMALL))
                        .text_color(theme.bad)
                        .child(e),
                )
            })
            .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(format!("Started {}", when(&r))));
        let timeline = if !self.events_loaded {
            div().flex().flex_col().gap(px(10.0)).children((0..4).map(|i| Skeleton::new(14.0).width(420.0 - i as f32 * 60.0)))
        } else if self.events.is_empty() {
            div().text_size(px(text::SMALL)).text_color(theme.muted).child(if live { "Starting…" } else { "No events were kept for this run." })
        } else {
            div().flex().flex_col().gap(px(10.0)).children(self.rows.render(&self.events, true, live, window, cx))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(back)
            .child(anim::appear(SharedString::from(format!("act-head-{id}")), head))
            .child(card(cx).p(px(16.0)).child(timeline))
            .into_any_element()
    }
}

fn kind_label(r: &Run) -> String {
    match r.kind {
        RunKind::Unknown if r.prompt.as_deref() == Some("(dream)") => "dream".into(),
        RunKind::Unknown => "run".into(),
        RunKind::Followup => "draft decision".into(),
        k => k.as_str().into(),
    }
}

fn kind_tone(r: &Run) -> Tone {
    if r.kind == RunKind::Handoff { Tone::Accent } else { Tone::Muted }
}

fn placeholder_prompt(r: &Run) -> String {
    if r.prompt.as_deref() == Some("(dream)") { "Looked back over recent chats".into() } else { "(no prompt)".into() }
}

/// "Oct 4, 14:05" in local time.
fn when(r: &Run) -> String {
    r.started_at.unwrap_or(r.created_at).with_timezone(&chrono::Local).format("%b %-d, %H:%M").to_string()
}

/// "3m 5s · ≈ $0.0412 at API prices" (runs use the owner's plan; nothing is billed).
fn cost_line(r: &Run) -> String {
    let took = match r.started_at {
        Some(s) => secs_label((r.finished_at.unwrap_or_else(chrono::Utc::now) - s).num_seconds().max(0) as u64),
        None => "not started".into(),
    };
    match r.cost_usd {
        Some(c) if c > 0.0 => format!("{took} · ≈ {} at API prices", money(c)),
        _ => took,
    }
}

impl Render for ActivityTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.view {
            None => anim::appear("act-list", div().child(self.list(cx))).into_any_element(),
            Some(id) => anim::appear(SharedString::from(format!("act-run-{id}")), div().child(self.detail(id, window, cx)))
                .into_any_element(),
        }
    }
}
