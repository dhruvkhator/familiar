//! The CRM page: Companies · Contacts · Pipeline · Activity, over the built-in CRM (`/api/crm/...`).
//!
//! Tables are virtualised (`uniform_list`, fixed-height rows) and loaded in the background page by page (200 rows a
//! request, every row up to [`MAX_ROWS`]), so counts are exact and scrolling never waits. Search, the tag /
//! do-not-contact / stage filters and the sort run on the server; the board's cards and the activity list filter
//! what they already hold. A record opens in a side panel ([`crate::crm_record`]). Import is a dry run first (what it
//! would create, update and skip, and each problem with its row), then the real thing; export saves a CSV per tab.
//!
//! Live: the page keeps its own data and listens for the stream's `crm_*` notices (coalesced per table): a notice
//! refreshes only the lists it touches, one refresh at a time per list (a notice during a refresh queues one more).
//! While the page is hidden notices only mark it stale; it catches up when shown.

use std::collections::HashMap;
use std::future::Future;
use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::Utc;
use familiar_client::{
    ApiError, CrmActivity, CrmActivityParams, CrmCompany, CrmContact, CrmDeal, CrmImportResult, CrmListParams, DealPatch,
    DealStage, PipelineDeal, PipelineStage,
};
use familiar_ui::anim;
use familiar_ui::components::{Button, ButtonSize, Segmented, Skeleton, card, chip};
use familiar_ui::icons::{self, icon};
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::theme::{RADIUS_CARD, RADIUS_CHIP, RADIUS_CONTROL, RADIUS_DIALOG, SIDEBAR_WIDTH, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use futures::StreamExt as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, FontWeight, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, PathPromptOptions, Render, ScrollHandle, ScrollStrategy,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, UniformListScrollHandle, Window, div,
    prelude::FluentBuilder as _, px, uniform_list,
};
use gpui_base::input::{InputEvent, InputState};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::crm_model::{self as model, STAGES};
use crate::crm_record::{PanelEvent, Rec, RecordPanel};
use crate::data::{AppData, DataEvent, Part, ago, avatar_of, excerpt};
use crate::menu::{self, MenuItem};
use crate::text_input;

/// Rows per request (the API's maximum).
const PAGE: u32 = 200;
/// A list stops loading here (the API pages on; this is what one window holds).
pub const MAX_ROWS: usize = 10_000;
/// Activity is read newest first, a page at a time as you scroll.
const ACT_PAGE: u32 = 100;
const ROW_H: f32 = 52.0;
/// Cards per board column before "Show more".
const BOARD_PAGE: usize = 25;
/// A refresh asked for during a refresh runs this long after it.
const REFRESH_GAP: Duration = Duration::from_millis(700);
/// How long "Deleted … · Undo" stays.
const UNDO_FOR: Duration = Duration::from_secs(12);

/// What the page asks the shell for.
pub enum CrmEvent {
    HireCrew,
    OpenNeeds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Companies,
    Contacts,
    Pipeline,
    Activity,
}

const TABS: [(Tab, &str, &str); 4] = [
    (Tab::Companies, "Companies", icons::BUILDINGS),
    (Tab::Contacts, "Contacts", icons::USER),
    (Tab::Pipeline, "Pipeline", icons::CASE),
    (Tab::Activity, "Activity", icons::LIST),
];

impl Tab {
    /// The export / import kind of the tab (Activity has none).
    fn kind(self) -> Option<&'static str> {
        match self {
            Tab::Companies => Some("companies"),
            Tab::Contacts => Some("contacts"),
            Tab::Pipeline => Some("deals"),
            Tab::Activity => None,
        }
    }
}

/// The lists loaded page by page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Companies,
    Contacts,
    Deals,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuId {
    Tag,
    Dnc,
    Sort,
    Stage,
    /// The board's "Move to" for this deal.
    Move(Uuid),
}

struct OpenMenu {
    id: MenuId,
    cursor: usize,
}

/// A row with an id (a list is de-duplicated by it after paging).
trait Row {
    fn row_id(&self) -> Uuid;
}
impl Row for CrmCompany {
    fn row_id(&self) -> Uuid {
        self.id
    }
}
impl Row for CrmContact {
    fn row_id(&self) -> Uuid {
        self.id
    }
}
impl Row for CrmDeal {
    fn row_id(&self) -> Uuid {
        self.id
    }
}

/// One list of records and how its loading is going.
struct Listing<T> {
    rows: Vec<T>,
    /// The first page answered (or the load failed).
    loaded: bool,
    /// Every page is in.
    complete: bool,
    loading: bool,
    /// A refresh was asked for while loading: run one more after.
    again: bool,
    /// The filters the rows were loaded with.
    key: Option<String>,
    /// The rows are the whole list (no search or filter).
    unfiltered: bool,
    error: Option<String>,
    scroll: UniformListScrollHandle,
    task: Option<Task<()>>,
}

impl<T> Default for Listing<T> {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            loaded: false,
            complete: false,
            loading: false,
            again: false,
            key: None,
            unfiltered: true,
            error: None,
            scroll: UniformListScrollHandle::new(),
            task: None,
        }
    }
}

/// The activity list: newest first, a page more as you scroll.
#[derive(Default)]
struct Activities {
    rows: Vec<CrmActivity>,
    loaded: bool,
    more: bool,
    loading: bool,
    again: bool,
    error: Option<String>,
    scroll: UniformListScrollHandle,
    task: Option<Task<()>>,
}

/// An import in progress: the file, what a dry run says, then what the import did.
struct Import {
    kind: &'static str,
    file: String,
    csv: String,
    shape: model::CsvShape,
    preview: Option<CrmImportResult>,
    done: Option<CrmImportResult>,
    busy: bool,
    error: Option<String>,
}

/// "Deleted … · Undo" after a delete from the panel.
struct UndoBar {
    rec: Rec,
    name: String,
    at: Instant,
    busy: bool,
}

pub struct CrmPage {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    tab: Tab,
    /// Pipeline as a list instead of the board.
    deal_list: bool,
    search: Entity<InputState>,
    /// The applied search (the box, after a short pause).
    q: String,
    search_wait: Option<Task<()>>,
    tag: Option<String>,
    dnc: Option<bool>,
    stage: Option<DealStage>,
    sort_companies: &'static str,
    sort_contacts: &'static str,
    sort_deals: &'static str,
    companies: Listing<CrmCompany>,
    contacts: Listing<CrmContact>,
    deals: Listing<CrmDeal>,
    activities: Activities,
    pipeline: Option<Vec<PipelineStage>>,
    pipeline_error: Option<String>,
    pipeline_task: Option<Task<()>>,
    /// Totals of the unfiltered lists (the header's counts).
    company_total: Option<usize>,
    contact_total: Option<usize>,
    /// Tags seen on the unfiltered lists, most used first.
    company_tags: Vec<(String, usize)>,
    contact_tags: Vec<(String, usize)>,
    /// Names of records by id (the activity list's links).
    names: HashMap<Uuid, (Rec, SharedString)>,
    menu: Option<OpenMenu>,
    menu_focus: FocusHandle,
    board_focus: FocusHandle,
    board_scroll: ScrollHandle,
    /// Cards shown per board column.
    board_more: HashMap<usize, usize>,
    /// The board's keyboard cursor: (column, card) among the cards shown.
    cursor: Option<(usize, usize)>,
    /// The ids of the cards the board shows, per column (for the keyboard).
    board_ids: Vec<Vec<Uuid>>,
    panel: Option<Entity<RecordPanel>>,
    /// The records opened before the current one in the panel (Back).
    trail: Vec<Rec>,
    import: Option<Import>,
    exporting: bool,
    undo: Option<UndoBar>,
    /// The page is on screen: notices refresh at once (else they mark it stale).
    shown: bool,
    stale: bool,
    /// The teammates' names (rows show who added what): a redraw when they change.
    bots_seen: Vec<(Uuid, String)>,
}

impl EventEmitter<CrmEvent> for CrmPage {}

