//! The "Set up" checklist on the chat of a teammate hired from a template: sign in to the sites it needs (in its own
//! browser, through the computer panel's take-over), tick each one off, and turn its schedules on. It goes away once
//! everything is done, or when you hide it.

use familiar_client::{Schedule, SchedulePatch, SetupPatch};
use familiar_ui::anim;
use familiar_ui::components::{Button, ButtonSize, card};
use familiar_ui::icons::{self, icon};
use familiar_ui::theme::{Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::bot_settings::checkbox;
use crate::data::{AppData, DataEvent, swr};
use crate::templates::describe_cron;

/// "Log in to …": open this page in the teammate's browser and hand the owner the controls.
pub struct OpenLogin(pub String);

pub struct SetupCard {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    bot: Uuid,
    /// This teammate's schedules (`None` until loaded).
    schedules: Option<Vec<Schedule>>,
    busy: bool,
}

impl EventEmitter<OpenLogin> for SetupCard {}

impl SetupCard {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, bot: Uuid, cx: &mut Context<Self>) -> Self {
        cx.observe(&data, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&data, |this: &mut Self, _, ev: &DataEvent, cx| match ev {
            DataEvent::Changed(None) => this.reload(cx),
            DataEvent::Changed(Some(n)) if n.t == "schedules" && n.bot.as_deref().is_none_or(|b| b == this.bot.to_string()) => {
                this.reload(cx)
            }
            _ => {}
        })
        .detach();
        let mut this = Self { data, toasts, bot, schedules: None, busy: false };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let client = self.data.read(cx).client.clone();
        swr(self, &client, format!("/api/bots/{}/schedules", self.bot), cx, |this, list: Vec<Schedule>, _| {
            this.schedules = Some(list)
        });
    }

    fn patch(&mut self, patch: SetupPatch, cx: &mut Context<Self>) {
        let client = self.data.read(cx).client.clone();
        let bot = self.bot;
        let task = Tokio::spawn(cx, async move { client.update_bot_setup(bot, &patch).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |this, cx| match r {
                Ok(bot) => this.data.update(cx, |d, cx| d.put_bot(bot, cx)),
                Err(e) => this.toasts.update(cx, |t, cx| t.push(Tone::Bad, "Couldn't save that", Some(e.into()), cx)),
            });
        })
        .detach();
    }

    /// Turn on the template's schedules that are still off.
    fn turn_on(&mut self, ids: Vec<Uuid>, cx: &mut Context<Self>) {
        if self.busy || ids.is_empty() {
            return;
        }
        self.busy = true;
        cx.notify();
        let client = self.data.read(cx).client.clone();
        let task = Tokio::spawn(cx, async move {
            for id in ids {
                client.update_schedule(id, &SchedulePatch { enabled: Some(true), ..Default::default() }).await?;
            }
            Ok::<_, familiar_client::ApiError>(())
        });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match r {
                    Ok(()) => this.toasts.update(cx, |t, cx| t.push(Tone::Ok, "Schedules are on", None, cx)),
                    Err(e) => this.toasts.update(cx, |t, cx| t.push(Tone::Bad, "Couldn't turn them on", Some(e.into()), cx)),
                }
                this.reload(cx);
                this.data.update(cx, |d, cx| d.reload_schedules(cx));
                cx.notify();
            });
        })
        .detach();
    }

    /// One checklist row: the box, what to do, and its action.
    fn row(done: bool, tick: AnyElement, title: String, detail: String, action: Option<AnyElement>, theme: &Theme) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(px(12.0))
            .px(px(6.0))
            .py(px(6.0))
            .rounded(px(8.0))
            .child(tick)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(text::SMALL))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if done { theme.muted } else { theme.ink })
                            .when(done, |el| el.line_through())
                            .truncate()
                            .child(title),
                    )
                    .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).truncate().child(detail)),
            )
            .children(action)
    }
}

