//! "Hire the GTM crew": the bundle from `GET /api/templates/bundles` as one form. Its shared questions (what you sell,
//! who to, the ask, who it's from, the voice, follow-up days), the five teammates it creates with their schedules in
//! plain words (all off until you turn them on) and the sites they sign in to; then one call hires them all (all or
//! none). Afterwards the crew and a Set up checklist: sign in to the sites (in each teammate's own browser), connect
//! Google Workspace for email (and link it to them), turn the schedules on.

use std::collections::{BTreeMap, HashMap};

use familiar_client::{Bot, Connector, ConnectorPreset, FromBundle, SchedulePatch, Template, TemplateBundle};
use familiar_ui::anim;
use familiar_ui::components::{Button, ButtonSize, Skeleton, card, chip, divider};
use familiar_ui::icons::{self, icon};
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::theme::{RADIUS_CARD, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState, TextareaState};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::bot_settings::checkbox;
use crate::data::{AppData, avatar_of, swr};
use crate::schedules::cron_words;
use crate::templates::{mascot_tile, template_avatar};
use crate::text_input;

/// The bundle this flow hires.
pub const CREW: &str = "gtm-crew";
/// The connector its email teammates use.
const EMAIL_CONNECTOR: &str = "google-workspace";

pub enum CrewEvent {
    Cancelled,
    /// Back to where it was opened from.
    Back,
    /// The crew exists (in the bundle's order).
    Hired(Vec<Bot>),
    /// Open a teammate's page.
    Open(Uuid),
    /// Open a teammate's page and sign in to a site in its browser.
    Login { bot: Uuid, url: String },
    Schedules,
    Crm,
}

enum Answer {
    Line(Entity<InputState>),
    Area(Entity<TextareaState>),
}

impl Answer {
    fn value(&self, cx: &App) -> String {
        match self {
            Answer::Line(s) => s.read(cx).value().to_string(),
            Answer::Area(s) => s.read(cx).value().to_string(),
        }
    }
}

pub struct CrewHire {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    bundle: Option<TemplateBundle>,
    templates: Option<Vec<Template>>,
    presets: Vec<ConnectorPreset>,
    connectors: Option<Vec<Connector>>,
    /// The connectors linked to each hired teammate.
    linked: HashMap<Uuid, Vec<Uuid>>,
    answers: Vec<(String, String, Answer)>,
    busy: bool,
    error: Option<String>,
    /// The hired crew (then the page is the checklist).
    hired: Option<Vec<Uuid>>,
}

impl EventEmitter<CrewEvent> for CrewHire {}