impl CrmPage {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = text_input::new_line("Search", false, window, cx);
        cx.subscribe_in(&search, window, |this: &mut Self, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change | InputEvent::PressEnter { .. }) {
                let now = matches!(ev, InputEvent::PressEnter { .. });
                this.search_changed(now, cx);
            }
        })
        .detach();
        cx.subscribe(&data, |this: &mut Self, data, ev: &DataEvent, cx| match ev {
            DataEvent::Changed(None) => this.notice(None, cx),
            DataEvent::Changed(Some(n)) if n.t.starts_with("crm_") => this.notice(Some(n.t.as_str()), cx),
            DataEvent::Updated(Part::Overview) => {
                let seen: Vec<(Uuid, String)> = data.read(cx).bots().iter().map(|b| (b.id, b.name.clone())).collect();
                if seen != this.bots_seen {
                    this.bots_seen = seen;
                    cx.notify();
                }
            }
            _ => {}
        })
        .detach();
        let bots_seen = data.read(cx).bots().iter().map(|b| (b.id, b.name.clone())).collect();
        let mut this = Self {
            data,
            toasts,
            tab: Tab::Companies,
            deal_list: false,
            search,
            q: String::new(),
            search_wait: None,
            tag: None,
            dnc: None,
            stage: None,
            sort_companies: "updated",
            sort_contacts: "updated",
            sort_deals: "updated",
            companies: Listing::default(),
            contacts: Listing::default(),
            deals: Listing::default(),
            activities: Activities::default(),
            pipeline: None,
            pipeline_error: None,
            pipeline_task: None,
            company_total: None,
            contact_total: None,
            company_tags: Vec::new(),
            contact_tags: Vec::new(),
            names: HashMap::new(),
            menu: None,
            menu_focus: cx.focus_handle(),
            board_focus: cx.focus_handle(),
            board_scroll: ScrollHandle::new(),
            board_more: HashMap::new(),
            cursor: None,
            board_ids: Vec::new(),
            panel: None,
            trail: Vec::new(),
            import: None,
            exporting: false,
            undo: None,
            shown: true,
            stale: false,
            bots_seen,
        };
        this.reload(Which::Companies, cx);
        this.reload(Which::Contacts, cx);
        this.load_pipeline(cx);
        this
    }

    fn client(&self, cx: &App) -> familiar_client::Client {
        self.data.read(cx).client.clone()
    }

    fn toast(&self, tone: Tone, title: impl Into<SharedString>, body: Option<String>, cx: &mut Context<Self>) {
        let title = title.into();
        self.toasts.update(cx, |t, cx| t.push(tone, title, body.map(Into::into), cx));
    }

    /// The shell shows or hides the page: a page that went stale while hidden catches up.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.shown = shown;
        if shown && std::mem::take(&mut self.stale) {
            self.notice(None, cx);
        }
    }

    /// Open a tab (also `--open crm/<tab>`).
    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.menu = None;
        self.tab = tab;
        self.ensure(cx);
        cx.notify();
    }

    /// The board, or the list of deals.
    pub fn set_deal_list(&mut self, list: bool, cx: &mut Context<Self>) {
        self.deal_list = list;
        self.ensure(cx);
        cx.notify();
    }

    /// Open a record in the side panel; `push` keeps the current one for Back.
    pub fn open(&mut self, rec: Rec, push: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = &self.panel {
            let current = p.read(cx).rec();
            if current == rec {
                return;
            }
            if push {
                self.trail.push(current);
            } else {
                self.trail.clear();
            }
        }
        let (data, toasts) = (self.data.clone(), self.toasts.clone());
        let panel = cx.new(|cx| RecordPanel::new(data, toasts, rec, window, cx));
        cx.subscribe_in(&panel, window, Self::on_panel).detach();
        panel.update(cx, |p, cx| p.focus(window, cx));
        self.panel = Some(panel);
        self.sync_back(cx);
        cx.notify();
    }

    /// The panel offers Back while there is a record to go back to.
    fn sync_back(&self, cx: &mut Context<Self>) {
        let back = !self.trail.is_empty();
        if let Some(p) = &self.panel {
            p.update(cx, |p, _| p.set_back(back));
        }
    }

    fn close_panel(&mut self, cx: &mut Context<Self>) {
        self.panel = None;
        self.trail.clear();
        cx.notify();
    }

    fn on_panel(&mut self, _: &Entity<RecordPanel>, ev: &PanelEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            PanelEvent::Close => {
                self.close_panel(cx);
                // Back to the board's keyboard when the deal was opened from it.
                if self.tab == Tab::Pipeline && !self.deal_list {
                    self.board_focus.focus(window, cx);
                }
            }
            PanelEvent::Back => match self.trail.pop() {
                Some(rec) => {
                    let trail = std::mem::take(&mut self.trail);
                    self.panel = None;
                    self.open(rec, false, window, cx);
                    self.trail = trail;
                    self.sync_back(cx);
                }
                None => self.close_panel(cx),
            },
            PanelEvent::Open(rec) => self.open(*rec, true, window, cx),
            PanelEvent::Deleted { rec, name } => {
                self.close_panel(cx);
                self.undo = Some(UndoBar { rec: *rec, name: name.clone(), at: Instant::now(), busy: false });
                // The bar goes away on its own.
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(UNDO_FOR).await;
                    let _ = this.update(cx, |p, cx| {
                        if p.undo.as_ref().is_some_and(|u| u.at.elapsed() >= UNDO_FOR && !u.busy) {
                            p.undo = None;
                            cx.notify();
                        }
                    });
                })
                .detach();
                self.notice(Some(rec.table()), cx);
            }
            PanelEvent::OpenNeeds => cx.emit(CrmEvent::OpenNeeds),
            PanelEvent::Saved(rec) => {
                // Shown at once rather than after the notice's round trip.
                self.notice(Some(rec.table()), cx);
            }
        }
    }

    /// Undo the delete the bar offers: the record's newest change (the delete) is undone, and it opens again.
    fn undo_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bar) = self.undo.as_mut().filter(|u| !u.busy) else { return };
        bar.busy = true;
        let rec = bar.rec;
        cx.notify();
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move {
            let p = familiar_client::CrmChangeParams {
                entity: Some(rec.entity().into()),
                entity_id: Some(rec.id()),
                limit: Some(5),
                ..Default::default()
            };
            let changes = client.crm_changes(&p).await?;
            match model::undoable(&changes) {
                Some(c) => client.undo_crm_change(c.id).await.map(|_| ()),
                None => Err(ApiError::Http { status: 409, message: "There's nothing to undo any more.".into() }),
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update_in(cx, |p, window, cx| {
                p.undo = None;
                match r {
                    Ok(()) => {
                        p.toast(Tone::Ok, "Brought back", None, cx);
                        p.notice(Some(rec.table()), cx);
                        p.open(rec, false, window, cx);
                    }
                    Err(e) => p.toast(Tone::Bad, "Couldn't undo it", Some(e), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- loading ------------------------------------------------------------------------------------------------

    fn filtered(&self, which: Which) -> bool {
        !self.q.is_empty()
            || match which {
                Which::Companies => self.tag.is_some(),
                Which::Contacts => self.tag.is_some() || self.dnc.is_some(),
                Which::Deals => self.stage.is_some(),
            }
    }

    fn params(&self, which: Which) -> CrmListParams {
        let q = (!self.q.is_empty()).then(|| self.q.clone());
        match which {
            Which::Companies => CrmListParams { q, tag: self.tag.clone(), sort: Some(self.sort_companies.into()), ..Default::default() },
            Which::Contacts => {
                CrmListParams { q, tag: self.tag.clone(), dnc: self.dnc, sort: Some(self.sort_contacts.into()), ..Default::default() }
            }
            Which::Deals => CrmListParams {
                q,
                stage: self.stage.map(|s| s.as_str().to_owned()),
                sort: Some(self.sort_deals.into()),
                ..Default::default()
            },
        }
    }

    fn key(p: &CrmListParams) -> String {
        format!("{:?}|{:?}|{:?}|{:?}|{:?}", p.q, p.tag, p.dnc, p.stage, p.sort)
    }

    /// Load (or refresh, when the filters are the same) one of the paged lists.
    fn reload(&mut self, which: Which, cx: &mut Context<Self>) {
        let client = self.client(cx);
        let p = self.params(which);
        let key = Self::key(&p);
        let unfiltered = !self.filtered(which);
        fn paged(p: &CrmListParams, offset: u32) -> CrmListParams {
            CrmListParams { limit: Some(PAGE), offset: Some(offset), ..p.clone() }
        }
        match which {
            Which::Companies => self.load(which, |s| &mut s.companies, key, unfiltered, cx, move |o| {
                let (c, p) = (client.clone(), paged(&p, o));
                async move { c.crm_companies(&p).await }
            }),
            Which::Contacts => self.load(which, |s| &mut s.contacts, key, unfiltered, cx, move |o| {
                let (c, p) = (client.clone(), paged(&p, o));
                async move { c.crm_contacts(&p).await }
            }),
            Which::Deals => self.load(which, |s| &mut s.deals, key, unfiltered, cx, move |o| {
                let (c, p) = (client.clone(), paged(&p, o));
                async move { c.crm_deals(&p).await }
            }),
        }
    }

    /// Page through a list in the background. A new filter shows rows as their pages arrive; a refresh of the same
    /// filter keeps the current rows until the new ones are all in (no flicker, no jumping counts).
    fn load<T, F, Fut>(
        &mut self,
        which: Which,
        pick: fn(&mut Self) -> &mut Listing<T>,
        key: String,
        unfiltered: bool,
        cx: &mut Context<Self>,
        fetch: F,
    ) where
        T: Row + Send + 'static,
        F: Fn(u32) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Vec<T>, ApiError>> + Send + 'static,
    {
        let l = pick(self);
        let refresh = l.loaded && l.key.as_deref() == Some(key.as_str());
        if refresh && l.loading {
            l.again = true;
            return;
        }
        l.again = false;
        l.loading = true;
        if !refresh {
            l.rows.clear();
            l.loaded = false;
            l.complete = false;
            l.error = None;
            l.key = Some(key);
            l.unfiltered = unfiltered;
            l.scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Result<Vec<T>, String>>();
        let pages = Tokio::spawn(cx, async move {
            let mut offset = 0u32;
            loop {
                let r = fetch(offset).await;
                let n = r.as_ref().map_or(0, Vec::len);
                let failed = r.is_err();
                if tx.unbounded_send(r.map_err(|e| e.message())).is_err() || failed || n < PAGE as usize {
                    break;
                }
                offset += PAGE;
                if offset as usize >= MAX_ROWS {
                    break;
                }
            }
        });
        l.task = Some(cx.spawn(async move |this, cx| {
            // Dropping this task (a newer load replaced it) drops `pages`, which stops the paging.
            let _pages = pages;
            let mut buf: Vec<T> = Vec::new();
            let mut failed = None;
            while let Some(page) = rx.next().await {
                match page {
                    Ok(rows) if refresh => buf.extend(rows),
                    Ok(rows) => {
                        let shown = this.update(cx, |p, cx| {
                            let l = pick(p);
                            l.rows.extend(rows);
                            l.loaded = true;
                            cx.notify();
                        });
                        if shown.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        failed = Some(e);
                        break;
                    }
                }
            }
            let _ = this.update(cx, |p, cx| {
                let l = pick(p);
                l.loading = false;
                l.loaded = true;
                match failed {
                    Some(e) => l.error = Some(e),
                    None => {
                        if refresh {
                            l.rows = buf;
                        }
                        let mut seen = std::collections::HashSet::new();
                        l.rows.retain(|r| seen.insert(r.row_id()));
                        l.complete = true;
                        l.error = None;
                    }
                }
                let again = std::mem::take(&mut l.again);
                p.loaded(which);
                cx.notify();
                if again {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(REFRESH_GAP).await;
                        let _ = this.update(cx, |p, cx| p.reload(which, cx));
                    })
                    .detach();
                }
            });
        }));
        cx.notify();
    }

    /// A list finished loading: the header's totals, the tag menus and the activity list's names follow it.
    fn loaded(&mut self, which: Which) {
        fn tally<'a>(tags: impl Iterator<Item = &'a String>) -> Vec<(String, usize)> {
            let mut by: HashMap<String, (String, usize)> = HashMap::new();
            for t in tags {
                by.entry(t.to_lowercase()).or_insert_with(|| (t.clone(), 0)).1 += 1;
            }
            let mut v: Vec<(String, usize)> = by.into_values().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
            v.truncate(40);
            v
        }
        match which {
            Which::Companies if self.companies.complete => {
                if self.companies.unfiltered {
                    self.company_total = Some(self.companies.rows.len());
                }
                if self.q.is_empty() && self.tag.is_none() {
                    self.company_tags = tally(self.companies.rows.iter().flat_map(|c| &c.tags));
                }
            }
            Which::Contacts if self.contacts.complete => {
                if self.contacts.unfiltered {
                    self.contact_total = Some(self.contacts.rows.len());
                }
                if self.q.is_empty() && self.tag.is_none() && self.dnc.is_none() {
                    self.contact_tags = tally(self.contacts.rows.iter().flat_map(|c| &c.tags));
                }
            }
            _ => {}
        }
        self.rebuild_names();
    }

    fn rebuild_names(&mut self) {
        let mut names = HashMap::new();
        for c in &self.companies.rows {
            names.insert(c.id, (Rec::Company(c.id), SharedString::from(c.name.clone())));
        }
        for c in &self.contacts.rows {
            names.insert(c.id, (Rec::Contact(c.id), SharedString::from(c.name.clone())));
        }
        for s in self.pipeline.iter().flatten() {
            for d in &s.deals {
                names.insert(d.id, (Rec::Deal(d.id), SharedString::from(d.title.clone())));
            }
        }
        for d in &self.deals.rows {
            names.insert(d.id, (Rec::Deal(d.id), SharedString::from(d.title.clone())));
        }
        self.names = names;
    }

    fn load_pipeline(&mut self, cx: &mut Context<Self>) {
        let client = self.client(cx);
        if self.pipeline.is_none()
            && let Some(cached) = client.peek::<Vec<PipelineStage>>("/api/crm/pipeline")
        {
            self.pipeline = Some(cached);
        }
        let task = Tokio::spawn(cx, async move { client.crm_pipeline().await });
        self.pipeline_task = Some(cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                match r {
                    Ok(stages) => {
                        p.pipeline = Some(stages);
                        p.pipeline_error = None;
                        p.rebuild_names();
                    }
                    Err(e) => p.pipeline_error = Some(e),
                }
                cx.notify();
            });
        }));
    }

    /// Load the activity list (`more`: the next page; else the newest page, merged with what is shown).
    fn load_activities(&mut self, more: bool, cx: &mut Context<Self>) {
        let client = self.client(cx);
        let a = &mut self.activities;
        if a.loading {
            a.again |= !more;
            return;
        }
        if more && (!a.more || !a.loaded) {
            return;
        }
        a.loading = true;
        let offset = if more { a.rows.len() as u32 } else { 0 };
        let p = CrmActivityParams { limit: Some(ACT_PAGE), offset: Some(offset), ..Default::default() };
        let task = Tokio::spawn(cx, async move { client.crm_activities(&p).await });
        a.task = Some(cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                let a = &mut p.activities;
                a.loading = false;
                a.loaded = true;
                match r {
                    Ok(rows) => {
                        a.error = None;
                        let full = rows.len() as u32 >= ACT_PAGE;
                        if more {
                            a.more = full;
                            let known: std::collections::HashSet<Uuid> = a.rows.iter().map(|r| r.id).collect();
                            a.rows.extend(rows.into_iter().filter(|r| !known.contains(&r.id)));
                        } else {
                            // The newest page, then whatever older rows were already shown.
                            let fresh: std::collections::HashSet<Uuid> = rows.iter().map(|r| r.id).collect();
                            let older = a.rows.iter().filter(|r| !fresh.contains(&r.id)).cloned().collect::<Vec<_>>();
                            if a.rows.is_empty() {
                                a.more = full;
                            }
                            a.rows = rows;
                            a.rows.extend(older);
                            a.rows.sort_by(|x, y| y.occurred_at.cmp(&x.occurred_at).then(y.id.cmp(&x.id)));
                        }
                    }
                    Err(e) => a.error = Some(e),
                }
                if std::mem::take(&mut a.again) {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(REFRESH_GAP).await;
                        let _ = this.update(cx, |p, cx| p.load_activities(false, cx));
                    })
                    .detach();
                }
                cx.notify();
            });
        }));
    }

    /// The tab on screen has what its filters ask for.
    fn ensure(&mut self, cx: &mut Context<Self>) {
        let want = |s: &Self, w: Which| Some(Self::key(&s.params(w)));
        match self.tab {
            Tab::Companies if self.companies.key != want(self, Which::Companies) => self.reload(Which::Companies, cx),
            Tab::Contacts if self.contacts.key != want(self, Which::Contacts) => self.reload(Which::Contacts, cx),
            Tab::Pipeline if self.deal_list && self.deals.key != want(self, Which::Deals) => self.reload(Which::Deals, cx),
            Tab::Activity if !self.activities.loaded => self.load_activities(false, cx),
            _ => {}
        }
    }

    /// A change notice for `table` (`None`: everything may have changed).
    fn notice(&mut self, table: Option<&str>, cx: &mut Context<Self>) {
        if !self.shown {
            self.stale = true;
            return;
        }
        let all = table.is_none();
        let t = table.unwrap_or("");
        // A contact or deal row shows its company's name; a deal shows its contact's and whether they opted out.
        if all || t == "crm_companies" {
            self.reload(Which::Companies, cx);
        }
        if all || t == "crm_companies" || t == "crm_contacts" {
            self.reload(Which::Contacts, cx);
        }
        if all || matches!(t, "crm_companies" | "crm_contacts" | "crm_deals") {
            if self.deals.key.is_some() {
                self.reload(Which::Deals, cx);
            }
            self.load_pipeline(cx);
        }
        if (all || t == "crm_activities") && self.activities.loaded {
            self.load_activities(false, cx);
        }
    }

    fn search_changed(&mut self, now: bool, cx: &mut Context<Self>) {
        let wait = if now { Duration::ZERO } else { Duration::from_millis(250) };
        self.search_wait = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |p, cx| {
                let q = p.search.read(cx).value().trim().to_owned();
                if q != p.q {
                    p.q = q;
                    p.ensure(cx);
                    p.cursor = None;
                    cx.notify();
                }
            });
        }));
    }

    fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tag = None;
        self.dnc = None;
        self.stage = None;
        self.q.clear();
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        self.ensure(cx);
        cx.notify();
    }

    /// The CRM holds nothing at all (and nothing is filtered): the page is its empty state.
    fn is_empty(&self) -> bool {
        let deals = self.pipeline.as_ref().map(|s| s.iter().map(|s| s.count).sum::<i64>());
        self.company_total == Some(0) && self.contact_total == Some(0) && deals == Some(0)
    }

    // ---- menus --------------------------------------------------------------------------------------------------

    fn open_menu(&mut self, id: MenuId, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.as_ref().is_some_and(|m| m.id == id) {
            self.menu = None;
        } else {
            let cursor = self.menu_items(id).iter().position(|i| i.checked).unwrap_or(0);
            self.menu = Some(OpenMenu { id, cursor });
            self.menu_focus.focus(window, cx);
        }
        cx.notify();
    }

    fn menu_items(&self, id: MenuId) -> Vec<MenuItem> {
        match id {
            MenuId::Tag => {
                let tags = if self.tab == Tab::Contacts { &self.contact_tags } else { &self.company_tags };
                let mut v = vec![MenuItem::new("Any tag").checked(self.tag.is_none())];
                v.extend(tags.iter().map(|(t, n)| {
                    MenuItem::new(t.clone()).detail(n.to_string()).checked(self.tag.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(t)))
                }));
                v
            }
            MenuId::Dnc => vec![
                MenuItem::new("Everyone").checked(self.dnc.is_none()),
                MenuItem::new("OK to contact").checked(self.dnc == Some(false)),
                MenuItem::new("Do not contact").checked(self.dnc == Some(true)),
            ],
            MenuId::Sort => {
                let (current, opts): (&str, &[(&str, &str)]) = match self.tab {
                    Tab::Companies => (self.sort_companies, &[("updated", "Recently changed"), ("name", "Name"), ("fit", "Best fit")]),
                    Tab::Contacts => (self.sort_contacts, &[("updated", "Recently changed"), ("name", "Name")]),
                    _ => (self.sort_deals, &[("updated", "Recently changed"), ("name", "Title")]),
                };
                opts.iter().map(|(k, l)| MenuItem::new(*l).checked(*k == current)).collect()
            }
            MenuId::Stage => {
                let mut v = vec![MenuItem::new("Every stage").checked(self.stage.is_none())];
                let counts: HashMap<DealStage, i64> =
                    self.pipeline.iter().flatten().map(|s| (s.stage, s.count)).collect();
                v.extend(STAGES.iter().map(|s| {
                    MenuItem::new(model::stage_label(*s))
                        .detail(counts.get(s).map(|n| n.to_string()).unwrap_or_default())
                        .checked(self.stage == Some(*s))
                }));
                v
            }
            MenuId::Move(deal) => {
                let at = self.find_card(deal).map(|d| d.stage);
                STAGES
                    .iter()
                    .enumerate()
                    .map(|(i, s)| MenuItem::new(model::stage_label(*s)).detail(format!("{}", i + 1)).checked(at == Some(*s)))
                    .collect()
            }
        }
    }

    fn pick(&mut self, id: MenuId, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match id {
            MenuId::Tag => {
                let tags = if self.tab == Tab::Contacts { &self.contact_tags } else { &self.company_tags };
                self.tag = if i == 0 { None } else { tags.get(i - 1).map(|(t, _)| t.clone()) };
            }
            MenuId::Dnc => self.dnc = [None, Some(false), Some(true)].get(i).copied().flatten(),
            MenuId::Sort => {
                let key = |opts: &[&'static str]| opts.get(i).copied().unwrap_or("updated");
                match self.tab {
                    Tab::Companies => self.sort_companies = key(&["updated", "name", "fit"]),
                    Tab::Contacts => self.sort_contacts = key(&["updated", "name"]),
                    _ => self.sort_deals = key(&["updated", "name"]),
                }
            }
            MenuId::Stage => self.stage = if i == 0 { None } else { STAGES.get(i - 1).copied() },
            MenuId::Move(deal) => {
                if let Some(stage) = STAGES.get(i) {
                    self.move_deal(deal, *stage, cx);
                }
                self.board_focus.focus(window, cx);
            }
        }
        self.ensure(cx);
        cx.notify();
    }

    /// The open menu's panel, for the trigger of `id`.
    fn popover(&self, id: MenuId, width: f32, right: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.menu.as_ref().filter(|m| m.id == id)?;
        let (pick, cursor, close) = (cx.entity(), cx.entity(), cx.entity());
        Some(
            menu::popover(
                format!("crm-menu-{id:?}"),
                &self.menu_focus,
                self.menu_items(id),
                m.cursor,
                width,
                right,
                move |i, window, cx| pick.update(cx, |p, cx| p.pick(id, i, window, cx)),
                move |i, _, cx| {
                    cursor.update(cx, |p, cx| {
                        if let Some(m) = p.menu.as_mut() {
                            m.cursor = i;
                        }
                        cx.notify()
                    })
                },
                move |window, cx| {
                    close.update(cx, |p, cx| {
                        p.menu = None;
                        if matches!(id, MenuId::Move(_)) {
                            p.board_focus.focus(window, cx);
                        }
                        cx.notify()
                    })
                },
                cx,
            )
            .into_any_element(),
        )
    }

    /// A toolbar select with its menu.
    fn select(&self, id: MenuId, label: String, active: bool, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .child(
                menu::trigger(format!("crm-sel-{id:?}"), label, active, cx)
                    .on_click(move |_, window, cx| this.update(cx, |p, cx| p.open_menu(id, window, cx))),
            )
            .children(self.popover(id, width, false, cx))
            .into_any_element()
    }

    // ---- the board ----------------------------------------------------------------------------------------------

    fn find_card(&self, deal: Uuid) -> Option<&PipelineDeal> {
        self.pipeline.iter().flatten().flat_map(|s| &s.deals).find(|d| d.id == deal)
    }

    /// Move a deal to another stage: on the board at once, then through the API (a failure puts it back).
    fn move_deal(&mut self, deal: Uuid, to: DealStage, cx: &mut Context<Self>) {
        let Some(stages) = self.pipeline.as_mut() else { return };
        let Some((from, at)) = stages.iter().enumerate().find_map(|(si, s)| s.deals.iter().position(|d| d.id == deal).map(|di| (si, di)))
        else {
            return;
        };
        if stages[from].stage == to {
            return;
        }
        let mut card = stages[from].deals.remove(at);
        let value = card.value_cents.unwrap_or(0);
        stages[from].count -= 1;
        stages[from].value_cents -= value;
        card.stage = to;
        card.stage_changed_at = Some(Utc::now());
        let title = card.title.clone();
        let Some(ti) = stages.iter().position(|s| s.stage == to) else { return };
        stages[ti].deals.insert(0, card);
        stages[ti].count += 1;
        stages[ti].value_cents += value;
        self.cursor = Some((ti, 0));
        cx.notify();
        let client = self.client(cx);
        let patch = DealPatch { stage: Some(to.as_str().to_owned()), ..Default::default() };
        let task = Tokio::spawn(cx, async move { client.update_crm_deal(deal, &patch).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                if let Err(e) = r {
                    p.toast(Tone::Bad, format!("Couldn't move “{}”", excerpt(&title, 40)), Some(e), cx);
                    p.load_pipeline(cx);
                }
            });
        })
        .detach();
    }

    /// The board's keyboard: arrows move between cards, Shift+←/→ moves the deal a stage, M (or Space) opens the
    /// stage menu, Enter opens the deal, Escape lets go.
    fn board_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_some() || self.board_ids.is_empty() {
            return;
        }
        let k = &ev.keystroke;
        let cols = self.board_ids.len();
        let first = || (0..cols).find(|c| !self.board_ids[*c].is_empty()).map(|c| (c, 0));
        let Some((c, r)) = self.cursor.filter(|(c, r)| *c < cols && *r < self.board_ids[*c].len()).or_else(first) else {
            return;
        };
        let id = self.board_ids[c][r];
        let next_col = |dir: isize| {
            let mut x = c as isize + dir;
            while x >= 0 && (x as usize) < cols {
                if !self.board_ids[x as usize].is_empty() {
                    return Some(x as usize);
                }
                x += dir;
            }
            None
        };
        match k.key.as_str() {
            "left" | "right" if k.modifiers.shift => {
                let dir = if k.key == "left" { -1 } else { 1 };
                let to = c as isize + dir;
                if (0..STAGES.len() as isize).contains(&to) {
                    self.move_deal(id, STAGES[to as usize], cx);
                }
            }
            "down" => self.cursor = Some((c, (r + 1).min(self.board_ids[c].len() - 1))),
            "up" => self.cursor = Some((c, r.saturating_sub(1))),
            "left" | "right" => {
                if let Some(n) = next_col(if k.key == "left" { -1 } else { 1 }) {
                    self.cursor = Some((n, r.min(self.board_ids[n].len() - 1)));
                }
            }
            "enter" => self.open(Rec::Deal(id), false, window, cx),
            "m" | "space" => {
                self.cursor = Some((c, r));
                self.open_menu(MenuId::Move(id), window, cx);
            }
            "escape" => self.cursor = None,
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn board(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let Some(stages) = self.pipeline.clone() else {
            if let Some(e) = self.pipeline_error.clone() {
                return self.failed("Couldn't load the pipeline", e, cx);
            }
            return div()
                .flex()
                .gap(px(12.0))
                .children((0..4).map(|_| div().w(px(264.0)).child(Skeleton::new(220.0).radius(RADIUS_CARD))))
                .into_any_element();
        };
        let focused = self.board_focus.is_focused(window);
        let q = self.q.clone();
        let mut ids = Vec::with_capacity(stages.len());
        let mut columns = Vec::with_capacity(stages.len());
        for (ci, st) in stages.iter().enumerate() {
            let cards: Vec<&PipelineDeal> = st
                .deals
                .iter()
                .filter(|d| model::matches(&q, &[Some(&d.title), d.company_name.as_deref(), d.contact_name.as_deref(), d.next_step.as_deref()]))
                .collect();
            let limit = *self.board_more.get(&ci).unwrap_or(&BOARD_PAGE);
            ids.push(cards.iter().take(limit).map(|d| d.id).collect::<Vec<_>>());
            let mut list = div()
                .id(("crm-col", ci))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .px(px(8.0))
                .pb(px(10.0));
            for (ri, d) in cards.iter().take(limit).enumerate() {
                let lit = focused && self.cursor == Some((ci, ri));
                list = list.child(self.deal_card(d, ci, ri, lit, cx));
            }
            if cards.len() > limit {
                let this = cx.entity();
                list = list.child(
                    Button::new(("crm-col-more", ci), format!("Show {} more", (cards.len() - limit).min(BOARD_PAGE)))
                        .ghost()
                        .size(ButtonSize::Small)
                        .full_width()
                        .on_click(move |_, _, cx| {
                            this.update(cx, |p, cx| {
                                *p.board_more.entry(ci).or_insert(BOARD_PAGE) += BOARD_PAGE;
                                cx.notify()
                            })
                        }),
                );
            }
            if (st.count as usize) > st.deals.len() && cards.len() <= limit {
                list = list.child(
                    div()
                        .px(px(4.0))
                        .text_size(px(text::CAPTION))
                        .text_color(theme.muted)
                        .child(format!("The newest {} are here; the list view has all {}.", st.deals.len(), st.count)),
                );
            }
            if cards.is_empty() {
                list = list.child(
                    div()
                        .mx(px(2.0))
                        .py(px(18.0))
                        .rounded(px(10.0))
                        .border_1()
                        .border_dashed()
                        .border_color(theme.line)
                        .flex()
                        .justify_center()
                        .text_size(px(text::CAPTION))
                        .text_color(theme.muted)
                        .child(if q.is_empty() { "No deals" } else { "None match" }),
                );
            }
            let currency = st.deals.first().map(|d| d.currency.clone()).unwrap_or_else(|| "USD".into());
            let head = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(12.0))
                .pt(px(12.0))
                .pb(px(10.0))
                .child(div().size(px(8.0)).rounded_full().bg(theme.tone(model::stage_tone(st.stage)).0))
                .child(div().font_weight(FontWeight::MEDIUM).text_size(px(text::SMALL)).child(model::stage_label(st.stage)))
                .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(model::thousands(st.count.max(0) as u64)))
                .child(div().flex_1())
                .when(st.value_cents > 0, |el| {
                    el.child(
                        div()
                            .text_size(px(text::CAPTION))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted)
                            .child(model::money_short(st.value_cents, &currency)),
                    )
                });
            columns.push(
                div()
                    .w(px(264.0))
                    .flex_none()
                    .h_full()
                    .flex()
                    .flex_col()
                    .rounded(px(RADIUS_CARD))
                    .bg(theme.sunken.opacity(0.55))
                    .border_1()
                    .border_color(theme.line.opacity(0.6))
                    .child(head)
                    .child(list),
            );
        }
        self.board_ids = ids;
        let hint = focused.then(|| {
            div()
                .flex_none()
                .pt(px(8.0))
                .text_size(px(text::CAPTION))
                .text_color(theme.muted)
                .child("Arrows move between cards · M moves the deal to another stage · Shift+← → moves it one stage · Enter opens it")
        });
        let this = cx.entity();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("crm-board")
                    .flex_1()
                    .min_h_0()
                    .track_focus(&self.board_focus)
                    .on_key_down(cx.listener(Self::board_key))
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                        this.update(cx, |p, cx| p.board_focus.focus(window, cx));
                    })
                    .overflow_x_scroll()
                    .track_scroll(&self.board_scroll)
                    .child(div().h_full().flex().gap(px(12.0)).pb(px(4.0)).children(columns)),
            )
            .children(hint)
            .into_any_element()
    }

    fn deal_card(&self, d: &PipelineDeal, ci: usize, ri: usize, lit: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let id = d.id;
        let key = id.as_u128() as u64;
        let selected = self.panel_rec(cx) == Some(Rec::Deal(id));
        let who = [d.company_name.as_deref(), d.contact_name.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        let next = d.next_step.as_deref().filter(|s| !s.trim().is_empty()).map(|s| {
            let due = d.next_step_at.map(|t| model::due_words(t, Utc::now()));
            div()
                .flex()
                .items_start()
                .gap(px(6.0))
                .text_size(px(text::CAPTION))
                .child(div().pt(px(1.0)).child(icon(icons::ARROW_RIGHT).size(px(12.0)).text_color(theme.muted)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(theme.ink)
                        .line_clamp(2)
                        .child(excerpt(s, 90))
                        .when_some(due, |el, (w, late)| {
                            el.child(div().text_color(if late { theme.warn } else { theme.muted }).child(w))
                        }),
                )
        });
        let open = cx.entity();
        let menu_this = cx.entity();
        let menu_open = self.menu.as_ref().is_some_and(|m| m.id == MenuId::Move(id));
        div()
            .id(("crm-card", key))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(12.0))
            .rounded(px(12.0))
            .bg(if selected { theme.accent_soft } else { theme.surface })
            .border_1()
            .border_color(if lit || selected { theme.accent } else { theme.line })
            .shadow(theme.card_shadow(0.0))
            .cursor_pointer()
            .hover(|s| s.border_color(theme.accent.opacity(0.6)))
            .on_click(move |_, window, cx| {
                open.update(cx, |p, cx| {
                    p.cursor = Some((ci, ri));
                    p.open(Rec::Deal(id), false, window, cx)
                })
            })
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(6.0))
                    .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).text_size(px(text::SMALL)).line_clamp(2).child(d.title.clone()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .child(
                                Button::icon_only(("crm-move", key), icons::ALT_ARROW_DOWN)
                                    .size(ButtonSize::Small)
                                    .tooltip("Move to another stage (M)")
                                    .on_click(move |_, window, cx| {
                                        cx.stop_propagation();
                                        menu_this.update(cx, |p, cx| {
                                            p.cursor = Some((ci, ri));
                                            p.open_menu(MenuId::Move(id), window, cx)
                                        })
                                    }),
                            )
                            .when(menu_open, |el| el.children(self.popover(MenuId::Move(id), 200.0, true, cx))),
                    ),
            )
            // Who, and what it's worth.
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(text::CAPTION))
                    .child(div().flex_1().min_w_0().text_color(theme.muted).truncate().child(who))
                    .when_some(d.value_cents, |el, v| {
                        el.child(div().flex_none().font_weight(FontWeight::MEDIUM).text_color(theme.ink).child(model::money(v, &d.currency)))
                    }),
            )
            .children(next)
            .when(d.contact_do_not_contact, |el| el.child(div().flex().child(dnc_badge(cx))))
            .into_any_element()
    }

    // ---- tables -------------------------------------------------------------------------------------------------

    fn panel_rec(&self, cx: &App) -> Option<Rec> {
        self.panel.as_ref().map(|p| p.read(cx).rec())
    }

    /// Room for the table's columns: (wide, medium).
    fn widths(window: &Window) -> (bool, bool) {
        let w = f32::from(window.viewport_size().width) - SIDEBAR_WIDTH - 64.0;
        (w >= 980.0, w >= 820.0)
    }

    /// A table: its header and a virtualised body of `count` rows drawn by `rows`.
    fn table(
        &self,
        id: &'static str,
        head: Vec<(&'static str, Option<f32>, bool)>,
        count: usize,
        scroll: &UniformListScrollHandle,
        rows: fn(&mut Self, Range<usize>, &mut Window, &mut Context<Self>) -> Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let mut header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(16.0))
            .h(px(36.0))
            .px(px(16.0))
            .border_b_1()
            .border_color(theme.line)
            .bg(theme.sunken.opacity(0.5))
            .text_size(px(text::CAPTION))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.muted);
        for (label, w, right) in head {
            header = header.child(cell(w, right).child(label));
        }
        let page = cx.entity();
        card(cx)
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(header)
            .child(
                div().flex_1().min_h_0().child(
                    uniform_list(id, count, move |range, window, cx| page.update(cx, |p, cx| rows(p, range, window, cx)))
                        .track_scroll(scroll)
                        .size_full(),
                ),
            )
            .into_any_element()
    }

    fn row_shell(id: impl Into<gpui::ElementId>, selected: bool, theme: &Theme) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id.into())
            .w_full()
            .flex()
            .items_center()
            .gap(px(16.0))
            .h(px(ROW_H))
            .px(px(16.0))
            .border_b_1()
            .border_color(theme.line.opacity(0.55))
            .cursor_pointer()
            .when(selected, |el| el.bg(theme.accent_soft))
            .when(!selected, |el| el.hover(|s| s.bg(theme.hover)))
    }

    /// Who added a record: a teammate's mascot and name, or "You".
    fn added_by(&self, bot: Option<Uuid>, cx: &App) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let d = self.data.read(cx);
        match bot.and_then(|b| d.bot(b)) {
            Some(b) => div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .min_w_0()
                .child(Mascot::new(format!("crm-by-{}", b.id), avatar_of(b), MascotState::Idle, 20.0).still())
                .child(div().truncate().text_size(px(text::CAPTION)).child(b.name.clone()))
                .into_any_element(),
            None if bot.is_some() => {
                div().text_size(px(text::CAPTION)).text_color(theme.muted).child("A former teammate").into_any_element()
            }
            None => div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(you_dot(&theme))
                .child(div().text_size(px(text::CAPTION)).child("You"))
                .into_any_element(),
        }
    }

    fn company_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        let (wide, mid) = Self::widths(window);
        let sel = self.panel_rec(cx);
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(c) = self.companies.rows.get(ix) else { continue };
            let id = c.id;
            let this = cx.entity();
            let fit = match c.fit_score {
                Some(s) => {
                    let (fg, _) = theme.tone(model::fit_tone(s));
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .w(px(52.0))
                                .h(px(6.0))
                                .rounded_full()
                                .bg(theme.sunken)
                                .child(div().h_full().rounded_full().bg(fg).w(px(52.0 * s.clamp(0, 100) as f32 / 100.0))),
                        )
                        .child(div().text_size(px(text::CAPTION)).font_weight(FontWeight::MEDIUM).text_color(fg).child(s.to_string()))
                        .into_any_element()
                }
                None => dash(&theme),
            };
            out.push(
                Self::row_shell(("crm-co", ix), sel == Some(Rec::Company(id)), &theme)
                    .on_click(move |_, window, cx| this.update(cx, |p, cx| p.open(Rec::Company(id), false, window, cx)))
                    .child(
                        cell(None, false).child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(12.0))
                                .child(monogram(&c.name, false, &theme))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .min_w_0()
                                        .child(div().truncate().font_weight(FontWeight::MEDIUM).child(c.name.clone()))
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(px(text::CAPTION))
                                                .text_color(theme.muted)
                                                .child(c.domain.clone().or_else(|| c.industry.clone()).unwrap_or_default()),
                                        ),
                                ),
                        ),
                    )
                    .child(cell(Some(110.0), false).child(fit))
                    .when(wide, |el| el.child(cell(Some(180.0), false).child(tag_chips(&c.tags, 2, cx))))
                    .when(mid, |el| el.child(cell(Some(150.0), false).child(self.added_by(c.created_by_bot, cx))))
                    .child(cell(Some(84.0), true).child(updated(c.updated_at, &theme)))
                    .into_any_element(),
            );
        }
        out
    }

    fn contact_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        let (wide, mid) = Self::widths(window);
        let sel = self.panel_rec(cx);
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(c) = self.contacts.rows.get(ix) else { continue };
            let id = c.id;
            let this = cx.entity();
            out.push(
                Self::row_shell(("crm-ct", ix), sel == Some(Rec::Contact(id)), &theme)
                    .on_click(move |_, window, cx| this.update(cx, |p, cx| p.open(Rec::Contact(id), false, window, cx)))
                    .child(
                        cell(None, false).child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(12.0))
                                .child(monogram(&c.name, true, &theme))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .min_w_0()
                                        .child(div().truncate().font_weight(FontWeight::MEDIUM).child(c.name.clone()))
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(px(text::CAPTION))
                                                .text_color(theme.muted)
                                                .child(c.title.clone().unwrap_or_default()),
                                        ),
                                ),
                        ),
                    )
                    .when(mid, |el| {
                        el.child(
                            cell(Some(170.0), false)
                                .child(div().truncate().text_size(px(text::SMALL)).child(c.company_name.clone().unwrap_or_default())),
                        )
                    })
                    .when(wide, |el| {
                        el.child(
                            cell(Some(220.0), false).child(
                                div().truncate().text_size(px(text::CAPTION)).text_color(theme.muted).child(c.email.clone().unwrap_or_default()),
                            ),
                        )
                    })
                    .child(cell(Some(128.0), false).when(c.do_not_contact, |el| el.child(dnc_badge(cx))))
                    .child(cell(Some(84.0), true).child(updated(c.updated_at, &theme)))
                    .into_any_element(),
            );
        }
        out
    }

    fn deal_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        let (wide, mid) = Self::widths(window);
        let sel = self.panel_rec(cx);
        let now = Utc::now();
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(d) = self.deals.rows.get(ix) else { continue };
            let id = d.id;
            let this = cx.entity();
            let who = [d.company_name.as_deref(), d.contact_name.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
            let next = d.next_step.clone().filter(|s| !s.trim().is_empty());
            let due = d.next_step_at.map(|t| model::due_words(t, now));
            out.push(
                Self::row_shell(("crm-dl", ix), sel == Some(Rec::Deal(id)), &theme)
                    .on_click(move |_, window, cx| this.update(cx, |p, cx| p.open(Rec::Deal(id), false, window, cx)))
                    .child(
                        cell(None, false).child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(8.0))
                                        .child(div().truncate().font_weight(FontWeight::MEDIUM).child(d.title.clone()))
                                        .when(d.contact_do_not_contact, |el| el.child(dnc_badge(cx))),
                                )
                                .child(div().truncate().text_size(px(text::CAPTION)).text_color(theme.muted).child(who)),
                        ),
                    )
                    .child(cell(Some(110.0), false).child(chip(model::stage_tone(d.stage), model::stage_label(d.stage), cx)))
                    .child(cell(Some(96.0), true).child(match d.value_cents {
                        Some(v) => div().text_size(px(text::SMALL)).child(model::money(v, &d.currency)).into_any_element(),
                        None => dash(&theme),
                    }))
                    .when(mid, |el| {
                        el.child(cell(Some(if wide { 240.0 } else { 180.0 }), false).child(match next {
                            Some(n) => div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .child(div().truncate().text_size(px(text::CAPTION)).child(n))
                                .when_some(due, |el, (w, late)| {
                                    el.child(div().text_size(px(text::CAPTION)).text_color(if late { theme.warn } else { theme.muted }).child(w))
                                })
                                .into_any_element(),
                            None => dash(&theme),
                        }))
                    })
                    .child(cell(Some(84.0), true).child(updated(d.updated_at, &theme)))
                    .into_any_element(),
            );
        }
        out
    }

    /// The activity list's rows match the search on what they say.
    fn shown_activities(&self) -> Vec<usize> {
        self.activities
            .rows
            .iter()
            .enumerate()
            .filter(|(_, a)| model::matches(&self.q, &[Some(&a.summary), a.bot_name.as_deref(), Some(model::kind_label(a.kind))]))
            .map(|(i, _)| i)
            .collect()
    }

    fn activity_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        let (_, about) = Self::widths(window);
        let shown = self.shown_activities();
        // The list reached its end: read the next page.
        if range.end + 20 >= shown.len() && self.activities.more && !self.activities.loading && self.q.is_empty() {
            let this = cx.entity();
            cx.defer(move |cx| this.update(cx, |p, cx| p.load_activities(true, cx)));
        }
        let pending: Vec<Uuid> = self.data.read(cx).pending.iter().map(|a| a.id).collect();
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(a) = shown.get(ix).and_then(|i| self.activities.rows.get(*i)).cloned() else { continue };
            let link = [a.deal_id, a.contact_id, a.company_id].into_iter().flatten().find_map(|id| self.names.get(&id).cloned()).or_else(|| {
                a.deal_id.map(|id| (Rec::Deal(id), "A deal".into()))
                    .or_else(|| a.contact_id.map(|id| (Rec::Contact(id), "A contact".into())))
                    .or_else(|| a.company_id.map(|id| (Rec::Company(id), "A company".into())))
            });
            let waiting = a.approval_id.is_some_and(|id| pending.contains(&id));
            let who = if a.actor_kind == "bot" {
                let d = self.data.read(cx);
                match a.bot_id.and_then(|b| d.bot(b)) {
                    Some(b) => div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .min_w_0()
                        .child(Mascot::new(format!("crm-by-{}", b.id), avatar_of(b), MascotState::Idle, 24.0).still())
                        .child(div().truncate().text_size(px(text::SMALL)).child(b.name.clone()))
                        .into_any_element(),
                    None => div()
                        .truncate()
                        .text_size(px(text::SMALL))
                        .text_color(theme.muted)
                        .child(a.bot_name.clone().unwrap_or_else(|| "A teammate".into()))
                        .into_any_element(),
                }
            } else {
                div().flex().items_center().gap(px(8.0)).child(you_dot(&theme)).child(div().text_size(px(text::SMALL)).child("You")).into_any_element()
            };
            let this = cx.entity();
            let open = link.as_ref().map(|(r, _)| *r);
            out.push(
                Self::row_shell(("crm-act", ix), open.is_some() && open == self.panel_rec(cx), &theme)
                    .when_some(open, |el, rec| el.on_click(move |_, window, cx| this.update(cx, |p, cx| p.open(rec, false, window, cx))))
                    .child(cell(Some(80.0), false).child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(ago(Some(a.occurred_at)))))
                    .child(cell(Some(170.0), false).child(who))
                    .child(
                        cell(None, false).child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .min_w_0()
                                .child(chip(model::kind_tone(a.kind), model::kind_label(a.kind), cx))
                                .child(div().flex_1().min_w_0().truncate().text_size(px(text::SMALL)).child(a.summary.clone()))
                                .when(waiting, |el| el.child(chip(Tone::Warn, "Waiting for you", cx))),
                        ),
                    )
                    .when(about, |el| {
                        el.child(cell(Some(200.0), false).when_some(link, |el, (rec, name)| {
                            el.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .min_w_0()
                                    .text_size(px(text::SMALL))
                                    .text_color(theme.accent)
                                    .child(icon(rec.glyph()).size(px(14.0)).text_color(theme.accent))
                                    .child(div().truncate().child(name)),
                            )
                        }))
                    })
                    .into_any_element(),
            );
        }
        out
    }

    // ---- import & export ----------------------------------------------------------------------------------------

    fn export(&mut self, cx: &mut Context<Self>) {
        let Some(kind) = self.tab.kind() else { return };
        if self.exporting {
            return;
        }
        let dir = std::env::var_os("USERPROFILE")
            .map(|h| PathBuf::from(h).join("Downloads"))
            .filter(|d| d.is_dir())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let name = format!("familiar-{kind}-{}.csv", chrono::Local::now().format("%Y-%m-%d"));
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
        self.exporting = true;
        cx.notify();
        let client = self.client(cx);
        cx.spawn(async move |this, cx| {
            let path = match rx.await {
                Ok(Ok(Some(p))) => p,
                _ => {
                    let _ = this.update(cx, |p, cx| {
                        p.exporting = false;
                        cx.notify()
                    });
                    return;
                }
            };
            let target = path.clone();
            let task = cx.update(|cx| {
                Tokio::spawn(cx, async move {
                    let csv = client.crm_export_csv(kind).await.map_err(|e| e.message())?;
                    let rows = csv.lines().count().saturating_sub(1);
                    std::fs::write(&target, csv).map_err(|e| format!("Couldn't write the file: {e}"))?;
                    Ok::<_, String>(rows)
                })
            });
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r);
            let _ = this.update(cx, |p, cx| {
                p.exporting = false;
                let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                match r {
                    Ok(_) => p.toast(Tone::Ok, format!("Saved {file}"), Some(path.display().to_string()), cx),
                    Err(e) => p.toast(Tone::Bad, "Couldn't export", Some(e), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Pick a CSV file, then show what importing it would do.
    fn start_import(&mut self, cx: &mut Context<Self>) {
        let kind = self.tab.kind().unwrap_or("companies");
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Import".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = rx.await else { return };
            let Some(path) = paths.pop() else { return };
            let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            let read = cx.update(|cx| {
                Tokio::spawn(cx, async move {
                    let len = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
                    if len > 5 * 1024 * 1024 {
                        return Err("The file is over 5 MB. Split it into smaller files.".to_owned());
                    }
                    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                    String::from_utf8(bytes).map_err(|_| "The file isn't UTF-8 text. Save it as “CSV UTF-8” and try again.".to_owned())
                })
            });
            let r = read.await.map_err(|e| e.to_string()).and_then(|r| r);
            let _ = this.update(cx, |p, cx| match r {
                Ok(csv) => p.preview_import(file, csv, kind, cx),
                Err(e) => p.toast(Tone::Bad, "Couldn't read that file", Some(e), cx),
            });
        })
        .detach();
    }

    /// Show what importing `csv` as `kind` would do (a dry run; nothing changes until Import).
    pub fn preview_import(&mut self, file: String, csv: String, kind: &'static str, cx: &mut Context<Self>) {
        let shape = model::csv_shape(&csv);
        self.import = Some(Import { kind, file, csv, shape, preview: None, done: None, busy: false, error: None });
        self.dry_run(cx);
    }

    /// The deal shown at (column, card) on the board.
    pub fn board_card(&self, col: usize, row: usize) -> Option<Uuid> {
        let q = self.q.clone();
        let stage = self.pipeline.as_ref()?.get(col)?;
        stage
            .deals
            .iter()
            .filter(|d| model::matches(&q, &[Some(&d.title), d.company_name.as_deref(), d.contact_name.as_deref(), d.next_step.as_deref()]))
            .nth(row)
            .map(|d| d.id)
    }

    /// The board's stage menu for a deal, as M opens it.
    pub fn open_move_menu(&mut self, deal: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let at = self.board_ids.iter().enumerate().find_map(|(c, ids)| ids.iter().position(|d| *d == deal).map(|r| (c, r)));
        self.cursor = at;
        self.board_focus.focus(window, cx);
        self.open_menu(MenuId::Move(deal), window, cx);
    }

    fn dry_run(&mut self, cx: &mut Context<Self>) {
        self.run_import(true, cx);
    }

    fn run_import(&mut self, dry: bool, cx: &mut Context<Self>) {
        let Some(imp) = self.import.as_mut().filter(|i| !i.busy) else { return };
        imp.busy = true;
        imp.error = None;
        if dry {
            imp.preview = None;
        }
        let (kind, csv) = (imp.kind, imp.csv.clone());
        cx.notify();
        let client = self.client(cx);
        let task = Tokio::spawn(cx, async move { client.crm_import(kind, &csv, dry).await });
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                let Some(imp) = p.import.as_mut().filter(|i| i.kind == kind) else { return };
                imp.busy = false;
                match r {
                    Ok(res) if dry => imp.preview = Some(res),
                    Ok(res) => {
                        let msg = format!("Imported: {} new, {} updated", res.created, res.updated);
                        imp.done = Some(res);
                        p.toast(Tone::Ok, msg, None, cx);
                        p.notice(None, cx);
                    }
                    Err(e) => imp.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn import_dialog(&self, imp: &Import, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let kinds = [("companies", "Companies"), ("contacts", "Contacts"), ("deals", "Deals")];
        let at = kinds.iter().position(|(k, _)| *k == imp.kind).unwrap_or(0);
        let (used, ignored, missing) = model::csv_fit(imp.kind, &imp.shape.columns);
        let this = cx.entity();
        let kind_picker = Segmented::new("crm-imp-kind", kinds.iter().map(|(_, l)| ((*l).into(), None)).collect(), at)
            .segment_width(96.0)
            .on_select(move |i, _, cx| {
                this.update(cx, |p, cx| {
                    if let Some(imp) = p.import.as_mut().filter(|x| !x.busy && x.done.is_none()) {
                        imp.kind = kinds[i].0;
                        p.dry_run(cx);
                    }
                })
            });
        let stat = |n: u32, label: &'static str, tone: Tone| {
            let (fg, bg) = theme.tone(tone);
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .p(px(12.0))
                .rounded(px(RADIUS_CONTROL))
                .bg(if n > 0 { bg } else { theme.sunken })
                .child(
                    div()
                        .text_size(px(text::HEADLINE))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if n > 0 { fg } else { theme.muted })
                        .child(model::thousands(n as u64)),
                )
                .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(label))
        };
        let result = imp.done.as_ref().or(imp.preview.as_ref());
        let mut body = div().flex().flex_col().gap(px(16.0));
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(div().flex().items_center().gap(px(10.0)).child(div().text_size(px(text::SMALL)).text_color(theme.muted).child("It holds")).child(kind_picker))
                .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(format!(
                    "{} row{} · uses {}{}",
                    model::thousands(imp.shape.rows as u64),
                    if imp.shape.rows == 1 { "" } else { "s" },
                    if used.is_empty() { "no known columns".to_owned() } else { used.join(", ") },
                    if ignored.is_empty() { String::new() } else { format!(" · ignores {}", ignored.join(", ")) },
                ))),
        );
        if let Some(m) = missing {
            body = body.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(format!(
                "The first row must be a header row with {m}. The columns it reads: {}.",
                model::csv_columns(imp.kind).join(", ")
            )));
        }
        match result {
            Some(r) => {
                body = body.child(
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(stat(r.created, if imp.done.is_some() { "added" } else { "new" }, Tone::Ok))
                        .child(stat(r.updated, "updated", Tone::Accent))
                        .child(stat(r.skipped, "skipped", Tone::Muted))
                        .child(stat(r.errors.len() as u32, "problems", Tone::Bad)),
                );
                if !r.errors.is_empty() {
                    let mut list = div()
                        .id("crm-imp-errors")
                        .flex()
                        .flex_col()
                        .max_h(px(160.0))
                        .overflow_y_scroll()
                        .rounded(px(RADIUS_CONTROL))
                        .border_1()
                        .border_color(theme.line);
                    for e in r.errors.iter().take(200) {
                        list = list.child(
                            div()
                                .flex()
                                .gap(px(10.0))
                                .px(px(12.0))
                                .py(px(6.0))
                                .border_b_1()
                                .border_color(theme.line.opacity(0.5))
                                .text_size(px(text::CAPTION))
                                .child(div().w(px(56.0)).flex_none().text_color(theme.muted).child(format!("Row {}", e.row)))
                                .child(div().flex_1().min_w_0().text_color(theme.ink).child(e.message.clone())),
                        );
                    }
                    body = body.child(list).child(
                        div()
                            .text_size(px(text::CAPTION))
                            .text_color(theme.muted)
                            .child("Rows with problems are left out; the others import. Row 1 is the header."),
                    );
                }
                if imp.done.is_none() && r.skipped > 0 {
                    body = body.child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(
                        "Skipped rows are blank, or match a record that already has everything in them.",
                    ));
                }
            }
            None if imp.busy => {
                body = body.child(div().flex().gap(px(8.0)).children((0..4).map(|_| div().flex_1().child(Skeleton::new(62.0).radius(RADIUS_CONTROL)))));
            }
            None => {}
        }
        if let Some(e) = imp.error.clone() {
            body = body.child(div().text_size(px(text::SMALL)).text_color(theme.bad).child(e));
        }
        let (cancel, go) = (cx.entity(), cx.entity());
        let would = imp.preview.as_ref().map(|r| r.created + r.updated).unwrap_or(0);
        let actions = match imp.done {
            Some(_) => div().flex().justify_end().child(Button::new("crm-imp-done", "Done").primary().on_click(move |_, _, cx| {
                cancel.update(cx, |p, cx| {
                    p.import = None;
                    cx.notify()
                })
            })),
            None => div()
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(Button::new("crm-imp-cancel", "Cancel").ghost().on_click(move |_, _, cx| {
                    cancel.update(cx, |p, cx| {
                        p.import = None;
                        cx.notify()
                    })
                }))
                .child(
                    Button::new(
                        "crm-imp-go",
                        if imp.busy && imp.preview.is_some() { "Importing…".to_owned() } else { format!("Import {} row{}", model::thousands(would as u64), if would == 1 { "" } else { "s" }) },
                    )
                    .primary()
                    .icon(icons::UPLOAD)
                    .disabled(imp.busy || would == 0)
                    .on_click(move |_, _, cx| go.update(cx, |p, cx| p.run_import(false, cx))),
                ),
        };
        let title = match (&imp.done, imp.busy) {
            (Some(_), _) => "Imported".to_owned(),
            (None, true) if imp.preview.is_none() => "Checking the file…".to_owned(),
            _ => "Here's what the import would do".to_owned(),
        };
        dialog(
            "crm-import",
            div()
                .w(px(560.0))
                .flex()
                .flex_col()
                .gap(px(18.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(div().text_size(px(text::TITLE)).font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_size(px(text::SMALL)).text_color(theme.muted).truncate().child(format!(
                            "{} · nothing changes until you import",
                            imp.file
                        ))),
                )
                .child(body)
                .child(actions),
            cx,
        )
    }

    // ---- page parts ---------------------------------------------------------------------------------------------

    fn failed(&self, title: &'static str, e: String, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.0))
            .py(px(48.0))
            .child(div().font_weight(FontWeight::MEDIUM).child(title))
            .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child(e))
            .child(Button::new("crm-retry", "Try again").size(ButtonSize::Small).icon(icons::REFRESH).on_click(move |_, _, cx| {
                this.update(cx, |p, cx| {
                    p.companies.key = None;
                    p.contacts.key = None;
                    p.deals.key = None;
                    p.activities.loaded = false;
                    p.load_pipeline(cx);
                    p.reload(Which::Companies, cx);
                    p.reload(Which::Contacts, cx);
                    p.ensure(cx);
                })
            }))
            .into_any_element()
    }

    /// Nothing matches the search or filters.
    fn no_match(&self, what: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.0))
            .py(px(48.0))
            .child(div().font_weight(FontWeight::MEDIUM).child(format!("No {what} match")))
            .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child("Try other words, or clear the filters."))
            .child(
                Button::new("crm-clear", "Clear filters")
                    .size(ButtonSize::Small)
                    .on_click(move |_, window, cx| this.update(cx, |p, cx| p.clear_filters(window, cx))),
            )
            .into_any_element()
    }

    fn none_yet(&self, title: &'static str, hint: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.0))
            .py(px(56.0))
            .child(div().font_weight(FontWeight::MEDIUM).child(title))
            .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child(hint))
            .into_any_element()
    }

    fn skeleton_rows() -> AnyElement {
        card_free()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .children((0..6).map(|i| Skeleton::new(40.0).width(if i % 2 == 0 { 620.0 } else { 560.0 }).radius(RADIUS_CONTROL)))
            .into_any_element()
    }

    /// The whole CRM is empty: what it is, and the two ways to fill it.
    fn empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let crew = [("e1", 2u32, 0x4fa9cf), ("e2", 1, 0x7285d5), ("e3", 3, 0xeda84b)];
        let mut faces = div().flex().items_end().gap(px(4.0));
        for (i, (key, shape, color)) in crew.iter().enumerate() {
            let mut a = familiar_ui::mascot::default_avatar(key);
            a.shape = *shape as u8;
            a.color = *color;
            faces = faces.child(Mascot::new(format!("crm-empty-{key}"), a, MascotState::Idle, if i == 1 { 76.0 } else { 60.0 }));
        }
        let (hire, import) = (cx.entity(), cx.entity());
        anim::appear(
            "crm-empty",
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(12.0))
                .py(px(56.0))
                .child(faces)
                .child(div().text_size(px(text::TITLE)).font_weight(FontWeight::SEMIBOLD).child("Your CRM is empty"))
                .child(div().max_w(px(460.0)).text_center().text_color(theme.muted).child(
                    "The companies, people and deals your teammates find and talk to land here, with every change tracked and undoable.",
                ))
                .child(
                    div()
                        .flex()
                        .gap(px(10.0))
                        .pt(px(8.0))
                        .child(
                            Button::new("crm-hire", "Hire the GTM crew")
                                .primary()
                                .icon(icons::PLUS)
                                .on_click(move |_, _, cx| hire.update(cx, |_, cx| cx.emit(CrmEvent::HireCrew))),
                        )
                        .child(
                            Button::new("crm-empty-import", "Import a CSV")
                                .icon(icons::UPLOAD)
                                .on_click(move |_, _, cx| import.update(cx, |p, cx| p.start_import(cx))),
                        ),
                )
                .child(div().max_w(px(520.0)).text_center().text_size(px(text::CAPTION)).text_color(theme.muted).child(
                    "Five teammates: one finds leads, one drafts first emails, one tracks replies, one reports weekly, one triages your inbox. Nothing goes out without your OK.",
                )),
        )
        .into_any_element()
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let deals: Option<(i64, i64)> = self.pipeline.as_ref().map(|s| {
            let open = s.iter().filter(|s| !matches!(s.stage, DealStage::Won | DealStage::Lost));
            (s.iter().map(|s| s.count).sum(), open.map(|s| s.value_cents).sum())
        });
        let currency = self
            .pipeline
            .iter()
            .flatten()
            .flat_map(|s| s.deals.first())
            .map(|d| d.currency.clone())
            .next()
            .unwrap_or_else(|| "USD".into());
        let plural = |n: usize, one: &str, many: &str| format!("{} {}", model::thousands(n as u64), if n == 1 { one } else { many });
        let mut counts = div().flex().items_center().gap(px(6.0));
        if let Some(n) = self.company_total {
            counts = counts.child(chip(Tone::Muted, plural(n, "company", "companies"), cx));
        }
        if let Some(n) = self.contact_total {
            counts = counts.child(chip(Tone::Muted, plural(n, "contact", "contacts"), cx));
        }
        if let Some((n, open)) = deals {
            counts = counts.child(chip(Tone::Muted, plural(n.max(0) as usize, "deal", "deals"), cx));
            if open > 0 {
                counts = counts.child(chip(Tone::Accent, format!("{} open", model::money_short(open, &currency)), cx));
            }
        }
        let kind = self.tab.kind();
        let (imp, exp) = (cx.entity(), cx.entity());
        div()
            .flex()
            .items_start()
            .gap(px(16.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).text_color(theme.ink).child("CRM"))
                            .child(counts),
                    )
                    .child(div().text_size(px(text::LEAD)).text_color(theme.muted).truncate().child(
                        "Who your teammates find and talk to. Every change is tracked; yours always win.",
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(8.0))
                    .pt(px(2.0))
                    .child(
                        Button::new("crm-import", "Import")
                            .size(ButtonSize::Small)
                            .icon(icons::UPLOAD)
                            .disabled(kind.is_none())
                            .tooltip("Add or update records from a CSV file. You see what it would do first.")
                            .on_click(move |_, _, cx| imp.update(cx, |p, cx| p.start_import(cx))),
                    )
                    .child(
                        Button::new("crm-export", if self.exporting { "Exporting…" } else { "Export" })
                            .size(ButtonSize::Small)
                            .icon(icons::DOWNLOAD)
                            .disabled(kind.is_none() || self.exporting)
                            .tooltip(match kind {
                                Some(k) => format!("Save every {} as a CSV file", k.trim_end_matches('s').replace("companie", "company")),
                                None => "Pick Companies, Contacts or Pipeline to export".to_owned(),
                            })
                            .on_click(move |_, _, cx| exp.update(cx, |p, cx| p.export(cx))),
                    ),
            )
            .into_any_element()
    }

    fn toolbar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let at = TABS.iter().position(|(t, _, _)| *t == self.tab).unwrap_or(0);
        let this = cx.entity();
        let tabs = Segmented::new("crm-tabs", TABS.iter().map(|(_, l, g)| ((*l).into(), Some(*g))).collect(), at)
            .segment_width(106.0)
            .on_select(move |i, _, cx| this.update(cx, |p, cx| p.set_tab(TABS[i].0, cx)));
        // The tabs (and the board / list switch) on one row, the search and filters under them.
        let mut tab_row = div().flex().items_center().gap(px(8.0)).child(tabs).child(div().flex_1());
        let mut bar = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(div().w(px(300.0)).flex_shrink_0().child(text_input::compact_field("crm-search", &self.search, icons::MAGNIFER, window, cx)));
        let tag_label = |t: &Option<String>| match t {
            Some(t) => format!("Tag: {t}"),
            None => "Tag".to_owned(),
        };
        let sort_label = |k: &str| match k {
            "name" => "A to Z",
            "fit" => "Best fit first",
            _ => "Recently changed",
        };
        match self.tab {
            Tab::Companies => {
                bar = bar
                    .child(self.select(MenuId::Tag, tag_label(&self.tag), self.tag.is_some(), 220.0, cx))
                    .child(self.select(MenuId::Sort, sort_label(self.sort_companies).into(), false, 200.0, cx));
            }
            Tab::Contacts => {
                let dnc = match self.dnc {
                    None => "Anyone",
                    Some(false) => "OK to contact",
                    Some(true) => "Do not contact",
                };
                bar = bar
                    .child(self.select(MenuId::Tag, tag_label(&self.tag), self.tag.is_some(), 220.0, cx))
                    .child(self.select(MenuId::Dnc, dnc.into(), self.dnc.is_some(), 200.0, cx))
                    .child(self.select(MenuId::Sort, sort_label(self.sort_contacts).into(), false, 200.0, cx));
            }
            Tab::Pipeline => {
                let this = cx.entity();
                tab_row = tab_row.child(
                    Segmented::new("crm-deal-view", vec![("Board".into(), Some(icons::WIDGET)), ("List".into(), Some(icons::LIST))], self.deal_list as usize)
                        .segment_width(84.0)
                        .on_select(move |i, _, cx| this.update(cx, |p, cx| p.set_deal_list(i == 1, cx))),
                );
                if self.deal_list {
                    let stage = self.stage.map(|s| format!("Stage: {}", model::stage_label(s))).unwrap_or_else(|| "Stage".into());
                    bar = bar
                        .child(self.select(MenuId::Stage, stage, self.stage.is_some(), 200.0, cx))
                        .child(self.select(MenuId::Sort, sort_label(self.sort_deals).into(), false, 200.0, cx));
                }
            }
            Tab::Activity => {}
        }
        let status = match self.tab {
            Tab::Companies => list_status(&self.companies, "company", "companies"),
            Tab::Contacts => list_status(&self.contacts, "contact", "contacts"),
            Tab::Pipeline if self.deal_list => list_status(&self.deals, "deal", "deals"),
            _ => None,
        };
        let bar = bar.child(div().flex_1()).children(status.map(|s| div().flex_none().text_size(px(text::CAPTION)).text_color(theme.muted).child(s)));
        div().flex().flex_col().gap(px(12.0)).child(tab_row).child(bar).into_any_element()
    }

    fn content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (wide, mid) = Self::widths(window);
        match self.tab {
            Tab::Companies => {
                let l = &self.companies;
                if let (Some(e), true) = (l.error.clone(), l.rows.is_empty()) {
                    return self.failed("Couldn't load companies", e, cx);
                }
                if !l.loaded {
                    return Self::skeleton_rows();
                }
                if l.rows.is_empty() {
                    return if self.filtered(Which::Companies) {
                        self.no_match("companies", cx)
                    } else {
                        self.none_yet("No companies yet", "Teammates add the companies they research; you can import a CSV too.", cx)
                    };
                }
                let mut head = vec![("Company", None, false), ("Fit", Some(110.0), false)];
                if wide {
                    head.push(("Tags", Some(180.0), false));
                }
                if mid {
                    head.push(("Added by", Some(150.0), false));
                }
                head.push(("Updated", Some(84.0), true));
                let scroll = self.companies.scroll.clone();
                self.table("crm-companies", head, self.companies.rows.len(), &scroll, Self::company_rows, cx)
            }
            Tab::Contacts => {
                let l = &self.contacts;
                if let (Some(e), true) = (l.error.clone(), l.rows.is_empty()) {
                    return self.failed("Couldn't load contacts", e, cx);
                }
                if !l.loaded {
                    return Self::skeleton_rows();
                }
                if l.rows.is_empty() {
                    return if self.filtered(Which::Contacts) {
                        self.no_match("contacts", cx)
                    } else {
                        self.none_yet("No contacts yet", "People at the companies your teammates research show up here.", cx)
                    };
                }
                let mut head = vec![("Name", None, false)];
                if mid {
                    head.push(("Company", Some(170.0), false));
                }
                if wide {
                    head.push(("Email", Some(220.0), false));
                }
                head.push(("", Some(128.0), false));
                head.push(("Updated", Some(84.0), true));
                let scroll = self.contacts.scroll.clone();
                self.table("crm-contacts", head, self.contacts.rows.len(), &scroll, Self::contact_rows, cx)
            }
            Tab::Pipeline if !self.deal_list => self.board(window, cx),
            Tab::Pipeline => {
                let l = &self.deals;
                if let (Some(e), true) = (l.error.clone(), l.rows.is_empty()) {
                    return self.failed("Couldn't load deals", e, cx);
                }
                if !l.loaded {
                    return Self::skeleton_rows();
                }
                if l.rows.is_empty() {
                    return if self.filtered(Which::Deals) {
                        self.no_match("deals", cx)
                    } else {
                        self.none_yet("No deals yet", "A deal is a company you're working on: teammates open one per lead.", cx)
                    };
                }
                let mut head = vec![("Deal", None, false), ("Stage", Some(110.0), false), ("Value", Some(96.0), true)];
                if mid {
                    head.push(("Next step", Some(if wide { 240.0 } else { 180.0 }), false));
                }
                head.push(("Updated", Some(84.0), true));
                let scroll = self.deals.scroll.clone();
                self.table("crm-deals", head, self.deals.rows.len(), &scroll, Self::deal_rows, cx)
            }
            Tab::Activity => {
                let a = &self.activities;
                if let (Some(e), true) = (a.error.clone(), a.rows.is_empty()) {
                    return self.failed("Couldn't load the activity", e, cx);
                }
                if !a.loaded {
                    return Self::skeleton_rows();
                }
                let shown = self.shown_activities().len();
                if shown == 0 {
                    return if self.q.is_empty() {
                        self.none_yet("Nothing has happened yet", "Research, emails sent and received, calls and stage moves show up here, newest first.", cx)
                    } else {
                        self.no_match("activities", cx)
                    };
                }
                let mut head = vec![("When", Some(80.0), false), ("Who", Some(170.0), false), ("What", None, false)];
                if mid {
                    head.push(("About", Some(200.0), false));
                }
                let scroll = self.activities.scroll.clone();
                self.table("crm-activity", head, shown, &scroll, Self::activity_rows, cx)
            }
        }
    }

    fn undo_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let u = self.undo.as_ref()?;
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        let close = cx.entity();
        Some(
            div()
                .absolute()
                .bottom(px(24.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(anim::appear(
                    "crm-undo",
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.0))
                        .pl(px(16.0))
                        .pr(px(6.0))
                        .py(px(6.0))
                        .rounded(px(RADIUS_DIALOG))
                        .bg(theme.tooltip_bg)
                        .text_color(theme.tooltip_ink)
                        .shadow(theme.float_shadow())
                        .text_size(px(text::SMALL))
                        .child(format!("Deleted “{}”", excerpt(&u.name, 48)))
                        .child(
                            div()
                                .id("crm-undo-btn")
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .px(px(10.0))
                                .py(px(5.0))
                                .rounded(px(RADIUS_CONTROL - 2.0))
                                .cursor_pointer()
                                .font_weight(FontWeight::SEMIBOLD)
                                .hover(|s| s.bg(theme.tooltip_ink.opacity(0.12)))
                                .on_click(move |_, window, cx| this.update(cx, |p, cx| p.undo_delete(window, cx)))
                                .child(icon(icons::UNDO).size(px(14.0)).text_color(theme.tooltip_ink))
                                .child(if u.busy { "Undoing…" } else { "Undo" }),
                        )
                        .child(
                            div()
                                .id("crm-undo-close")
                                .p(px(5.0))
                                .rounded(px(RADIUS_CONTROL - 2.0))
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.tooltip_ink.opacity(0.12)))
                                .on_click(move |_, _, cx| {
                                    close.update(cx, |p, cx| {
                                        p.undo = None;
                                        cx.notify()
                                    })
                                })
                                .child(icon(icons::CLOSE).size(px(14.0)).text_color(theme.tooltip_ink.opacity(0.7))),
                        ),
                ))
                .into_any_element(),
        )
    }
}