impl Render for SetupCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let Some(bot) = self.data.read(cx).bot(self.bot).cloned() else { return div() };
        let Some(setup) = bot.setup.clone().filter(|s| !s.dismissed) else { return div() };
        // In the template's order (the API lists them by prompt).
        let mine: Option<Vec<&Schedule>> = self
            .schedules
            .as_ref()
            .map(|all| setup.schedules.iter().filter_map(|id| all.iter().find(|s| s.id == *id)).collect());
        let logins_left = setup.logins.iter().filter(|l| !l.done).count();
        // Until the schedules load, assume they are still off; with nothing else to show, wait for them instead.
        let off: Vec<Uuid> = match &mine {
            Some(m) => m.iter().filter(|s| !s.enabled).map(|s| s.id).collect(),
            None if logins_left == 0 => return div(),
            None => setup.schedules.clone(),
        };
        let has_schedules = !setup.schedules.is_empty() && mine.as_ref().is_none_or(|m| !m.is_empty());
        if logins_left == 0 && off.is_empty() {
            return div();
        }
        let total = setup.logins.len() + has_schedules as usize;
        let done = setup.logins.len() - logins_left + (has_schedules && off.is_empty()) as usize;

        let mut rows = div().flex().flex_col().gap(px(2.0));
        for l in &setup.logins {
            let host = reqwest::Url::parse(&l.url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_default();
            let action = (!l.done).then(|| {
                let this = cx.entity();
                let url = l.url.clone();
                Button::new(SharedString::from(format!("setup-login-{}", l.site)), "Log in")
                    .size(ButtonSize::Small)
                    .icon(icons::MONITOR)
                    .tooltip("Opens it in the teammate's browser and hands you the controls")
                    .on_click(move |_, _, cx| this.update(cx, |_, cx| cx.emit(OpenLogin(url.clone()))))
                    .into_any_element()
            });
            let (site, was) = (l.site.clone(), l.done);
            let this = cx.entity();
            let tick = div()
                .id(SharedString::from(format!("setup-tick-{}", l.site)))
                .cursor_pointer()
                .tooltip(familiar_ui::components::tooltip_text(if was { "Not signed in after all" } else { "I'm signed in" }))
                .on_click(move |_, _, cx| {
                    this.update(cx, |p, cx| p.patch(SetupPatch { login: Some(site.clone()), done: Some(!was), ..Default::default() }, cx))
                })
                .child(checkbox(l.done, &theme).mt(px(0.0)))
                .into_any_element();
            rows = rows.child(Self::row(
                l.done,
                tick,
                format!("Log in to {}", l.site),
                if l.done { "Done. Its browser keeps you signed in.".to_owned() } else { format!("{host} · tick it off once you're in") },
                action,
                &theme,
            ));
        }
        if has_schedules {
            let named: Vec<String> = mine
                .as_ref()
                .map(|m| m.iter().map(|s| describe_cron(&s.cron)).collect())
                .unwrap_or_default();
            let n = mine.as_ref().map_or(setup.schedules.len(), |m| m.len());
            let title = if off.is_empty() {
                format!("{n} schedule{} on", if n == 1 { "" } else { "s" })
            } else {
                format!("Turn on {n} schedule{}", if n == 1 { "" } else { "s" })
            };
            let detail = if off.is_empty() {
                "They run on their own now.".to_owned()
            } else if named.is_empty() {
                "Off until you turn them on.".to_owned()
            } else {
                format!("Off until you turn them on · {}", named.join(" · "))
            };
            let action = (!off.is_empty()).then(|| {
                let this = cx.entity();
                let ids = off.clone();
                Button::new("setup-schedules", if self.busy { "Turning on…" } else { "Turn on" })
                    .size(ButtonSize::Small)
                    .icon(icons::CALENDAR)
                    .disabled(self.busy)
                    .on_click(move |_, _, cx| this.update(cx, |p, cx| p.turn_on(ids.clone(), cx)))
                    .into_any_element()
            });
            let tick = checkbox(off.is_empty(), &theme).mt(px(0.0)).into_any_element();
            rows = rows.child(Self::row(off.is_empty(), tick, title, detail, action, &theme));
        }

        let hide_this = cx.entity();
        let body = card(cx)
            .px(px(14.0))
            .pt(px(12.0))
            .pb(px(8.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(icon(icons::CHECKLIST).size(px(16.0)).text_color(theme.accent))
                    .child(div().font_weight(FontWeight::MEDIUM).child(format!("Set up {}", bot.name)))
                    .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(format!("{done} of {total} done")))
                    .child(div().flex_1())
                    .child(
                        Button::icon_only("setup-hide", icons::CLOSE)
                            .size(ButtonSize::Small)
                            .tooltip("Hide this checklist")
                            .on_click(move |_, _, cx| {
                                hide_this.update(cx, |p, cx| p.patch(SetupPatch { dismissed: Some(true), ..Default::default() }, cx))
                            }),
                    ),
            )
            .child(rows);
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(px(28.0))
            .pt(px(16.0))
            .child(div().w_full().max_w(px(720.0)).child(anim::appear(SharedString::from(format!("setup-{}", self.bot)), body)))
    }
}
