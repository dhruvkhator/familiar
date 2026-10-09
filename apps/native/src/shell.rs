//! The default launch: a sidebar of teammates (mascots + live status) and the main area — Today, Needs you, or a
//! teammate's page — with route crossfades. Everything comes from the live [`AppData`] entity.
//!
//! Redraws stay local: the [`Sidebar`] and a teammate's page are their own views, drawn cached (each redraws when it
//! is notified, not whenever the shell does), and the shell redraws for the data it shows ([`DataEvent::Updated`];
//! live text only on Today, which shows it).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use familiar_client::{Run, RunKind};
use familiar_ui::anim::{self, Crossfade, Expand};
use familiar_ui::components::{
    Button, ButtonSize, HoverCard, SectionHeader, Skeleton, StatusChip, card, chip, divider, empty,
};
use familiar_ui::edge_fade::edge_faded;
use familiar_ui::icons::{self, icon};
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::notice::{NoticeChipIcon, notice_chip};
use familiar_ui::theme::{RADIUS_CARD, SIDEBAR_WIDTH, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, StyleRefinement,
    Styled as _, Task, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::approval::ApprovalCards;
use crate::crm::{CrmEvent, CrmPage, Tab};
use crate::crm_crew::{CrewEvent, CrewHire};
use crate::sidebar::{self, Sidebar};
use crate::bot_settings::{BotSettings, CreateEvent};
use crate::chat::{BotPage, TAB_FILES, TAB_SETTINGS};
use crate::integrations::IntegrationsPage;
use crate::rules::RulesPage;
use crate::schedules::SchedulesPage;
use crate::settings::AppSettings;
use crate::data::{AppData, DataEvent, Status, Teammate, ago, excerpt, run_status, tail, until};
use crate::templates::{Picked, PickedBundle, TemplatePicker};

/// Resting on a teammate this long (sidebar row, Today's cards) fetches their chat before the click.
const PREFETCH_AFTER: Duration = Duration::from_millis(120);
/// A teammate prefetched this recently is not fetched again on the next rest (the pointer wandering over the list).
const PREFETCH_AGAIN_AFTER: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    Today,
    NeedsYou,
    Schedules,
    Crm,
    Teammate(SharedString),
    Integrations,
    Rules,
    Settings,
    NewTeammate,
}

/// Opens a page by name from anywhere (set by the shell): a teammate's settings linking to Integrations, the crew's
/// checklist opening a connector's install form. Takes [`Shell::open`]'s names.
pub struct PageOpener(pub std::rc::Rc<dyn Fn(&str, &mut App)>);

impl gpui::Global for PageOpener {}

/// Open `target` (see [`Shell::open`]) through the shell, if there is one.
pub fn open_page(target: &str, cx: &mut App) {
    if let Some(open) = cx.try_global::<PageOpener>().map(|o| o.0.clone()) {
        open(target, cx);
    }
}

pub struct Shell {
    data: Entity<AppData>,
    route: Crossfade<Route>,
    toasts: Entity<ToastStack>,
    sidebar: Entity<Sidebar>,
    scroll: ScrollHandle,
    expanded: HashMap<Uuid, Expand>,
    approvals: ApprovalCards,
    /// The inbox's large cards (their own answer boxes).
    inbox: ApprovalCards,
    /// Teammate pages, kept so switching back is instant and keeps the scroll.
    pages: HashMap<Uuid, Entity<BotPage>>,
    settings: Option<Entity<AppSettings>>,
    schedules: Option<Entity<SchedulesPage>>,
    integrations: Option<Entity<IntegrationsPage>>,
    /// `--open integrations/<preset>` before the page exists: open that preset's install form.
    install: Option<String>,
    rules: Option<Entity<RulesPage>>,
    crm: Option<Entity<CrmPage>>,
    /// `--open crm/<tab>` before the CRM page exists: (tab, the deals as a list).
    crm_tab: Option<(Tab, bool)>,
    /// "Hire the GTM crew" while it is open (in the New teammate route); `crew_from_crm`: Back returns to the CRM.
    crew: Option<Entity<CrewHire>>,
    crew_from_crm: bool,
    /// Open the crew form when the New teammate route next draws (it needs the window).
    want_crew: bool,
    /// The New teammate flow while it is open (fresh each time): the template picker, then the form.
    picker: Option<Entity<TemplatePicker>>,
    new_bot: Option<Entity<BotSettings>>,
    /// Bumped per form so each one plays its entrance.
    form_gen: usize,
    /// Open this teammate's page on this tab (from `--open <name>/settings`).
    open_tab: Option<(Uuid, usize)>,
    /// No route crossfade is running: pages may be drawn cached.
    settled: bool,
    /// The teammate under the pointer, and the wait before their chat is fetched (dropping it cancels the wait).
    hovering: Option<(Uuid, Task<()>)>,
    /// When each teammate's chat was last prefetched.
    prefetched: HashMap<Uuid, Instant>,
}

