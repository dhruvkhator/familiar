//! The Schedules page: every teammate's schedules in one list (who, what, when in plain words, the next run, how the
//! last one went), with a switch, Run now, an inline editor (name, a few friendly time presets or a custom cron,
//! instructions) and delete with a confirmation. Data comes from [`AppData::schedules`] (`GET /api/schedules`).

use std::collections::HashSet;

use chrono::{DateTime, Local, Utc};
use familiar_client::{Schedule, SchedulePatch};
use familiar_ui::anim;
use familiar_ui::components::{Button, ButtonSize, Segmented, Skeleton, StatusChip, Switch, card, chip, divider};
use familiar_ui::icons;
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::theme::{RADIUS_CARD, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Window,
    div, prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState, TextareaState};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::data::{AppData, ago, avatar_of, excerpt, run_status, until};
use crate::templates::describe_cron;
use crate::text_input;

/// The editor's ways to say when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Daily,
    Weekdays,
    Weekly,
    Hourly,
    Custom,
}

const PRESETS: [(Preset, &str); 5] = [
    (Preset::Daily, "Every day"),
    (Preset::Weekdays, "Weekdays"),
    (Preset::Weekly, "Weekly"),
    (Preset::Hourly, "Hourly"),
    (Preset::Custom, "Custom"),
];

/// Weekly's day picker, Monday first, with the cron day number of each.
const DAYS: [(&str, u32); 7] = [("Mon", 1), ("Tue", 2), ("Wed", 3), ("Thu", 4), ("Fri", 5), ("Sat", 6), ("Sun", 0)];

/// When a schedule runs, as the editor shows it: the preset, the time (hour, minute) and the weekday (cron number).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct When {
    pub preset: Preset,
    pub hour: u32,
    pub minute: u32,
    pub day: u32,
}

/// Read a cron as one of the presets; anything else is Custom (and keeps 9:00 / Monday for switching back).
pub fn when_of(cron: &str) -> When {
    let custom = When { preset: Preset::Custom, hour: 9, minute: 0, day: 1 };
    let f: Vec<&str> = cron.split_whitespace().collect();
    if f.len() != 5 || f[2] != "*" || f[3] != "*" {
        return custom;
    }
    let Ok(minute) = f[0].parse::<u32>() else { return custom };
    if minute > 59 {
        return custom;
    }
    if f[1] == "*" && f[4] == "*" {
        return When { preset: Preset::Hourly, hour: 9, minute, day: 1 };
    }
    let Ok(hour) = f[1].parse::<u32>() else { return custom };
    if hour > 23 {
        return custom;
    }
    match f[4] {
        "*" => When { preset: Preset::Daily, hour, minute, day: 1 },
        "1-5" => When { preset: Preset::Weekdays, hour, minute, day: 1 },
        d => match d.parse::<u32>() {
            Ok(d) if d <= 7 => When { preset: Preset::Weekly, hour, minute, day: d % 7 },
            _ => When { hour, minute, ..custom },
        },
    }
}

/// The cron for a preset (Custom has none: the owner's own expression is used).
pub fn cron_of(w: When) -> Option<String> {
    let (h, m) = (w.hour, w.minute);
    match w.preset {
        Preset::Daily => Some(format!("{m} {h} * * *")),
        Preset::Weekdays => Some(format!("{m} {h} * * 1-5")),
        Preset::Weekly => Some(format!("{m} {h} * * {}", w.day)),
        Preset::Hourly => Some(format!("{m} * * * *")),
        Preset::Custom => None,
    }
}

