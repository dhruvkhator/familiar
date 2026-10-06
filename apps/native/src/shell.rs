//! The default launch: a sidebar of teammates (mascots + live status) and the main area — Today, Needs you, or a
//! teammate's page — with route crossfades. Everything comes from the live [`AppData`] entity.

use std::collections::HashMap;
use std::time::Duration;

use familiar_client::{Run, RunKind};
use familiar_ui::anim::{self, Crossfade, Expand};
use familiar_ui::appearance::{self, AppearanceMode};
use familiar_ui::components::{
    Button, ButtonSize, HoverCard, Led, LedStatus, SectionHeader, SidebarItem, Skeleton, StatusChip, card, chip,
    divider, empty, group_label,
};
use familiar_ui::edge_fade::edge_faded;
use familiar_ui::icons::{self, icon};
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::notice::{NoticeChipIcon, notice_chip};
use familiar_ui::theme::{RADIUS_CARD, SIDEBAR_WIDTH, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use uuid::Uuid;

use crate::approval::ApprovalCards;
use crate::bot_settings::{BotSettings, CreateEvent};
use crate::chat::{BotPage, TAB_SETTINGS};
use crate::settings::AppSettings;
use crate::data::{AppData, Status, Teammate, ago, excerpt, run_status, tail, until};
use crate::templates::{Picked, TemplatePicker};

#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    Today,
    NeedsYou,
    Teammate(SharedString),
    Settings,
    NewTeammate,
}

pub struct Shell {
    data: Entity<AppData>,
    route: Crossfade<Route>,
    toasts: Entity<ToastStack>,
    scroll: ScrollHandle,
    side_scroll: ScrollHandle,
    expanded: HashMap<Uuid, Expand>,
    approvals: ApprovalCards,
    /// The inbox's large cards (their own answer boxes).
    inbox: ApprovalCards,
    /// Teammate pages, kept so switching back is instant and keeps the scroll.
    pages: HashMap<Uuid, Entity<BotPage>>,
    settings: Option<Entity<AppSettings>>,
    /// The New teammate flow while it is open (fresh each time): the template picker, then the form.
    picker: Option<Entity<TemplatePicker>>,
    new_bot: Option<Entity<BotSettings>>,
    /// Bumped per form so each one plays its entrance.
    form_gen: usize,
    /// Open this teammate's page on its Settings tab (from `--open <name>/settings`).
    open_tab: Option<Uuid>,
}