impl CrewHire {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, bundle: Option<TemplateBundle>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::data::redraw_on_updates(&data, cx);
        let client = data.read(cx).client.clone();
        let mut this = Self {
            data,
            toasts,
            bundle: None,
            templates: None,
            presets: Vec::new(),
            connectors: None,
            linked: HashMap::new(),
            answers: Vec::new(),
            busy: false,
            error: None,
            hired: None,
        };
        match bundle {
            Some(b) => this.set_bundle(b, window, cx),
            None => {
                let task = Tokio::spawn(cx, {
                    let client = client.clone();
                    async move { client.template_bundles().await }
                });
                cx.spawn_in(window, async move |this, cx| {
                    let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
                    let _ = this.update_in(cx, |p, window, cx| {
                        match r.map(|list| list.into_iter().find(|b| b.id == CREW)) {
                            Ok(Some(b)) => p.set_bundle(b, window, cx),
                            Ok(None) => p.error = Some("This version of Familiar doesn't have the GTM crew.".into()),
                            Err(e) => p.error = Some(e),
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        swr(&mut this, &client, "/api/templates".into(), cx, |this, list: Vec<Template>, _| this.templates = Some(list));
        swr(&mut this, &client, "/api/connectors/presets".into(), cx, |this, p: Vec<ConnectorPreset>, _| this.presets = p);
        this
    }

    fn set_bundle(&mut self, b: TemplateBundle, window: &mut Window, cx: &mut Context<Self>) {
        for q in &b.questions {
            let placeholder = format!("e.g. {}", q.placeholder);
            let field = if q.multiline {
                let s = cx.new(|cx| TextareaState::new(window, cx).placeholder(placeholder).auto_grow(2, 5));
                cx.subscribe(&s, |_, _, _: &InputEvent, cx| cx.notify()).detach();
                Answer::Area(s)
            } else {
                let s = text_input::new_line(placeholder, false, window, cx);
                cx.subscribe(&s, |_, _, _: &InputEvent, cx| cx.notify()).detach();
                Answer::Line(s)
            };
            self.answers.push((q.key.clone(), q.label.clone(), field));
        }
        if let Some((_, _, first)) = self.answers.first() {
            match first {
                Answer::Line(s) => s.update(cx, |s, cx| s.focus(window, cx)),
                Answer::Area(s) => s.update(cx, |s, cx| s.focus(window, cx)),
            }
        }
        self.bundle = Some(b);
    }

    fn toast(&self, tone: Tone, title: impl Into<SharedString>, body: Option<String>, cx: &mut Context<Self>) {
        let title = title.into();
        self.toasts.update(cx, |t, cx| t.push(tone, title, body.map(Into::into), cx));
    }

    /// The bundle's teammates, in its order (`None` until the catalog is in).
    fn crew(&self) -> Option<Vec<Template>> {
        let (b, all) = (self.bundle.as_ref()?, self.templates.as_ref()?);
        Some(b.templates.iter().filter_map(|id| all.iter().find(|t| &t.id == id).cloned()).collect())
    }

    pub fn hire(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let answers: BTreeMap<String, String> =
            self.answers.iter().map(|(k, _, f)| (k.clone(), f.value(cx).trim().to_owned())).filter(|(_, v)| !v.is_empty()).collect();
        self.busy = true;
        self.error = None;
        cx.notify();
        let client = self.data.read(cx).client.clone();
        let task = Tokio::spawn(cx, async move { client.create_bundle(CREW, &FromBundle { answers: Some(answers) }).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                p.busy = false;
                match r {
                    Ok(h) => {
                        let bots: Vec<Bot> = h.hired.into_iter().map(|h| h.bot).collect();
                        p.hired = Some(bots.iter().map(|b| b.id).collect());
                        p.toast(Tone::Ok, format!("Your GTM crew is here: {} teammates", bots.len()), None, cx);
                        p.load_links(cx);
                        cx.emit(CrewEvent::Hired(bots));
                    }
                    Err(e) => {
                        p.error = Some(e.clone());
                        p.toast(Tone::Bad, "Couldn't hire the crew", Some(e), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Which connectors exist, and which each new teammate has.
    fn load_links(&mut self, cx: &mut Context<Self>) {
        let Some(ids) = self.hired.clone() else { return };
        let client = self.data.read(cx).client.clone();
        let task = Tokio::spawn(cx, async move {
            let all = client.connectors().await.ok();
            let mut linked = HashMap::new();
            for id in ids {
                if let Ok(list) = client.bot_connectors(id).await {
                    linked.insert(id, list.into_iter().map(|c| c.id).collect::<Vec<_>>());
                }
            }
            (all, linked)
        });
        cx.spawn(async move |this, cx| {
            if let Ok((all, linked)) = task.await {
                let _ = this.update(cx, |p, cx| {
                    p.connectors = all;
                    p.linked = linked;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// The hired teammates whose template uses the email connector.
    fn email_bots(&self, cx: &App) -> Vec<Uuid> {
        let (Some(ids), Some(all)) = (self.hired.as_ref(), self.templates.as_ref()) else { return Vec::new() };
        let d = self.data.read(cx);
        ids.iter()
            .filter(|id| {
                let tpl = d.bot(**id).and_then(|b| b.setup.as_ref()).and_then(|s| s.template.clone());
                tpl.and_then(|t| all.iter().find(|x| x.id == t)).is_some_and(|t| t.connectors.iter().any(|c| c == EMAIL_CONNECTOR))
            })
            .copied()
            .collect()
    }

    /// Link the Google Workspace connector to the teammates that use it (keeping what each already has).
    fn link_email(&mut self, connector: Uuid, cx: &mut Context<Self>) {
        let bots = self.email_bots(cx);
        let linked = self.linked.clone();
        let client = self.data.read(cx).client.clone();
        self.busy = true;
        cx.notify();
        let task = Tokio::spawn(cx, async move {
            for b in bots {
                let mut ids = linked.get(&b).cloned().unwrap_or_default();
                if !ids.contains(&connector) {
                    ids.push(connector);
                    client.set_bot_connectors(b, &ids).await?;
                }
            }
            Ok::<_, familiar_client::ApiError>(())
        });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                p.busy = false;
                match r {
                    Ok(()) => p.toast(Tone::Ok, "Google Workspace is linked to the crew", None, cx),
                    Err(e) => p.toast(Tone::Bad, "Couldn't link it", Some(e), cx),
                }
                p.load_links(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Turn on every schedule the crew was hired with.
    fn schedules_on(&mut self, ids: Vec<Uuid>, cx: &mut Context<Self>) {
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
            let _ = this.update(cx, |p, cx| {
                p.busy = false;
                match r {
                    Ok(()) => p.toast(Tone::Ok, "The crew's schedules are on", None, cx),
                    Err(e) => p.toast(Tone::Bad, "Couldn't turn them all on", Some(e), cx),
                }
                p.data.update(cx, |d, cx| d.reload_schedules(cx));
                cx.notify();
            });
        })
        .detach();
    }

    // ---- drawing ------------------------------------------------------------------------------------------------

    /// The crew's faces, overlapping a little.
    fn faces(&self, avatars: Vec<(String, familiar_ui::mascot::Avatar)>, size: f32, theme: &Theme) -> AnyElement {
        let mut row = div().flex().items_end();
        for (i, (key, a)) in avatars.into_iter().enumerate() {
            row = row.child(
                div()
                    .when(i > 0, |el| el.ml(px(-size * 0.18)))
                    .size(px(size + 6.0))
                    .rounded_full()
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.line)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Mascot::new(format!("crew-face-{key}"), a, MascotState::Idle, size * 0.86)),
            );
        }
        row.into_any_element()
    }

    fn form(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let Some(bundle) = self.bundle.clone() else {
            return match self.error.clone() {
                Some(e) => div().text_color(theme.bad).child(e).into_any_element(),
                None => div().flex().flex_col().gap(px(12.0)).child(Skeleton::new(40.0).width(320.0)).child(Skeleton::new(220.0).radius(RADIUS_CARD)).into_any_element(),
            };
        };
        let crew = self.crew();
        let faces = crew
            .as_ref()
            .map(|c| self.faces(c.iter().map(|t| (t.id.clone(), template_avatar(t))).collect(), 44.0, &theme))
            .unwrap_or_else(|| Skeleton::new(50.0).width(200.0).into_any_element());
        let back = cx.entity();
        let head = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .id("crew-back")
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .cursor_pointer()
                    .text_size(px(text::SMALL))
                    .text_color(theme.muted)
                    .hover(|s| s.text_color(theme.ink))
                    .on_click(move |_, _, cx| back.update(cx, |_, cx| cx.emit(CrewEvent::Back)))
                    .child(
                        icon(icons::ALT_ARROW_RIGHT)
                            .size(px(14.0))
                            .text_color(theme.muted)
                            .with_transformation(gpui::Transformation::rotate(gpui::radians(std::f32::consts::PI))),
                    )
                    .child("Back"),
            )
            .child(faces)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).child(format!("Hire the {}", bundle.name)))
                            .child(chip(Tone::Accent, format!("{} teammates", bundle.templates.len()), cx)),
                    )
                    .child(div().text_size(px(text::LEAD)).text_color(theme.muted).child(bundle.summary.clone())),
            );

        let mut questions = div().flex().flex_col().gap(px(14.0));
        for (i, (key, label, field)) in self.answers.iter().enumerate() {
            let id = SharedString::from(format!("crew-q-{key}"));
            let input = match field {
                Answer::Line(s) => text_input::field(id, s, 38.0, window, cx).into_any_element(),
                Answer::Area(s) => text_input::field(id, s, 60.0, window, cx).into_any_element(),
            };
            questions = questions.child(anim::stagger(
                SharedString::from(format!("crew-q-in-{key}")),
                i,
                div().flex().flex_col().gap(px(6.0)).child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child(label.clone())).child(input),
            ));
        }

        let mut who = card(cx).flex().flex_col();
        match &crew {
            None => {
                for i in 0..bundle.templates.len() {
                    if i > 0 {
                        who = who.child(divider(cx));
                    }
                    who = who.child(div().p(px(14.0)).child(Skeleton::new(44.0).radius(10.0)));
                }
            }
            Some(list) => {
                for (i, t) in list.iter().enumerate() {
                    if i > 0 {
                        who = who.child(divider(cx));
                    }
                    who = who.child(self.member(t, cx));
                }
            }
        }

        let go = cx.entity();
        let cancel = cx.entity();
        div()
            .flex()
            .flex_col()
            .gap(px(26.0))
            .child(head)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(div().text_size(px(text::TITLE)).font_weight(FontWeight::SEMIBOLD).child("A few questions, once for all of them"))
                            .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child(
                                "Your answers go into each teammate's instructions. Skip any you like: they ask you when it matters.",
                            )),
                    )
                    .child(questions),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(div().text_size(px(text::TITLE)).font_weight(FontWeight::SEMIBOLD).child("Who you're hiring"))
                    .child(who)
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap(px(8.0))
                            .text_size(px(text::CAPTION))
                            .text_color(theme.muted)
                            .child(div().pt(px(1.0)).child(icon(icons::INFO_CIRCLE).size(px(14.0)).text_color(theme.muted)))
                            .child(
                                "Every schedule starts off, so nothing runs until you turn it on. Nothing goes out without your OK: each \
                                 email or message waits in Needs you, and anyone who opts out is never contacted again.",
                            ),
                    ),
            )
            .when_some(self.error.clone(), |el, e| el.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        Button::new("crew-hire", if self.busy { "Hiring…" } else { "Hire the crew" })
                            .primary()
                            .icon(icons::PLUS)
                            .disabled(self.busy || crew.is_none())
                            .on_click(move |_, _, cx| go.update(cx, |p, cx| p.hire(cx))),
                    )
                    .child(Button::new("crew-cancel", "Cancel").ghost().on_click(move |_, _, cx| cancel.update(cx, |_, cx| cx.emit(CrewEvent::Cancelled))))
                    .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child("Hiring uses none of your plan.")),
            )
            .into_any_element()
    }

    /// One teammate of the crew: who, what, when it runs, where it signs in.
    fn member(&self, t: &Template, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let when: Vec<String> = t.schedules.iter().map(|s| format!("{}: {}", s.label, cron_words(&s.cron))).collect();
        let sites: Vec<&str> = t.logins.iter().map(|l| l.site.as_str()).collect();
        div()
            .flex()
            .items_start()
            .gap(px(14.0))
            .px(px(16.0))
            .py(px(14.0))
            .child(mascot_tile(format!("crew-m-{}", t.id), template_avatar(t), 44.0, &theme))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.0))
                    .child(div().font_weight(FontWeight::MEDIUM).child(t.name.clone()))
                    .child(div().text_size(px(text::SMALL)).text_color(theme.muted).line_clamp(2).child(t.summary.clone()))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_x(px(12.0))
                            .gap_y(px(4.0))
                            .pt(px(2.0))
                            .text_size(px(text::CAPTION))
                            .text_color(theme.muted)
                            .when(!when.is_empty(), |el| {
                                el.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(5.0))
                                        .child(icon(icons::CALENDAR).size(px(13.0)).text_color(theme.muted))
                                        .child(when.join(" · ")),
                                )
                            })
                            .when(!sites.is_empty(), |el| {
                                el.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(5.0))
                                        .child(icon(icons::MONITOR).size(px(13.0)).text_color(theme.muted))
                                        .child(format!("Signs in to {}", crate::crm_model::join_and(&sites))),
                                )
                            }),
                    ),
            )
            .child(chip(Tone::Muted, "Off", cx))
            .into_any_element()
    }

    /// After hiring: the crew and the Set up checklist.
    fn done(&self, ids: &[Uuid], cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let (bots, schedules) = {
            let d = self.data.read(cx);
            let bots: Vec<Bot> = ids.iter().filter_map(|id| d.bot(*id).cloned()).collect();
            (bots, d.schedules.clone())
        };
        let faces = self.faces(bots.iter().map(|b| (b.id.to_string(), avatar_of(b))).collect(), 44.0, &theme);

        // The crew.
        let mut crew = card(cx).flex().flex_col();
        for (i, b) in bots.iter().enumerate() {
            if i > 0 {
                crew = crew.child(divider(cx));
            }
            let open = cx.entity();
            let id = b.id;
            crew = crew.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(16.0))
                    .py(px(10.0))
                    .child(Mascot::new(format!("crew-done-{}", b.id), avatar_of(b), MascotState::Idle, 32.0))
                    .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).truncate().child(b.name.clone()))
                    .child(
                        Button::new(("crew-open", i), "Open")
                            .size(ButtonSize::Small)
                            .icon(icons::ARROW_RIGHT)
                            .on_click(move |_, _, cx| open.update(cx, |_, cx| cx.emit(CrewEvent::Open(id)))),
                    ),
            );
        }

        // Set up: the sites to sign in to, grouped by site.
        let mut rows: Vec<AnyElement> = Vec::new();
        // (site, sign-in page, [(teammate, name, signed in)])
        type Site = (String, String, Vec<(Uuid, String, bool)>);
        let mut sites: Vec<Site> = Vec::new();
        for b in &bots {
            for l in b.setup.iter().flat_map(|s| &s.logins) {
                match sites.iter_mut().find(|(s, _, _)| *s == l.site) {
                    Some((_, _, who)) => who.push((b.id, b.name.clone(), l.done)),
                    None => sites.push((l.site.clone(), l.url.clone(), vec![(b.id, b.name.clone(), l.done)])),
                }
            }
        }
        for (i, (site, url, who)) in sites.iter().enumerate() {
            let left: Vec<&(Uuid, String, bool)> = who.iter().filter(|w| !w.2).collect();
            let done = left.is_empty();
            let names: Vec<&str> = who.iter().map(|w| w.1.as_str()).collect();
            let detail = if done {
                format!("Done for {}", crate::crm_model::join_and(&names))
            } else {
                format!(
                    "For {} · {}",
                    crate::crm_model::join_and(&left.iter().map(|w| w.1.as_str()).collect::<Vec<_>>()),
                    if left.len() > 1 { "each in its own browser" } else { "in its own browser" }
                )
            };
            let action = left.first().map(|(bot, name, _)| {
                let this = cx.entity();
                let (bot, url) = (*bot, url.clone());
                Button::new(("crew-login", i), "Sign in")
                    .size(ButtonSize::Small)
                    .icon(icons::MONITOR)
                    .tooltip(format!("Opens {name} with its browser on the sign-in page and hands you the controls"))
                    .on_click(move |_, _, cx| this.update(cx, |_, cx| cx.emit(CrewEvent::Login { bot, url: url.clone() })))
                    .into_any_element()
            });
            rows.push(check_row(done, format!("Log in to {site}"), detail, action, &theme));
        }

        // Google Workspace for email.
        let email_bots = self.email_bots(cx);
        if !email_bots.is_empty() {
            let conn = self.connectors.as_ref().and_then(|all| all.iter().find(|c| c.preset.as_deref() == Some(EMAIL_CONNECTOR)).cloned());
            let linked_all = conn.as_ref().is_some_and(|c| email_bots.iter().all(|b| self.linked.get(b).is_some_and(|l| l.contains(&c.id))));
            let names: Vec<String> = email_bots.iter().filter_map(|id| bots.iter().find(|b| b.id == *id)).map(|b| b.name.clone()).collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let (detail, action): (String, Option<AnyElement>) = match (&conn, linked_all) {
                (Some(_), true) => (format!("Linked to {}", crate::crm_model::join_and(&names)), None),
                (Some(c), false) => {
                    let this = cx.entity();
                    let id = c.id;
                    (
                        "It's set up in Familiar: link it so they can read replies and send what you approve.".to_owned(),
                        Some(
                            Button::new("crew-link", "Link it")
                                .size(ButtonSize::Small)
                                .disabled(self.busy)
                                .on_click(move |_, _, cx| this.update(cx, |p, cx| p.link_email(id, cx)))
                                .into_any_element(),
                        ),
                    )
                }
                (None, _) => {
                    let docs = self.presets.iter().find(|p| p.id == EMAIL_CONNECTOR).and_then(|p| p.docs_url.clone());
                    (
                        "So they can read replies and send the emails you approve. Add it under Integrations in Familiar's web app (it needs a Google OAuth client), then link it here.".to_owned(),
                        docs.map(|url| {
                            Button::new("crew-docs", "How to")
                                .size(ButtonSize::Small)
                                .icon(icons::LINK)
                                .on_click(move |_, _, cx| cx.open_url(&url))
                                .into_any_element()
                        }),
                    )
                }
            };
            let done = conn.is_some() && linked_all;
            rows.push(check_row(done, "Connect Google Workspace for email".to_owned(), detail, action, &theme));
        }

        // Schedules.
        let wanted: Vec<Uuid> = bots.iter().flat_map(|b| b.setup.iter().flat_map(|s| s.schedules.clone())).collect();
        let off: Vec<Uuid> = wanted.iter().filter(|id| schedules.iter().find(|s| s.id == **id).is_none_or(|s| !s.enabled)).copied().collect();
        if !wanted.is_empty() {
            let done = off.is_empty();
            let this = cx.entity();
            let review = cx.entity();
            let action = div()
                .flex()
                .gap(px(6.0))
                .child(Button::new("crew-review", "Review").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| review.update(cx, |_, cx| cx.emit(CrewEvent::Schedules))))
                .when(!done, |el| {
                    let ids = off.clone();
                    el.child(
                        Button::new("crew-sched-on", if self.busy { "Turning on…" } else { "Turn all on" })
                            .size(ButtonSize::Small)
                            .icon(icons::CALENDAR)
                            .disabled(self.busy)
                            .on_click(move |_, _, cx| this.update(cx, |p, cx| p.schedules_on(ids.clone(), cx))),
                    )
                })
                .into_any_element();
            let n = wanted.len();
            rows.push(check_row(
                done,
                if done {
                    format!("{n} schedules on")
                } else if off.len() == n {
                    format!("Turn on the crew's {n} schedules")
                } else {
                    format!("Turn on {} more schedule{}", off.len(), if off.len() == 1 { "" } else { "s" })
                },
                if done { "They run on their own now.".to_owned() } else { "Off until you turn them on. Turn them on once the sign-ins are done.".to_owned() },
                Some(action),
                &theme,
            ));
        }
        let finished = rows.len();
        let crm = cx.entity();
        div()
            .flex()
            .flex_col()
            .gap(px(24.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(faces)
                    .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).child("Your GTM crew is ready"))
                    .child(div().text_size(px(text::LEAD)).text_color(theme.muted).child(
                        "Each has a chat, a checklist and schedules (off). Finish the set-up below, then they fill your CRM and bring you drafts to approve.",
                    )),
            )
            .child(
                card(cx)
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
                            .child(div().font_weight(FontWeight::MEDIUM).child("Set up the crew"))
                            .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(format!("{finished} steps"))),
                    )
                    .children(rows),
            )
            .child(div().flex().flex_col().gap(px(10.0)).child(div().text_size(px(text::TITLE)).font_weight(FontWeight::SEMIBOLD).child("The crew")).child(crew))
            .child(
                div().flex().child(
                    Button::new("crew-crm", "Go to the CRM").primary().icon(icons::CASE).on_click(move |_, _, cx| crm.update(cx, |_, cx| cx.emit(CrewEvent::Crm))),
                ),
            )
            .into_any_element()
    }
}

/// A Set up checklist row: the tick, what to do, and its action.
fn check_row(done: bool, title: String, detail: String, action: Option<AnyElement>, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .px(px(6.0))
        .py(px(7.0))
        .child(checkbox(done, theme).mt(px(0.0)))
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
                        .child(title),
                )
                .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(detail)),
        )
        .children(action)
        .into_any_element()
}

impl Render for CrewHire {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.hired.clone() {
            Some(ids) => anim::appear("crew-done", div().child(self.done(&ids, cx))).into_any_element(),
            None => self.form(window, cx),
        }
    }
}