impl Shell {
    pub fn new(data: Entity<AppData>, open: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        familiar_ui::observe_window(window, cx);
        let mut open = open;
        cx.subscribe(&data, move |this: &mut Self, data, ev: &DataEvent, cx| match ev {
            DataEvent::Updated(_) => {
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
            }
            // Live text: only Today shows it (the tail of each active run), the teammate's page redraws itself.
            DataEvent::Delta(run) => {
                if *this.route.current() == Route::Today && data.read(cx).runs.iter().any(|r| r.id == *run && r.status.is_active()) {
                    cx.notify()
                }
            }
            DataEvent::Changed(_) => {}
        })
        .detach();
        // "Review…" on a compact approval card opens the inbox.
        let weak = cx.entity().downgrade();
        let weak_shell = weak.clone();
        cx.set_global(crate::approval::InboxOpener(std::rc::Rc::new(move |cx: &mut App| {
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |s, cx| s.navigate(Route::NeedsYou, cx));
            }
        })));
        let weak_open = weak_shell.clone();
        cx.set_global(PageOpener(std::rc::Rc::new(move |target: &str, cx: &mut App| {
            let target = target.to_owned();
            // Deferred: the caller is usually a view the shell is drawing or updating.
            let weak = weak_open.clone();
            cx.defer(move |cx| {
                if let Some(shell) = weak.upgrade() {
                    shell.update(cx, |s, cx| {
                        s.open(&target, cx);
                    });
                }
            });
        })));
        // Relative times ("5m ago", "in 2h") move on their own: re-render every 30 s like the web's clock (the sidebar
        // and the pages too: a teammate's "Done" fades to "Idle", the computer goes offline).
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                let ticked = this.update(cx, |s, cx| {
                    cx.notify();
                    s.sidebar.update(cx, |_, cx| cx.notify());
                    for page in s.pages.values() {
                        page.update(cx, |_, cx| cx.notify());
                    }
                    // The CRM's "updated 5m ago" and due dates, while it is on screen.
                    if *s.route.current() == Route::Crm
                        && let Some(p) = s.crm.as_ref()
                    {
                        p.update(cx, |_, cx| cx.notify());
                    }
                });
                if ticked.is_err() {
                    break;
                }
            }
        })
        .detach();
        let sidebar = cx.new(|cx| Sidebar::new(data.clone(), weak_shell, cx));
        Self {
            data,
            route: Crossfade::new(Route::Today),
            toasts: cx.new(|_| ToastStack::new()),
            sidebar,
            scroll: ScrollHandle::new(),
            expanded: HashMap::new(),
            approvals: ApprovalCards::default(),
            inbox: ApprovalCards::big(),
            pages: HashMap::new(),
            settings: None,
            schedules: None,
            integrations: None,
            install: None,
            rules: None,
            crm: None,
            crm_tab: None,
            crew: None,
            crew_from_crm: false,
            want_crew: false,
            picker: None,
            new_bot: None,
            form_gen: 0,
            open_tab: None,
            settled: true,
            hovering: None,
            prefetched: HashMap::new(),
        }
    }

    /// Open a page by name: `today`, `needs`, `schedules`, `crm` (or `crm/contacts`, `crm/pipeline`, `crm/deals`,
    /// `crm/activity`), `crew`, `integrations` (or `integrations/<preset>`: that connector's install form), `rules`,
    /// `settings`, `first`, a teammate's name or `bot:<id>`, optionally with `/settings` or `/files` for that tab of
    /// its page. Returns whether it matched.
    pub fn open(&mut self, target: &str, cx: &mut Context<Self>) -> bool {
        let want = target.trim().to_lowercase();
        let (who, tab) = match (want.strip_suffix("/settings"), want.strip_suffix("/files")) {
            (Some(w), _) => (w.to_owned(), Some(TAB_SETTINGS)),
            (_, Some(w)) => (w.to_owned(), Some(TAB_FILES)),
            _ => (want.clone(), None),
        };
        if let Some(rest) = who.strip_prefix("integrations") {
            let preset = rest.trim_start_matches('/');
            if !preset.is_empty() {
                match self.integrations.clone() {
                    Some(p) => {
                        let preset = preset.to_owned();
                        p.update(cx, |p, cx| p.install_when_ready(preset, cx));
                    }
                    None => self.install = Some(preset.to_owned()),
                }
            }
            self.navigate(Route::Integrations, cx);
            return true;
        }
        if let Some(tab) = who.strip_prefix("crm") {
            let want = match tab.trim_start_matches('/') {
                "" | "companies" => (Tab::Companies, false),
                "contacts" => (Tab::Contacts, false),
                "pipeline" | "board" => (Tab::Pipeline, false),
                "deals" => (Tab::Pipeline, true),
                "activity" => (Tab::Activity, false),
                _ => return false,
            };
            match self.crm.clone() {
                Some(p) => p.update(cx, |p, cx| {
                    p.set_tab(want.0, cx);
                    p.set_deal_list(want.1, cx);
                }),
                None => self.crm_tab = Some(want),
            }
            self.navigate(Route::Crm, cx);
            return true;
        }
        if who == "crew" {
            self.hire_crew(true, cx);
            return true;
        }
        let route = match who.as_str() {
            "today" => Some(Route::Today),
            "needs" => Some(Route::NeedsYou),
            "schedules" => Some(Route::Schedules),
            "rules" => Some(Route::Rules),
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
        if let (Route::Teammate(id), Some(tab)) = (&route, tab) {
            self.open_tab = id.parse().ok().map(|bot| (bot, tab));
        }
        self.navigate(route, cx);
        true
    }

    pub fn navigate(&mut self, route: Route, cx: &mut Context<Self>) {
        if matches!(route, Route::Teammate(_)) && &route != self.route.current() {
            crate::perf::begin("switch");
        }
        if route != Route::NewTeammate {
            self.new_bot = None;
            self.picker = None;
            self.crew = None;
            self.want_crew = false;
        }
        if let Some(p) = self.crm.clone() {
            let shown = route == Route::Crm;
            p.update(cx, |p, cx| p.set_shown(shown, cx));
        }
        if let Some(s) = self.settings.clone() {
            let shown = route == Route::Settings;
            s.update(cx, |s, cx| s.set_shown(shown, cx));
        }
        if let Some(p) = self.integrations.clone() {
            let shown = route == Route::Integrations;
            p.update(cx, |p, cx| p.set_shown(shown, cx));
        }
        if let Some(p) = self.rules.clone() {
            let shown = route == Route::Rules;
            p.update(cx, |p, cx| p.set_shown(shown, cx));
        }
        // A teammate's page that leaves the screen wipes what it showed only once (a trigger's new address).
        if let Route::Teammate(id) = self.route.current().clone()
            && route != Route::Teammate(id.clone())
            && let Some(page) = id.parse::<Uuid>().ok().and_then(|b| self.pages.get(&b).cloned())
        {
            page.update(cx, |p, cx| p.set_shown(false, cx));
        }
        let shown = route.clone();
        if self.route.set(route) {
            self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
            self.sidebar.update(cx, |s, cx| s.set_route(shown, cx));
            cx.notify();
        }
    }

    /// "Hire the GTM crew" (`from_crm`: Back returns to the CRM rather than the template picker).
    pub fn hire_crew(&mut self, from_crm: bool, cx: &mut Context<Self>) {
        self.navigate(Route::NewTeammate, cx);
        self.new_bot = None;
        self.crew = None;
        self.crew_from_crm = from_crm;
        self.want_crew = true;
        self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        cx.notify();
    }

    /// The CRM page, if it was opened.
    pub fn crm_page(&self) -> Option<Entity<CrmPage>> {
        self.crm.clone()
    }

    /// Integrations, if it was opened.
    pub fn integrations_page(&self) -> Option<Entity<IntegrationsPage>> {
        self.integrations.clone()
    }

    /// Rules, if it was opened.
    pub fn rules_page(&self) -> Option<Entity<RulesPage>> {
        self.rules.clone()
    }

    /// App Settings, if it was opened.
    pub fn settings_page(&self) -> Option<Entity<AppSettings>> {
        self.settings.clone()
    }

    /// Scroll an overview page (Settings, Today…) to `y` px from its top.
    pub fn scroll_page_to(&self, y: f32) {
        self.scroll.set_offset(gpui::point(px(0.0), px(-y)));
    }

    /// The crew form, while it is open.
    pub fn crew_form(&self) -> Option<Entity<CrewHire>> {
        self.crew.clone()
    }

    /// A teammate's page, if it was opened.
    pub fn page_of(&self, bot: Uuid) -> Option<Entity<BotPage>> {
        self.pages.get(&bot).cloned()
    }

    /// The pointer rests on a teammate (a sidebar row, Today's cards): unless their page is already open, fetch their
    /// chat into the cache after [`PREFETCH_AFTER`], so the click shows it at once (and refreshes it as usual).
    pub fn hover_teammate(&mut self, bot: Uuid, cx: &mut Context<Self>) {
        if self.pages.contains_key(&bot)
            || self.hovering.as_ref().is_some_and(|(b, _)| *b == bot)
            || self.prefetched.get(&bot).is_some_and(|at| at.elapsed() < PREFETCH_AGAIN_AFTER)
        {
            return;
        }
        let client = self.data.read(cx).client.clone();
        let wait = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PREFETCH_AFTER).await;
            let _ = this.update(cx, |this, _| this.prefetched.insert(bot, Instant::now()));
            // Once started, the fetch finishes even if the pointer moves on: it only warms the cache.
            Tokio::spawn(cx, crate::chat::prefetch(client, bot)).detach();
        });
        self.hovering = Some((bot, wait));
    }

    /// The pointer left teammate `bot`: a fetch still waiting is cancelled.
    pub fn unhover_teammate(&mut self, bot: Uuid, _cx: &mut Context<Self>) {
        if self.hovering.as_ref().is_some_and(|(b, _)| *b == bot) {
            self.hovering = None;
        }
    }

    fn loading(&self, cx: &App) -> bool {
        sidebar::loading(&self.data, cx)
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
            Route::Schedules => {
                let (data, toasts) = (self.data.clone(), self.toasts.clone());
                let page = self.schedules.get_or_insert_with(|| cx.new(|cx| SchedulesPage::new(data, toasts, cx))).clone();
                self.scrolled(page.into_any_element())
            }
            Route::Crm => {
                let page = match self.crm.clone() {
                    Some(p) => p,
                    None => {
                        let (data, toasts) = (self.data.clone(), self.toasts.clone());
                        let p = cx.new(|cx| CrmPage::new(data, toasts, window, cx));
                        cx.subscribe_in(&p, window, Self::on_crm).detach();
                        if let Some((tab, list)) = self.crm_tab.take() {
                            p.update(cx, |p, cx| {
                                p.set_tab(tab, cx);
                                p.set_deal_list(list, cx);
                            });
                        }
                        self.crm = Some(p.clone());
                        p
                    }
                };
                // Full width with its own scrolling (tables, the board); drawn cached like a teammate's page.
                if self.settled {
                    page.cached(StyleRefinement::default().size_full()).into_any_element()
                } else {
                    page.into_any_element()
                }
            }
            Route::Teammate(id) => self.bot_page(id, window, cx),
            Route::Integrations => {
                let page = match self.integrations.clone() {
                    Some(p) => p,
                    None => {
                        let (data, toasts) = (self.data.clone(), self.toasts.clone());
                        let p = cx.new(|cx| IntegrationsPage::new(data, toasts, cx));
                        if let Some(preset) = self.install.take() {
                            p.update(cx, |p, cx| p.install_when_ready(preset, cx));
                        }
                        self.integrations = Some(p.clone());
                        p
                    }
                };
                // Its own scrolling (the install form is a dialog over the page); drawn cached like the CRM.
                if self.settled {
                    page.cached(StyleRefinement::default().size_full()).into_any_element()
                } else {
                    page.into_any_element()
                }
            }
            Route::Rules => {
                let (data, toasts) = (self.data.clone(), self.toasts.clone());
                let page = self.rules.get_or_insert_with(|| cx.new(|cx| RulesPage::new(data, toasts, cx))).clone();
                self.scrolled(page.into_any_element())
            }
            Route::Settings => {
                let (data, toasts) = (self.data.clone(), self.toasts.clone());
                let page = self.settings.get_or_insert_with(|| cx.new(|cx| AppSettings::new(data, toasts, window, cx))).clone();
                self.scrolled(page.into_any_element())
            }
            Route::NewTeammate => {
                if std::mem::take(&mut self.want_crew) {
                    let (data, toasts) = (self.data.clone(), self.toasts.clone());
                    let crew = cx.new(|cx| CrewHire::new(data, toasts, None, window, cx));
                    cx.subscribe_in(&crew, window, Self::on_crew).detach();
                    self.crew = Some(crew);
                    self.form_gen += 1;
                }
                if let Some(crew) = self.crew.clone() {
                    let id = SharedString::from(format!("crew-form-{}", self.form_gen));
                    return self.scrolled(anim::appear(id, div().child(crew)).into_any_element());
                }
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
                        cx.subscribe_in(&p, window, Self::on_pick_bundle).detach();
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

    /// The GTM crew was picked: its own form (hires every teammate of the bundle at once).
    fn on_pick_bundle(&mut self, _: &Entity<TemplatePicker>, ev: &PickedBundle, window: &mut Window, cx: &mut Context<Self>) {
        let (data, toasts, bundle) = (self.data.clone(), self.toasts.clone(), ev.0.clone());
        let crew = cx.new(|cx| CrewHire::new(data, toasts, Some(bundle), window, cx));
        cx.subscribe_in(&crew, window, Self::on_crew).detach();
        self.crew = Some(crew);
        self.crew_from_crm = false;
        self.form_gen += 1;
        self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        cx.notify();
    }

    fn on_crew(&mut self, _: &Entity<CrewHire>, ev: &CrewEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            CrewEvent::Cancelled => self.navigate(if self.crew_from_crm { Route::Crm } else { Route::Today }, cx),
            CrewEvent::Back if self.crew_from_crm => self.navigate(Route::Crm, cx),
            CrewEvent::Back => {
                self.crew = None;
                self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                cx.notify();
            }
            CrewEvent::Hired(bots) => {
                for b in bots {
                    self.data.update(cx, |d, cx| d.add_bot(b.clone(), cx));
                }
                self.data.update(cx, |d, cx| d.reload_schedules(cx));
                self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
            }
            CrewEvent::Open(bot) => self.navigate(Route::Teammate(bot.to_string().into()), cx),
            CrewEvent::Login { bot, url } => {
                let (data, toasts, id) = (self.data.clone(), self.toasts.clone(), *bot);
                let page = self.pages.entry(id).or_insert_with(|| cx.new(|cx| BotPage::new(data, toasts, id, window, cx))).clone();
                self.navigate(Route::Teammate(id.to_string().into()), cx);
                page.update(cx, |p, cx| p.open_login(url.clone(), window, cx));
            }
            CrewEvent::Schedules => self.navigate(Route::Schedules, cx),
            CrewEvent::Crm => self.navigate(Route::Crm, cx),
        }
    }

    fn on_crm(&mut self, _: &Entity<CrmPage>, ev: &CrmEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            CrmEvent::HireCrew => self.hire_crew(true, cx),
            CrmEvent::OpenNeeds => self.navigate(Route::NeedsYou, cx),
        }
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
        let weak = this.downgrade();
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
                    sidebar::hover_target(SharedString::from(format!("now-hover-{}", r.id)), t.uuid, &weak).child(
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
                                .child(div().truncate().child(crate::schedules::label_of(s)))
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
        let weak = this.downgrade();
        let mut strip = div().flex().gap(px(12.0)).flex_wrap();
        for (i, t) in teammates.iter().enumerate() {
            let route = Route::Teammate(t.id.clone());
            let this = this.clone();
            let status = if t.state == MascotState::NeedsYou { theme.warn } else { theme.muted };
            strip = strip.child(anim::stagger(
                SharedString::from(format!("strip-in-{}", t.id)),
                i,
                sidebar::hover_target(SharedString::from(format!("strip-hover-{}", t.id)), t.uuid, &weak).child(
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
        // Drafts first (they are reviewed as a batch), then the rest; oldest first within each.
        let mut pending = self.data.read(cx).pending.clone();
        pending.sort_by_key(|a| (!a.is_draft(), a.created_at));
        let n = pending.len();
        let drafts = pending.iter().filter(|a| a.is_draft()).count();
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
                    .when(n > 0, |el| el.child(chip(Tone::Warn, format!("{n} waiting"), cx)))
                    .when(drafts > 0, |el| {
                        el.child(chip(Tone::Accent, format!("{drafts} draft{}", if drafts == 1 { "" } else { "s" }), cx))
                    }),
            )
            .child(
                div()
                    .text_size(px(text::LEAD))
                    .text_color(theme.muted)
                    .child(
                        "Drafts wait here up to 7 days without holding their teammate up: it hears your decision as \
                         soon as you make it. Other requests pause the teammate and expire after 30 minutes. Approving \
                         a draft moves you to the next one.",
                    ),
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
                    .child(div().text_color(theme.muted).child(
                        "When a teammate drafts a post or a reply, wants to run something risky or has a question, it shows up here.",
                    )),
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
        if let Some((_, tab)) = self.open_tab.filter(|(b, _)| *b == bot) {
            self.open_tab = None;
            page.update(cx, |p, cx| p.set_tab(tab, window, cx));
        }
        // Cached: the page redraws when it is notified (its data, its live text, its own animations), not with the
        // shell. Mid-crossfade it is drawn afresh, since a cached view keeps the opacity it was painted with.
        if self.settled {
            page.cached(StyleRefinement::default().size_full()).into_any_element()
        } else {
            page.into_any_element()
        }
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
        crate::perf::count("Shell");
        if !self.loading(cx) {
            crate::perf::milestone_painted("shell_usable");
        }
        let theme = Theme::of(cx).clone();
        let sidebar = self
            .sidebar
            .clone()
            .cached(StyleRefinement::default().flex_none().w(px(SIDEBAR_WIDTH)).h_full())
            .into_any_element();
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
        self.settled = route.settled(reduced);
        let element = route.render("route", reduced, window, cx, |r, window, cx| self.page(r, window, cx));
        self.route = route;
        element
    }
}