impl Shell {
    pub fn new(data: Entity<AppData>, open: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        familiar_ui::observe_window(window, cx);
        let mut open = open;
        cx.observe(&data, move |this: &mut Self, data, cx| {
            if open.is_some() && data.read(cx).overview.is_some() {
                let want = open.take().unwrap_or_default();
                this.open(&want, cx);
            }
            // A deleted teammate's page closes.
            if let Route::Teammate(id) = this.route.current().clone() {
                let d = data.read(cx);
                if d.overview.is_some() && !d.bots().iter().any(|b| b.id.to_string() == id.as_ref()) {
                    if let Ok(uuid) = id.parse::<Uuid>() {
                        this.pages.remove(&uuid);
                    }
                    this.navigate(Route::Today, cx);
                }
            }
            cx.notify()
        })
        .detach();
        // "Review…" on a compact approval card opens the inbox.
        let weak = cx.entity().downgrade();
        cx.set_global(crate::approval::InboxOpener(std::rc::Rc::new(move |cx: &mut App| {
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |s, cx| s.navigate(Route::NeedsYou, cx));
            }
        })));
        // Relative times ("5m ago", "in 2h") move on their own: re-render every 30 s like the web's clock.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            data,
            route: Crossfade::new(Route::Today),
            toasts: cx.new(|_| ToastStack::new()),
            scroll: ScrollHandle::new(),
            side_scroll: ScrollHandle::new(),
            expanded: HashMap::new(),
            approvals: ApprovalCards::default(),
            inbox: ApprovalCards::big(),
            pages: HashMap::new(),
            settings: None,
            picker: None,
            new_bot: None,
            form_gen: 0,
            open_tab: None,
        }
    }

    /// Open a page by name: `today`, `needs`, `settings`, `first`, a teammate's name or `bot:<id>`, optionally with
    /// `/settings` for that teammate's Settings tab. Returns whether it matched.
    pub fn open(&mut self, target: &str, cx: &mut Context<Self>) -> bool {
        let want = target.trim().to_lowercase();
        let (who, tab) = match want.strip_suffix("/settings") {
            Some(w) => (w.to_owned(), true),
            None => (want.clone(), false),
        };
        let route = match who.as_str() {
            "today" => Some(Route::Today),
            "needs" => Some(Route::NeedsYou),
            "settings" => Some(Route::Settings),
            "new" => Some(Route::NewTeammate),
            _ => {
                let list = self.data.read(cx).teammates();
                let id = who.strip_prefix("bot:");
                list.iter()
                    .find(|t| match id {
                        Some(id) => t.id.as_ref() == id,
                        None => who == "first" || t.name.to_lowercase() == who,
                    })
                    .map(|t| Route::Teammate(t.id.clone()))
            }
        };
        let Some(route) = route else { return false };
        if let (Route::Teammate(id), true) = (&route, tab) {
            self.open_tab = id.parse().ok();
        }
        self.navigate(route, cx);
        true
    }

    pub fn navigate(&mut self, route: Route, cx: &mut Context<Self>) {
        if route != Route::NewTeammate {
            self.new_bot = None;
            self.picker = None;
        }
        if let Some(s) = self.settings.clone() {
            let shown = route == Route::Settings;
            s.update(cx, |s, cx| s.set_shown(shown, cx));
        }
        if self.route.set(route) {
            self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
            cx.notify();
        }
    }

    fn loading(&self, cx: &App) -> bool {
        let d = self.data.read(cx);
        d.overview.is_none() && d.status == Status::Connecting
    }


    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        let current = self.route.current().clone();
        let dark = theme.is_dark();
        let pending = self.data.read(cx).pending.len();
        let nav = |id: &'static str, label: &'static str, glyph: &'static str, route: Route, badge: usize| {
            let this = this.clone();
            let selected = current == route;
            SidebarItem::new(id, label).icon(glyph).selected(selected).badge(badge).on_click(move |_, _, cx| {
                this.update(cx, |shell, cx| shell.navigate(route.clone(), cx))
            })
        };
        let mut teammates = div().flex().flex_col().gap(px(2.0));
        if self.loading(cx) {
            for i in 0..4 {
                teammates = teammates.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .h(px(48.0))
                        .px(px(8.0))
                        .child(Skeleton::new(30.0).width(30.0).radius(15.0))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .child(Skeleton::new(10.0).width(90.0 - i as f32 * 8.0))
                                .child(Skeleton::new(8.0).width(56.0)),
                        ),
                );
            }
        } else {
            let list = self.data.read(cx).teammates();
            if list.is_empty() {
                teammates = teammates.child(
                    div().px(px(12.0)).text_size(px(text::SMALL)).text_color(theme.muted).child("No teammates yet."),
                );
            }
            for (i, t) in list.into_iter().enumerate() {
                let this = this.clone();
                let route = Route::Teammate(t.id.clone());
                let color = (t.state == MascotState::NeedsYou).then_some(theme.warn);
                teammates = teammates.child(anim::stagger(
                    SharedString::from(format!("side-in-{}", t.id)),
                    i,
                    div().child(
                        SidebarItem::new(SharedString::from(format!("side-{}", t.id)), t.name.clone())
                            .leading(Mascot::new(format!("side-{}", t.id), t.avatar, t.state, 30.0))
                            .sublabel(t.state.label(), color)
                            .selected(current == route)
                            .on_click(move |_, _, cx| this.update(cx, |s, cx| s.navigate(route.clone(), cx))),
                    ),
                ));
            }
        }
        let pc_online = self.data.read(cx).pc_online();
        let this_toggle = cx.entity();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .bg(theme.surface)
            .border_r_1()
            .border_color(theme.line)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(16.0))
                    // Under the app's own title bar the sidebar already starts lower.
                    .pt(px(if crate::titlebar::CUSTOM { 4.0 } else { 16.0 }))
                    .pb(px(12.0))
                    .child(
                        div()
                            .text_size(px(18.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.ink)
                            .child("Familiar"),
                    )
                    .child(
                        Button::icon_only("theme-toggle", if dark { icons::SUN } else { icons::MOON })
                            .tooltip(if dark { "Light appearance" } else { "Dark appearance" })
                            .on_click(move |_, _, cx| {
                                let next = if dark { AppearanceMode::Light } else { AppearanceMode::Dark };
                                appearance::set_mode(next, cx);
                                crate::prefs::update(|p| p.theme = next);
                                this_toggle.update(cx, |_, cx| cx.notify());
                            }),
                    ),
            )
            .child(
                edge_faded(
                    18.0,
                    true,
                    true,
                    div()
                        .id("side-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.side_scroll)
                        .px(px(8.0))
                        .flex()
                        .flex_col()
                        .gap(px(20.0))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(nav("nav-today", "Today", icons::HOME, Route::Today, 0))
                                .child(nav("nav-needs", "Needs you", icons::BELL, Route::NeedsYou, pending)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .pr(px(4.0))
                                        .child(group_label("Teammates", cx))
                                        .child({
                                            let this = cx.entity();
                                            Button::icon_only("new-teammate", icons::PLUS)
                                                .size(ButtonSize::Small)
                                                .tooltip("New teammate")
                                                .on_click(move |_, _, cx| {
                                                    this.update(cx, |s, cx| s.navigate(Route::NewTeammate, cx))
                                                })
                                        }),
                                )
                                .child(teammates),
                        ),
                )
                .fade_overflow_y(&self.side_scroll)
                .into_any_element(),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .p(px(8.0))
                    .border_t_1()
                    .border_color(theme.line)
                    .child(nav("nav-settings", "Settings", icons::SETTINGS, Route::Settings, 0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .px(px(14.0))
                            .h(px(36.0))
                            .text_size(px(text::SMALL))
                            .text_color(theme.ink)
                            .child(Led::new(if pc_online { LedStatus::Online } else { LedStatus::Offline }))
                            .child(if pc_online { "Computer online" } else { "Computer offline" }),
                    ),
            )
    }

    fn page(&mut self, route: &Route, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match route {
            Route::Today => {
                let page = self.today(window, cx).into_any_element();
                self.scrolled(page)
            }
            Route::NeedsYou => {
                let page = self.needs_you(window, cx).into_any_element();
                self.scrolled(page)
            }
            Route::Teammate(id) => self.bot_page(id, window, cx),
            Route::Settings => {
                let (data, toasts) = (self.data.clone(), self.toasts.clone());
                let page = self.settings.get_or_insert_with(|| cx.new(|cx| AppSettings::new(data, toasts, window, cx))).clone();
                self.scrolled(page.into_any_element())
            }
            Route::NewTeammate => {
                if let Some(form) = self.new_bot.clone() {
                    let id = SharedString::from(format!("new-teammate-form-{}", self.form_gen));
                    return self.scrolled(anim::appear(id, div().child(form)).into_any_element());
                }
                let picker = match self.picker.clone() {
                    Some(p) => p,
                    None => {
                        let data = self.data.clone();
                        let p = cx.new(|cx| TemplatePicker::new(data, cx));
                        cx.subscribe_in(&p, window, Self::on_pick).detach();
                        self.picker = Some(p.clone());
                        p
                    }
                };
                self.scrolled(anim::appear("new-teammate", div().child(picker)).into_any_element())
            }
        }
    }

    /// A template (or a blank teammate) was picked: open the form for it.
    fn on_pick(&mut self, _: &Entity<TemplatePicker>, ev: &Picked, window: &mut Window, cx: &mut Context<Self>) {
        let (data, toasts, template) = (self.data.clone(), self.toasts.clone(), ev.0.clone());
        let form = cx.new(|cx| BotSettings::create(data, toasts, template, window, cx));
        cx.subscribe_in(&form, window, Self::on_create).detach();
        self.new_bot = Some(form);
        self.form_gen += 1;
        self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        cx.notify();
    }

    /// The New teammate form finished: show the teammate, and (if asked) send its first message.
    fn on_create(&mut self, _: &Entity<BotSettings>, ev: &CreateEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            CreateEvent::Cancelled => self.navigate(Route::Today, cx),
            CreateEvent::Back => {
                self.new_bot = None;
                self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                cx.notify();
            }
            CreateEvent::Created { bot, post } => {
                let id = bot.id;
                self.data.update(cx, |d, cx| d.add_bot(bot.clone(), cx));
                let (data, toasts) = (self.data.clone(), self.toasts.clone());
                let page = cx.new(|cx| BotPage::new(data, toasts, id, window, cx));
                if let Some(text) = post.clone() {
                    page.update(cx, |p, cx| p.post(text, window, cx));
                }
                self.pages.insert(id, page);
                self.navigate(Route::Teammate(id.to_string().into()), cx);
            }
        }
    }

    fn today(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        if self.loading(cx) {
            return div()
                .flex()
                .flex_col()
                .gap(px(14.0))
                .child(Skeleton::new(36.0).width(240.0))
                .child(Skeleton::new(96.0).radius(RADIUS_CARD))
                .child(Skeleton::new(160.0).radius(RADIUS_CARD));
        }
        let (teammates, pending, runs, schedules, failed, runs_loaded) = {
            let d = self.data.read(cx);
            let failed = match &d.status {
                Status::Failed(e) if d.overview.is_none() => Some(e.clone()),
                _ => None,
            };
            (d.teammates(), d.pending.clone(), d.runs.clone(), d.schedules.clone(), failed, d.runs_loaded)
        };
        let this = cx.entity();
        let active: Vec<&Run> = runs.iter().filter(|r| r.status.is_active()).collect();
        let subtitle = if !pending.is_empty() {
            let n = pending.len();
            format!("{n} thing{} waiting on you.", if n == 1 { "" } else { "s" })
        } else if !active.is_empty() {
            let n = active.len();
            format!("{n} task{} in progress.", if n == 1 { "" } else { "s" })
        } else {
            "Everything is quiet.".to_owned()
        };
        let who = |id: Uuid| teammates.iter().find(|t| t.uuid == id).cloned();

        let mut page = div()
            .flex()
            .flex_col()
            .gap(px(32.0))
            .child(anim::appear(
                "today-head",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_size(px(text::DISPLAY))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.ink)
                            .child(greeting()),
                    )
                    .child(div().text_size(px(text::LEAD)).text_color(theme.muted).child(subtitle)),
            ));

        if let Some(e) = failed {
            return page
                .child(anim::appear(
                    "today-offline",
                    div().child(notice_chip(&theme, true, "Couldn't reach Familiar", e, NoticeChipIcon::Tile)),
                ))
                .child(empty(
                    "Familiar's engine isn't answering",
                    Some("This page retries on its own as soon as the engine is back.".into()),
                    cx,
                ));
        }

        if teammates.is_empty() {
            return page.child(anim::appear(
                "today-first",
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(10.0))
                    .py(px(40.0))
                    .child(Mascot::new("today-first", familiar_ui::mascot::default_avatar("first"), MascotState::Idle, 96.0))
                    .child(div().text_size(px(text::TITLE)).font_weight(FontWeight::SEMIBOLD).child("Meet your first teammate"))
                    .child(
                        div()
                            .text_color(theme.muted)
                            .child("Name it, give it a look and a job, and it starts working beside you."),
                    )
                    .child({
                        let this = cx.entity();
                        Button::new("create-first", "Create a teammate")
                            .primary()
                            .icon(icons::PLUS)
                            .on_click(move |_, _, cx| this.update(cx, |s, cx| s.navigate(Route::NewTeammate, cx)))
                    }),
            ));
        }

        page = page.child(self.strip(&teammates, cx));

        // Needs you: every pending approval / question.
        if !pending.is_empty() {
            let data = self.data.clone();
            let toasts = self.toasts.clone();
            let cards = self.approvals.render(&pending, &data, &toasts, window, cx);
            page = page.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(SectionHeader::new("Needs you").count(pending.len()))
                    .children(cards),
            );
        }

        // Happening now: every active run, with the live text tail.
        if !active.is_empty() {
            let mut list = div().flex().flex_col().gap(px(10.0));
            for (i, r) in active.iter().enumerate() {
                let Some(t) = who(r.bot_id) else { continue };
                let live = self.data.read(cx).live.get(&r.id).map(|b| tail(&b.text, 220)).unwrap_or_default();
                let route = Route::Teammate(t.id.clone());
                let this = this.clone();
                list = list.child(anim::stagger(
                    SharedString::from(format!("now-in-{}", r.id)),
                    i,
                    div().child(
                        HoverCard::new(SharedString::from(format!("now-{}", r.id)))
                            .on_click(move |_, _, cx| this.update(cx, |s, cx| s.navigate(route.clone(), cx)))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(14.0))
                                    .items_start()
                                    .child(Mascot::new(format!("now-{}", r.id), t.avatar, MascotState::Working, 40.0))
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
                                                    .child(div().font_weight(FontWeight::MEDIUM).child(t.name.clone()))
                                                    .child(StatusChip::new(run_status(r.status))),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(text::SMALL))
                                                    .text_color(theme.muted)
                                                    .truncate()
                                                    .child(excerpt(r.prompt.as_deref().unwrap_or(""), 100)),
                                            )
                                            .when(!live.trim().is_empty(), |el| {
                                                el.child(
                                                    div()
                                                        .pt(px(4.0))
                                                        .text_size(px(text::SMALL))
                                                        .text_color(theme.ink)
                                                        .line_clamp(2)
                                                        .child(live.trim().to_owned()),
                                                )
                                            }),
                                    ),
                            ),
                    ),
                ));
            }
            page = page.child(
                div().flex().flex_col().gap(px(12.0)).child(SectionHeader::new("Happening now")).child(list),
            );
        }

        // Recently done: the newest finished runs across bots, expandable.
        let done: Vec<Run> = runs.iter().filter(|r| !r.status.is_active()).take(6).cloned().collect();
        let recent = if !runs_loaded {
            div().flex().flex_col().gap(px(8.0)).children((0..3).map(|_| Skeleton::new(52.0).radius(RADIUS_CARD)))
        } else if done.is_empty() {
            div().child(anim::appear(
                "done-empty",
                empty(
                    "Nothing finished yet",
                    Some("Finished work will appear here. Say hello to a teammate to get started.".into()),
                    cx,
                ),
            ))
        } else {
            let mut list = card(cx).flex().flex_col().overflow_hidden();
            for (i, r) in done.iter().enumerate() {
                let t = who(r.bot_id);
                let exp = self.expanded.entry(r.id).or_insert_with(|| Expand::new(false));
                let openness = exp.openness();
                let this = this.clone();
                let rid = r.id;
                if i > 0 {
                    list = list.child(divider(cx));
                }
                let title = match excerpt(r.prompt.as_deref().unwrap_or(""), 90) {
                    s if s.is_empty() => "(no prompt)".to_owned(),
                    s => s,
                };
                let when = ago(r.finished_at.or(Some(r.created_at)));
                let detail = run_detail(r);
                let open_route = t.as_ref().map(|t| Route::Teammate(t.id.clone()));
                let this_open = cx.entity();
                list = list.child(anim::stagger(
                    SharedString::from(format!("done-in-{}", r.id)),
                    i,
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .id(SharedString::from(format!("done-{}", r.id)))
                                .flex()
                                .items_center()
                                .gap(px(12.0))
                                .px(px(16.0))
                                .py(px(11.0))
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.hover))
                                .on_click(move |_, _, cx| {
                                    this.update(cx, |s, cx| {
                                        if let Some(e) = s.expanded.get_mut(&rid) {
                                            e.toggle();
                                        }
                                        cx.notify();
                                    })
                                })
                                .when_some(t.clone(), |el, t| {
                                    let state =
                                        if r.status == familiar_client::RunStatus::Succeeded { MascotState::Done } else { MascotState::Idle };
                                    el.child(Mascot::new(format!("done-{}", r.id), t.avatar, state, 30.0))
                                })
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().truncate().child(title))
                                        .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(
                                            SharedString::from(format!(
                                                "{} · {when}",
                                                t.as_ref().map(|t| t.name.clone()).unwrap_or_default()
                                            )),
                                        )),
                                )
                                .child(StatusChip::new(run_status(r.status)))
                                .child(
                                    icon(icons::ALT_ARROW_DOWN)
                                        .size(px(14.0))
                                        .text_color(theme.muted)
                                        .opacity(0.5 + 0.5 * openness)
                                        .with_transformation(gpui::Transformation::rotate(gpui::radians(openness * std::f32::consts::PI))),
                                ),
                        )
                        .child(exp.render(
                            SharedString::from(format!("done-detail-{}", r.id)),
                            window,
                            cx,
                            div()
                                .pl(px(58.0))
                                .pr(px(16.0))
                                .pb(px(12.0))
                                .flex()
                                .items_center()
                                .gap(px(12.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_size(px(text::SMALL))
                                        .text_color(if r.error.is_some() { theme.bad } else { theme.muted })
                                        .child(detail),
                                )
                                .when_some(open_route, |el, route| {
                                    el.child(
                                        Button::new(SharedString::from(format!("done-open-{}", r.id)), "Open chat")
                                            .size(ButtonSize::Small)
                                            .icon(icons::ARROW_RIGHT)
                                            .on_click(move |_, _, cx| {
                                                this_open.update(cx, |s, cx| s.navigate(route.clone(), cx))
                                            }),
                                    )
                                }),
                        )),
                ));
            }
            div().child(list)
        };
        page = page.child(div().flex().flex_col().gap(px(12.0)).child(SectionHeader::new("Recently done")).child(recent));

        // Coming up: enabled schedules by next run.
        let mut upcoming: Vec<_> = schedules.iter().filter(|s| s.enabled && s.next_run_at.is_some()).collect();
        upcoming.sort_by_key(|s| s.next_run_at);
        if !upcoming.is_empty() {
            let mut coming = card(cx).flex().flex_col();
            for (i, s) in upcoming.into_iter().take(5).enumerate() {
                let t = who(s.bot_id);
                if i > 0 {
                    coming = coming.child(divider(cx));
                }
                let tone = if s.kind == RunKind::Proactive { Tone::Accent } else { Tone::Muted };
                coming = coming.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.0))
                        .px(px(16.0))
                        .py(px(11.0))
                        .when_some(t.clone(), |el, t| {
                            el.child(Mascot::new(format!("up-{}", s.id), t.avatar, MascotState::Idle, 30.0))
                        })
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(div().truncate().child(excerpt(&s.prompt, 80)))
                                .child(
                                    div()
                                        .text_size(px(text::CAPTION))
                                        .text_color(theme.muted)
                                        .child(t.map(|t| t.name).unwrap_or_default()),
                                ),
                        )
                        .child(chip(tone, until(s.next_run_at), cx)),
                );
            }
            page = page.child(div().flex().flex_col().gap(px(12.0)).child(SectionHeader::new("Coming up")).child(coming));
        }
        page
    }

    fn strip(&self, teammates: &[Teammate], cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        let mut strip = div().flex().gap(px(12.0)).flex_wrap();
        for (i, t) in teammates.iter().enumerate() {
            let route = Route::Teammate(t.id.clone());
            let this = this.clone();
            let status = if t.state == MascotState::NeedsYou { theme.warn } else { theme.muted };
            strip = strip.child(anim::stagger(
                SharedString::from(format!("strip-in-{}", t.id)),
                i,
                div().child(
                    HoverCard::new(SharedString::from(format!("strip-{}", t.id)))
                        .flat()
                        .padding(10.0)
                        .on_click(move |_, _, cx| this.update(cx, |s, cx| s.navigate(route.clone(), cx)))
                        .child(
                            div()
                                .w(px(84.0))
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap(px(6.0))
                                .child(Mascot::new(format!("strip-{}", t.id), t.avatar, t.state, 64.0))
                                .child(
                                    div()
                                        .text_size(px(text::SMALL))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.ink)
                                        .truncate()
                                        .child(t.name.clone()),
                                )
                                .child(div().text_size(px(text::CAPTION)).text_color(status).child(t.state.label())),
                        ),
                ),
            ));
        }
        strip.child(anim::stagger(
            "strip-in-new",
            teammates.len(),
            div().child(
                HoverCard::new("strip-new").flat().padding(10.0).on_click(move |_, _, cx| {
                    this.update(cx, |s, cx| s.navigate(Route::NewTeammate, cx))
                }).child(
                    div()
                        .w(px(84.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .size(px(64.0))
                                .rounded_full()
                                .border_2()
                                .border_dashed()
                                .border_color(theme.line)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon(icons::PLUS).size(px(22.0)).text_color(theme.muted)),
                        )
                        .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child("New")),
                ),
            ),
        ))
    }

    fn needs_you(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        // Oldest first: the one that has waited longest is at the top.
        let mut pending = self.data.read(cx).pending.clone();
        pending.sort_by_key(|a| a.created_at);
        let n = pending.len();
        let head = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).text_color(theme.ink).child("Needs you"))
                    .when(n > 0, |el| el.child(chip(Tone::Warn, format!("{n} waiting"), cx))),
            )
            .child(
                div()
                    .text_size(px(text::LEAD))
                    .text_color(theme.muted)
                    .child("Teammates pause here until you decide. Requests expire after 30 minutes."),
            );
        let mut page = div().flex().flex_col().gap(px(24.0)).child(anim::appear("needs-head", head));
        if pending.is_empty() {
            page = page.child(anim::appear(
                "needs-empty",
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(10.0))
                    .py(px(48.0))
                    .child(Mascot::new("needs-empty", familiar_ui::mascot::default_avatar("needs"), MascotState::Done, 88.0))
                    .child(div().text_size(px(text::TITLE)).font_weight(FontWeight::SEMIBOLD).child("You're all caught up"))
                    .child(
                        div()
                            .text_color(theme.muted)
                            .child("When a teammate wants to run something risky or has a question, it shows up here."),
                    ),
            ));
        } else {
            let data = self.data.clone();
            let toasts = self.toasts.clone();
            let cards = self.inbox.render(&pending, &data, &toasts, window, cx);
            page = page.child(div().flex().flex_col().gap(px(14.0)).children(cards));
        }
        page
    }

    fn bot_page(&mut self, id: &SharedString, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Ok(bot) = id.parse::<Uuid>() else {
            return empty("Teammate not found", None, cx).into_any_element();
        };
        let data = self.data.clone();
        let toasts = self.toasts.clone();
        let page = self.pages.entry(bot).or_insert_with(|| cx.new(|cx| BotPage::new(data, toasts, bot, window, cx))).clone();
        if self.open_tab == Some(bot) {
            self.open_tab = None;
            page.update(cx, |p, cx| p.set_tab(TAB_SETTINGS, window, cx));
        }
        page.into_any_element()
    }

    /// A scrolling, centred column for the overview pages.
    fn scrolled(&self, page: AnyElement) -> AnyElement {
        edge_faded(
            24.0,
            true,
            true,
            div()
                .id("main-scroll")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .child(
                    div()
                        .w_full()
                        .flex()
                        .justify_center()
                        .px(px(40.0))
                        .py(px(36.0))
                        .child(div().w_full().max_w(px(760.0)).child(page)),
                ),
        )
        .fade_overflow_y(&self.scroll)
        .into_any_element()
    }
}