impl Render for CrmPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::count("CrmPage");
        let theme = Theme::of(cx).clone();
        let empty = self.is_empty() && self.q.is_empty();
        let main = if empty {
            div()
                .flex()
                .flex_col()
                .gap(px(24.0))
                .child(div().text_size(px(text::HEADLINE)).font_weight(FontWeight::SEMIBOLD).child("CRM"))
                .child(self.empty_state(cx))
                .into_any_element()
        } else {
            let header = self.header(cx);
            let toolbar = self.toolbar(window, cx);
            let content = self.content(window, cx);
            div()
                .size_full()
                .flex()
                .flex_col()
                .gap(px(18.0))
                .child(anim::appear("crm-head", div().child(header)))
                .child(toolbar)
                .child(div().flex_1().min_h_0().flex().flex_col().child(content))
                .into_any_element()
        };
        let panel_w = (f32::from(window.viewport_size().width) - SIDEBAR_WIDTH - 80.0).clamp(320.0, 460.0);
        let panel = self.panel.clone().map(|p| {
            div()
                .absolute()
                .top(px(12.0))
                .bottom(px(12.0))
                .right(px(12.0))
                .w(px(panel_w))
                .child(
                    anim::appear(
                        SharedString::from(format!("crm-panel-{}", p.read(cx).rec().id())),
                        div()
                            .size_full()
                            .rounded(px(RADIUS_DIALOG))
                            .border_1()
                            .border_color(theme.line)
                            .bg(theme.surface)
                            .shadow(theme.float_shadow())
                            .overflow_hidden()
                            .child(p),
                    ),
                )
        });
        let import = self.import.as_ref().map(|i| self.import_dialog(i, cx));
        let undo = self.undo_bar(cx);
        div()
            .id("crm-page")
            .relative()
            .size_full()
            .bg(theme.bg)
            .child(div().size_full().px(px(32.0)).pt(px(28.0)).pb(px(20.0)).child(main))
            .children(panel)
            .children(import)
            .children(undo)
    }
}