/// "9:30", "09:30", "930", "9" → (hour, minute).
pub fn parse_time(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    let (h, m) = match s.split_once(':') {
        Some((h, m)) => (h, m),
        None if s.len() > 2 => s.split_at(s.len() - 2),
        None => (s, "0"),
    };
    let (h, m) = (h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

/// A cron in plain words, including the hourly shape the template wording leaves as an expression.
pub fn cron_words(cron: &str) -> String {
    let w = when_of(cron);
    match w.preset {
        Preset::Hourly if w.minute == 0 => "Every hour".to_owned(),
        Preset::Hourly => format!("Every hour at :{:02}", w.minute),
        _ => describe_cron(cron),
    }
}

/// When it runs next: "Tue 09:00 · in 3h", or why it won't.
pub fn next_words(s: &Schedule) -> String {
    match (s.enabled, s.next_run_at) {
        (false, _) => "Off: it won't run until you turn it on".to_owned(),
        (true, Some(t)) => format!("Next {} · {}", local(t), until(Some(t))),
        (true, None) => "Next run being worked out".to_owned(),
    }
}

fn local(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%a %H:%M").to_string()
}

/// The schedule's name: its label (the title of its thread, without the "Schedule: " the database gives a new one), or
/// the start of its instructions.
pub fn label_of(s: &Schedule) -> String {
    match s.label.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
        Some(l) => l.strip_prefix("Schedule: ").unwrap_or(l).to_owned(),
        None => excerpt(&s.prompt, 60),
    }
}

/// The open editor of one schedule.
struct Editor {
    id: Uuid,
    label: Entity<InputState>,
    prompt: Entity<TextareaState>,
    time: Entity<InputState>,
    cron: Entity<InputState>,
    when: When,
}

pub struct SchedulesPage {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    editor: Option<Editor>,
    confirm_delete: Option<Uuid>,
    /// Schedules with a request in flight (switch, Run now, save, delete).
    busy: HashSet<Uuid>,
}

impl SchedulesPage {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, cx: &mut Context<Self>) -> Self {
        cx.observe(&data, |_, _, cx| cx.notify()).detach();
        data.update(cx, |d, cx| d.reload_schedules(cx));
        Self { data, toasts, editor: None, confirm_delete: None, busy: HashSet::new() }
    }

    fn toast(&self, tone: Tone, title: &str, detail: Option<String>, cx: &mut Context<Self>) {
        let title = title.to_owned();
        self.toasts.update(cx, |t, cx| t.push(tone, title, detail.map(Into::into), cx));
    }

    /// Run an API call for one schedule: busy while it runs, a toast when it fails (or `done` when it worked), and the
    /// list reloaded after.
    fn act<T: Send + 'static>(
        &mut self,
        id: Uuid,
        call: impl std::future::Future<Output = Result<T, familiar_client::ApiError>> + Send + 'static,
        done: Option<String>,
        failed: &'static str,
        cx: &mut Context<Self>,
    ) {
        if !self.busy.insert(id) {
            return;
        }
        cx.notify();
        let task = Tokio::spawn(cx, call);
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map(|_| ()).map_err(|e| e.message()));
            let _ = this.update(cx, |this, cx| {
                this.busy.remove(&id);
                match r {
                    Ok(()) => {
                        if let Some(d) = done {
                            this.toast(Tone::Ok, &d, None, cx);
                        }
                    }
                    Err(e) => this.toast(Tone::Bad, failed, Some(e), cx),
                }
                this.data.update(cx, |d, cx| d.reload_schedules(cx));
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle(&mut self, s: &Schedule, on: bool, cx: &mut Context<Self>) {
        let client = self.data.read(cx).client.clone();
        let id = s.id;
        // Shown at once; the reload corrects it if the request fails.
        self.data.update(cx, |d, _| {
            if let Some(x) = d.schedules.iter_mut().find(|x| x.id == id) {
                x.enabled = on;
            }
        });
        let patch = SchedulePatch { enabled: Some(on), ..Default::default() };
        self.act(id, async move { client.update_schedule(id, &patch).await }, None, "Couldn't change that schedule", cx);
    }

    fn run_now(&mut self, s: &Schedule, cx: &mut Context<Self>) {
        let client = self.data.read(cx).client.clone();
        let (id, done) = (s.id, format!("Started “{}”", label_of(s)));
        self.act(id, async move { client.run_schedule(id).await }, Some(done), "Couldn't start it", cx);
    }

    fn delete(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.confirm_delete = None;
        if self.editor.as_ref().is_some_and(|e| e.id == id) {
            self.editor = None;
        }
        let client = self.data.read(cx).client.clone();
        self.act(id, async move { client.delete_schedule(id).await }, Some("Schedule deleted".into()), "Couldn't delete it", cx);
    }

    fn edit(&mut self, s: &Schedule, window: &mut Window, cx: &mut Context<Self>) {
        let when = when_of(&s.cron);
        let label = text_input::new_line("A name for it", false, window, cx);
        label.update(cx, |x, cx| x.set_value(label_of(s), window, cx));
        let prompt = text_input::new_field("What it should do each time", false, 10, window, cx);
        prompt.update(cx, |x, cx| x.set_value(s.prompt.clone(), window, cx));
        let time = text_input::new_line("09:00", false, window, cx);
        time.update(cx, |x, cx| x.set_value(format!("{:02}:{:02}", when.hour, when.minute), window, cx));
        let cron = text_input::new_line("minute hour day month weekday", false, window, cx);
        cron.update(cx, |x, cx| x.set_value(s.cron.clone(), window, cx));
        for input in [&label, &time, &cron] {
            cx.subscribe_in(input, window, |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } => this.save(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            })
            .detach();
        }
        cx.subscribe(&prompt, |_, _, _: &InputEvent, cx| cx.notify()).detach();
        self.editor = Some(Editor { id: s.id, label, prompt, time, cron, when });
        self.confirm_delete = None;
        cx.notify();
    }

    /// The editor's cron, or why it can't be saved.
    fn editor_cron(e: &Editor, cx: &gpui::App) -> Result<String, &'static str> {
        if e.when.preset == Preset::Custom {
            let c = e.cron.read(cx).value().split_whitespace().collect::<Vec<_>>().join(" ");
            return if c.split(' ').count() == 5 { Ok(c) } else { Err("A cron has 5 parts: minute hour day month weekday") };
        }
        let (hour, minute) = if e.when.preset == Preset::Hourly {
            (0, e.when.minute)
        } else {
            parse_time(&e.time.read(cx).value()).ok_or("Write the time as 9:00 or 17:30")?
        };
        cron_of(When { hour, minute, ..e.when }).ok_or("Pick when it runs")
    }

    fn save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = self.editor.as_ref() else { return };
        let cron = match Self::editor_cron(e, cx) {
            Ok(c) => c,
            Err(why) => return self.toast(Tone::Bad, "Can't save yet", Some(why.into()), cx),
        };
        let label = e.label.read(cx).value().trim().to_owned();
        let prompt = e.prompt.read(cx).value().trim().to_owned();
        if label.is_empty() || prompt.is_empty() {
            return self.toast(Tone::Bad, "Can't save yet", Some("Give it a name and instructions.".into()), cx);
        }
        let id = e.id;
        self.editor = None;
        let client = self.data.read(cx).client.clone();
        let patch = SchedulePatch { label: Some(label), cron: Some(cron), prompt: Some(prompt), ..Default::default() };
        self.act(id, async move { client.update_schedule(id, &patch).await }, Some("Schedule saved".into()), "Couldn't save it", cx);
    }

    fn row(&self, s: &Schedule, i: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let id = s.id;
        let key = id.as_u128() as u64;
        let (avatar, bot) = {
            let d = self.data.read(cx);
            match d.bot(s.bot_id) {
                Some(b) => (avatar_of(b), b.name.clone()),
                None => (familiar_ui::mascot::default_avatar(&s.bot_id.to_string()), s.bot_name.clone().unwrap_or_default()),
            }
        };
        let busy = self.busy.contains(&id);
        let last = match s.last_status {
            Some(st) => div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(StatusChip::new(run_status(st)))
                .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(ago(s.last_finished_at.or(s.last_run_at))))
                .when_some(s.last_error.clone().filter(|e| !e.trim().is_empty()), |el, e| {
                    el.child(div().text_size(px(text::CAPTION)).text_color(theme.bad).truncate().child(excerpt(&e, 80)))
                }),
            None => div().text_size(px(text::CAPTION)).text_color(theme.muted).child("Hasn't run yet"),
        };
        let (s1, s2, s3, s4) = (s.clone(), s.clone(), s.clone(), cx.entity());
        let this = cx.entity();
        let main = div()
            .flex()
            .items_center()
            .gap(px(14.0))
            .px(px(16.0))
            .py(px(12.0))
            .child(Mascot::new(format!("sched-{id}"), avatar, if s.enabled { MascotState::Idle } else { MascotState::Paused }, 36.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(3.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().font_weight(FontWeight::MEDIUM).text_color(theme.ink).truncate().child(label_of(s)))
                            .when(s.kind == familiar_client::RunKind::Proactive, |el| el.child(chip(Tone::Accent, "research only", cx))),
                    )
                    .child(div().text_size(px(text::SMALL)).text_color(theme.muted).truncate().child(SharedString::from(format!(
                        "{bot} · {} · {}",
                        cron_words(&s.cron),
                        next_words(s)
                    ))))
                    .child(last),
            )
            .child(
                Switch::new(("sched-on", key), s.enabled)
                    .on_toggle(move |on, _, cx| this.update(cx, |p, cx| p.toggle(&s1, on, cx))),
            )
            .child({
                let this = cx.entity();
                Button::new(("sched-run", key), "Run now")
                    .size(ButtonSize::Small)
                    .icon(icons::ARROW_RIGHT)
                    .disabled(busy)
                    .tooltip("Starts it once now; its times don't change")
                    .on_click(move |_, _, cx| this.update(cx, |p, cx| p.run_now(&s2, cx)))
            })
            .child({
                let this = cx.entity();
                Button::icon_only(("sched-edit", key), icons::PEN)
                    .size(ButtonSize::Small)
                    .tooltip("Edit")
                    .on_click(move |_, window, cx| this.update(cx, |p, cx| p.edit(&s3, window, cx)))
            })
            .child(
                Button::icon_only(("sched-del", key), icons::CLOSE)
                    .size(ButtonSize::Small)
                    .tooltip("Delete")
                    .on_click(move |_, _, cx| {
                        s4.update(cx, |p, cx| {
                            p.confirm_delete = Some(id);
                            cx.notify()
                        })
                    }),
            );
        let mut row = div().flex().flex_col().child(main);
        if self.confirm_delete == Some(id) {
            let (yes, no) = (cx.entity(), cx.entity());
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(16.0))
                    .pb(px(12.0))
                    .pl(px(66.0))
                    .child(div().flex_1().text_size(px(text::SMALL)).text_color(theme.bad).child(SharedString::from(format!(
                        "Delete “{}” for good? {bot} stops doing it.",
                        label_of(s)
                    ))))
                    .child(Button::new(("sched-del-no", key), "Cancel").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| {
                        no.update(cx, |p, cx| {
                            p.confirm_delete = None;
                            cx.notify()
                        })
                    }))
                    .child(
                        Button::new(("sched-del-yes", key), "Delete")
                            .danger()
                            .size(ButtonSize::Small)
                            .on_click(move |_, _, cx| yes.update(cx, |p, cx| p.delete(id, cx))),
                    ),
            );
        }
        if let Some(e) = self.editor.as_ref().filter(|e| e.id == id) {
            row = row.child(self.editor_view(e, window, cx));
        }
        anim::stagger(SharedString::from(format!("sched-in-{id}")), i, row).into_any_element()
    }

    fn editor_view(&self, e: &Editor, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let key = e.id.as_u128() as u64;
        let label = |s: &'static str| div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child(s);
        let preset = PRESETS.iter().position(|(p, _)| *p == e.when.preset).unwrap_or(0);
        let this = cx.entity();
        let presets = Segmented::new(("sched-preset", key), PRESETS.iter().map(|(_, l)| ((*l).into(), None)).collect(), preset)
            .segment_width(84.0)
            .on_select(move |i, _, cx| {
                this.update(cx, |p, cx| {
                    if let Some(e) = p.editor.as_mut() {
                        e.when.preset = PRESETS[i].0;
                    }
                    cx.notify()
                })
            });
        let preview = match Self::editor_cron(e, cx) {
            Ok(c) => (cron_words(&c), theme.muted),
            Err(why) => (why.to_owned(), theme.bad),
        };
        let mut when = div().flex().items_center().gap(px(10.0)).flex_wrap();
        match e.when.preset {
            Preset::Custom => {
                when = when.child(div().w(px(240.0)).child(text_input::field(("sched-cron", key), &e.cron, 38.0, window, cx)));
            }
            Preset::Hourly => {
                let this = cx.entity();
                let minutes = [0u32, 15, 30, 45];
                let at = minutes.iter().position(|m| *m == e.when.minute).unwrap_or(0);
                when = when.child(
                    Segmented::new(("sched-min", key), minutes.iter().map(|m| (format!(":{m:02}").into(), None)).collect(), at)
                        .segment_width(52.0)
                        .on_select(move |i, _, cx| {
                            this.update(cx, |p, cx| {
                                if let Some(e) = p.editor.as_mut() {
                                    e.when.minute = minutes[i];
                                }
                                cx.notify()
                            })
                        }),
                );
            }
            _ => {
                when = when.child(div().w(px(96.0)).child(text_input::field(("sched-time", key), &e.time, 38.0, window, cx)));
                if e.when.preset == Preset::Weekly {
                    let this = cx.entity();
                    let at = DAYS.iter().position(|(_, d)| *d == e.when.day).unwrap_or(0);
                    when = when.child(
                        Segmented::new(("sched-day", key), DAYS.iter().map(|(l, _)| ((*l).into(), None)).collect(), at)
                            .segment_width(46.0)
                            .on_select(move |i, _, cx| {
                                this.update(cx, |p, cx| {
                                    if let Some(e) = p.editor.as_mut() {
                                        e.when.day = DAYS[i].1;
                                    }
                                    cx.notify()
                                })
                            }),
                    );
                }
            }
        }
        when = when.child(div().text_size(px(text::SMALL)).text_color(preview.1).child(preview.0));
        let (cancel, save) = (cx.entity(), cx.entity());
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .mx(px(16.0))
            .mb(px(14.0))
            .ml(px(66.0))
            .p(px(14.0))
            .rounded(px(10.0))
            .bg(theme.sunken)
            .child(div().flex().flex_col().gap(px(6.0)).child(label("Name")).child(text_input::field(("sched-label", key), &e.label, 38.0, window, cx)))
            // Wrapped in rows so the segmented controls keep their own width instead of stretching.
            .child(div().flex().flex_col().gap(px(6.0)).child(label("When")).child(div().flex().child(presets)).child(when))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(label("What it does each time"))
                    .child(text_input::field(("sched-prompt", key), &e.prompt, 80.0, window, cx)),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(Button::new(("sched-cancel", key), "Cancel").ghost().on_click(move |_, _, cx| {
                        cancel.update(cx, |p, cx| {
                            p.editor = None;
                            cx.notify()
                        })
                    }))
                    .child(
                        Button::new(("sched-save", key), "Save")
                            .primary()
                            .on_click(move |_, window, cx| save.update(cx, |p, cx| p.save(window, cx))),
                    ),
            )
            .into_any_element()
    }
}