pub fn state_tone(state: MascotState) -> Tone {
    match state {
        MascotState::Working => Tone::Accent,
        MascotState::NeedsYou => Tone::Warn,
        MascotState::Done => Tone::Ok,
        _ => Tone::Muted,
    }
}

/// The expanded line of a finished run: its error, or how it went.
fn run_detail(r: &Run) -> String {
    if let Some(e) = r.error.as_deref().filter(|e| !e.trim().is_empty()) {
        return excerpt(e, 240);
    }
    let kind = match r.kind {
        RunKind::Unknown => "run".to_owned(),
        k => format!("{} run", k.as_str()),
    };
    let mut parts = vec![kind, format!("finished {}", ago(r.finished_at))];
    if let (Some(s), Some(f)) = (r.started_at, r.finished_at) {
        let secs = (f - s).num_seconds().max(0);
        parts.push(if secs < 60 { format!("took {secs}s") } else { format!("took {}m", secs / 60) });
    }
    if let Some(c) = r.cost_usd.filter(|c| *c > 0.0) {
        // Claude Code's notional list-price figure; runs use the owner's plan, nothing is billed.
        parts.push(format!("≈ ${c:.2} at API prices · included in your plan"));
    }
    parts.join(" · ")
}

/// The web's `greeting()`, by local hour.
fn greeting() -> &'static str {
    let h = local_hour();
    if h < 5 {
        "Still up?"
    } else if h < 12 {
        "Good morning"
    } else if h < 18 {
        "Good afternoon"
    } else {
        "Good evening"
    }
}

fn local_hour() -> u32 {
    use chrono::Timelike as _;
    chrono::Local::now().hour()
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        anim::frame(window);
        let theme = Theme::of(cx).clone();
        let sidebar = self.sidebar(cx).into_any_element();
        let page = self.render_route(window, cx);
        div()
            .size_full()
            .flex()
            .bg(theme.bg)
            .text_color(theme.ink)
            .text_size(px(text::BODY))
            .font_family(theme.font_sans.clone())
            .child(sidebar)
            .child(
                div().flex_1().min_w_0().h_full().child(page),
            )
            .child(self.toasts.clone())
    }
}

impl Shell {
    /// The route crossfade. The crossfade state is moved out while it renders so the page builders can borrow the
    /// shell mutably alongside the view context.
    fn render_route(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut route = std::mem::replace(&mut self.route, Crossfade::new(Route::Today));
        let reduced = familiar_ui::motion::reduced_motion(cx);
        let element = route.render("route", reduced, window, cx, |r, window, cx| self.page(r, window, cx));
        self.route = route;
        element
    }
}