// ---- small pieces -----------------------------------------------------------------------------------------------

/// A table cell: a fixed width (or the rest of the row), left or right aligned.
fn cell(width: Option<f32>, right: bool) -> gpui::Div {
    let c = div().flex().items_center().min_w_0().when(right, |el| el.justify_end());
    match width {
        Some(w) => c.w(px(w)).flex_none(),
        None => c.flex_1(),
    }
}

fn dash(theme: &Theme) -> AnyElement {
    div().text_size(px(text::CAPTION)).text_color(theme.muted.opacity(0.6)).child("—").into_any_element()
}

fn updated(t: chrono::DateTime<Utc>, theme: &Theme) -> gpui::Div {
    div().text_size(px(text::CAPTION)).text_color(theme.muted).child(ago(Some(t)))
}

/// A monogram: a rounded tile for a company, a circle for a person.
fn monogram(name: &str, person: bool, theme: &Theme) -> gpui::Div {
    div()
        .size(px(30.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .when(person, |el| el.rounded_full())
        .when(!person, |el| el.rounded(px(8.0)))
        .bg(if person { theme.accent_soft } else { theme.sunken })
        .border_1()
        .border_color(theme.line.opacity(0.6))
        .text_size(px(text::MICRO))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(if person { theme.accent } else { theme.muted })
        .child(model::initials(name))
}

/// "You" as a byline: a small filled circle.
fn you_dot(theme: &Theme) -> gpui::Div {
    div()
        .size(px(20.0))
        .flex_none()
        .rounded_full()
        .bg(theme.accent_soft)
        .flex()
        .items_center()
        .justify_center()
        .child(icon(icons::USER).size(px(12.0)).text_color(theme.accent))
}

/// The do-not-contact mark: impossible to miss, never alarming.
pub fn dnc_badge(cx: &App) -> gpui::Div {
    let theme = Theme::of(cx);
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.0))
        .h(px(20.0))
        .px(px(7.0))
        .rounded(px(RADIUS_CHIP))
        .bg(theme.bad_soft)
        .text_color(theme.bad)
        .text_size(px(text::CAPTION))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .child(icon(icons::BLOCK).size(px(12.0)).text_color(theme.bad))
        .child("Do not contact")
}