impl Render for SchedulesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::count("SchedulesPage");
        let theme = Theme::of(cx).clone();
        let (list, loaded) = {
            let d = self.data.read(cx);
            (d.schedules.clone(), d.schedules_loaded)
        };
        if self.editor.as_ref().is_some_and(|e| !list.iter().any(|s| s.id == e.id)) {
            self.editor = None;
        }
        let on = list.iter().filter(|s| s.enabled).count();
        let head = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).text_color(theme.ink).child("Schedules"))
                    .when(!list.is_empty(), |el| el.child(chip(Tone::Muted, format!("{on} of {} on", list.len()), cx))),
            )
            .child(div().text_size(px(text::LEAD)).text_color(theme.muted).child(
                "What your teammates do on their own, and when. A schedule that is off never runs until you turn it on.",
            ));
        let mut page = div().flex().flex_col().gap(px(24.0)).child(anim::appear("sched-head", head));
        if !loaded && list.is_empty() {
            return page.child(div().flex().flex_col().gap(px(8.0)).children((0..3).map(|_| Skeleton::new(64.0).radius(RADIUS_CARD))));
        }
        if list.is_empty() {
            return page.child(anim::appear(
                "sched-empty",
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(10.0))
                    .py(px(48.0))
                    .child(Mascot::new("sched-empty", familiar_ui::mascot::default_avatar("schedules"), MascotState::Idle, 88.0))
                    .child(div().text_size(px(text::TITLE)).font_weight(FontWeight::SEMIBOLD).child("No schedules yet"))
                    .child(div().text_color(theme.muted).child(
                        "Ask a teammate to do something every morning, or hire one from a template: its schedules show up here.",
                    )),
            ));
        }
        let mut rows = card(cx).flex().flex_col().overflow_hidden();
        for (i, s) in list.iter().enumerate() {
            if i > 0 {
                rows = rows.child(divider(cx));
            }
            rows = rows.child(self.row(s, i, window, cx));
        }
        page = page.child(rows);
        page
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_round_trip() {
        for (cron, preset) in [
            ("0 9 * * *", Preset::Daily),
            ("30 8 * * 1-5", Preset::Weekdays),
            ("0 16 * * 5", Preset::Weekly),
            ("15 * * * *", Preset::Hourly),
        ] {
            let w = when_of(cron);
            assert_eq!(w.preset, preset, "{cron}");
            assert_eq!(cron_of(w).as_deref(), Some(cron));
        }
        assert_eq!(when_of("0 9 * * 7").day, 0, "Sunday is 0 or 7");
        for c in ["0 8 * * 2,4", "*/5 * * * *", "0 9 1 * *", "nonsense"] {
            assert_eq!(when_of(c).preset, Preset::Custom, "{c}");
        }
        assert_eq!(cron_of(When { preset: Preset::Custom, hour: 9, minute: 0, day: 1 }), None);
    }

    #[test]
    fn times_and_words() {
        assert_eq!(parse_time("9:30"), Some((9, 30)));
        assert_eq!(parse_time(" 09:05 "), Some((9, 5)));
        assert_eq!(parse_time("1730"), Some((17, 30)));
        assert_eq!(parse_time("7"), Some((7, 0)));
        for bad in ["24:00", "9:60", "", "nine"] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
        assert_eq!(cron_words("0 * * * *"), "Every hour");
        assert_eq!(cron_words("15 * * * *"), "Every hour at :15");
        assert_eq!(cron_words("30 9 * * 1-5"), "Weekdays at 9:30");
    }

    #[test]
    fn labels_and_next() {
        let s = Schedule { label: Some("Schedule: check the inbox".into()), prompt: "x".into(), ..Default::default() };
        assert_eq!(label_of(&s), "check the inbox");
        let s = Schedule { label: None, prompt: "Summarise   the week".into(), ..Default::default() };
        assert_eq!(label_of(&s), "Summarise the week");
        assert!(next_words(&Schedule { enabled: false, ..Default::default() }).starts_with("Off"));
        let soon = Schedule { enabled: true, next_run_at: Some(Utc::now() + chrono::Duration::hours(3)), ..Default::default() };
        assert!(next_words(&soon).starts_with("Next ") && next_words(&soon).ends_with("in 3h"));
    }
}