/// Up to `n` tag chips, then "+3".
pub fn tag_chips(tags: &[String], n: usize, cx: &App) -> gpui::Div {
    let theme = Theme::of(cx).clone();
    let mut row = div().flex().items_center().gap(px(4.0)).min_w_0().overflow_hidden();
    for t in tags.iter().take(n) {
        row = row.child(chip(Tone::Muted, excerpt(t, 18), cx));
    }
    if tags.len() > n {
        row = row.child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(format!("+{}", tags.len() - n)));
    }
    row
}

/// The toolbar's "312 companies" (or how far the loading is).
fn list_status<T>(l: &Listing<T>, one: &str, many: &str) -> Option<String> {
    if !l.loaded {
        return None;
    }
    let n = l.rows.len();
    let words = format!("{} {}", model::thousands(n as u64), if n == 1 { one } else { many });
    if !l.complete && l.loading {
        Some(format!("{words} so far…"))
    } else if n >= MAX_ROWS {
        Some(format!("The first {words}"))
    } else if !l.unfiltered {
        Some(format!("{} found", model::thousands(n as u64)))
    } else {
        // The header has the total.
        None
    }
}

/// A plain block for skeletons (no card chrome).
fn card_free() -> gpui::Div {
    div()
}

/// A centred dialog over a soft scrim.
pub fn dialog(id: &'static str, content: impl IntoElement, cx: &App) -> AnyElement {
    let theme = Theme::of(cx).clone();
    div()
        .id(id)
        .absolute()
        .inset_0()
        .occlude()
        .bg(theme.ink.opacity(if theme.is_dark() { 0.45 } else { 0.18 }))
        .flex()
        .items_center()
        .justify_center()
        .child(anim::appear(
            SharedString::from(format!("{id}-in")),
            div()
                .p(px(24.0))
                .rounded(px(RADIUS_DIALOG))
                .border_1()
                .border_color(theme.line)
                .bg(theme.surface)
                .shadow(theme.float_shadow())
                .child(content),
        ))
        .into_any_element()
}
