//! A CRM record in the side panel: every field (click one to edit it; your edit goes through `PATCH` and wins over a
//! teammate's), a company's people and deals, a contact's deals, a deal's stage (a menu), the activity timeline (an
//! entry from a draft says so, and a draft still waiting opens in Needs you), do-not-contact with its reason and who
//! set it, "Changed by <teammate> · Undo" from the change log, and delete with a confirmation (the page then offers
//! Undo). It keeps its own data and reloads on the stream's `crm_*` notices.

use chrono::{Local, Utc};
use familiar_client::{
    ActivityKind, CompanyPatch, ContactPatch, CrmActivity, CrmActivityParams, CrmChange, CrmChangeParams, CrmCompany,
    CrmContact, CrmDeal, CrmListParams, DealPatch, DealStage, NewActivity,
};
use familiar_ui::components::{Button, ButtonSize, Skeleton, Switch, chip};
use familiar_ui::icons::{self, icon};
use familiar_ui::mascot::{Mascot, MascotState};
use familiar_ui::theme::{RADIUS_CONTROL, Theme, Tone, text};
use familiar_ui::toast::ToastStack;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, FontWeight, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, Task, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_base::input::{InputEvent, InputState, TextareaState};
use gpui_tokio::Tokio;
use uuid::Uuid;

use crate::crm::{dnc_badge, tag_chips};
use crate::crm_model::{self as model, STAGES};
use crate::data::{AppData, DataEvent, Part, ago, avatar_of, excerpt};
use crate::menu::{self, MenuItem};
use crate::text_input;

/// A record the panel can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rec {
    Company(Uuid),
    Contact(Uuid),
    Deal(Uuid),
}

impl Rec {
    pub fn id(&self) -> Uuid {
        match self {
            Rec::Company(id) | Rec::Contact(id) | Rec::Deal(id) => *id,
        }
    }
    /// The change log's name for it.
    pub fn entity(&self) -> &'static str {
        match self {
            Rec::Company(_) => "company",
            Rec::Contact(_) => "contact",
            Rec::Deal(_) => "deal",
        }
    }
    /// Its table (as change notices name it).
    pub fn table(&self) -> &'static str {
        match self {
            Rec::Company(_) => "crm_companies",
            Rec::Contact(_) => "crm_contacts",
            Rec::Deal(_) => "crm_deals",
        }
    }
    pub fn glyph(&self) -> &'static str {
        match self {
            Rec::Company(_) => icons::BUILDINGS,
            Rec::Contact(_) => icons::USER,
            Rec::Deal(_) => icons::CASE,
        }
    }
    fn noun(&self) -> &'static str {
        match self {
            Rec::Company(_) => "Company",
            Rec::Contact(_) => "Contact",
            Rec::Deal(_) => "Deal",
        }
    }
}

pub enum PanelEvent {
    Close,
    /// Back to the record opened before this one.
    Back,
    Open(Rec),
    /// It was deleted (the page offers Undo).
    Deleted { rec: Rec, name: String },
    /// A draft from its timeline is waiting: show Needs you.
    OpenNeeds,
    /// The owner changed it here.
    Saved(Rec),
    /// Undoing how it was added took it out of the CRM (an undo can't itself be undone).
    Removed { rec: Rec, name: String },
}

/// The pieces of the panel that are read from the API (the change log goes with the record).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Parts {
    pub record: bool,
    pub contacts: bool,
    pub deals: bool,
    pub activities: bool,
}

impl Parts {
    pub const NONE: Parts = Parts { record: false, contacts: false, deals: false, activities: false };
    pub const ALL: Parts = Parts { record: true, contacts: true, deals: true, activities: true };
    fn is_empty(self) -> bool {
        self == Self::NONE
    }
    fn or(self, o: Parts) -> Parts {
        Parts {
            record: self.record || o.record,
            contacts: self.contacts || o.contacts,
            deals: self.deals || o.deals,
            activities: self.activities || o.activities,
        }
    }
}

/// What an open panel shows: the record, the company and contact it links to, the people and deals it lists.
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    pub rec: Option<Rec>,
    pub company: Option<Uuid>,
    pub contact: Option<Uuid>,
    pub contacts: Vec<Uuid>,
    pub deals: Vec<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Contact,
    Deal,
}

/// What a change notice means for an open panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Nothing,
    Parts(Parts),
    /// A contact or deal the panel doesn't list: read it to see whether it belongs here.
    Check(Kind, Uuid),
}

/// Which parts of a panel a change notice (`table`, the row's id) touches. Rows of other records are ignored; the
/// timeline's notices name only the entry, so any re-reads the timeline.
pub fn scope(ctx: &Ctx, table: &str, id: Option<Uuid>) -> Scope {
    let Some(rec) = ctx.rec else { return Scope::Nothing };
    let record = Scope::Parts(Parts { record: true, ..Parts::NONE });
    if table == "crm_activities" {
        return Scope::Parts(Parts { activities: true, ..Parts::NONE });
    }
    if !matches!(table, "crm_companies" | "crm_contacts" | "crm_deals") {
        return Scope::Nothing;
    }
    let Some(id) = id else { return Scope::Parts(Parts::ALL) };
    match (rec, table) {
        (r, _) if r.id() == id && r.table() == table => record,
        // A contact or deal shows its company's name; a deal its contact's.
        (Rec::Contact(_) | Rec::Deal(_), "crm_companies") if ctx.company == Some(id) => record,
        (Rec::Deal(_), "crm_contacts") if ctx.contact == Some(id) => record,
        (Rec::Company(_), "crm_contacts") if ctx.contacts.contains(&id) => Scope::Parts(Parts { contacts: true, ..Parts::NONE }),
        (Rec::Company(_), "crm_contacts") => Scope::Check(Kind::Contact, id),
        (Rec::Company(_) | Rec::Contact(_), "crm_deals") if ctx.deals.contains(&id) => {
            Scope::Parts(Parts { deals: true, ..Parts::NONE })
        }
        (Rec::Company(_) | Rec::Contact(_), "crm_deals") => Scope::Check(Kind::Deal, id),
        _ => Scope::Nothing,
    }
}

/// The record itself.
#[derive(Clone)]
enum Record {
    Company(CrmCompany),
    Contact(CrmContact),
    Deal(CrmDeal),
}

impl Record {
    fn name(&self) -> &str {
        match self {
            Record::Company(c) => &c.name,
            Record::Contact(c) => &c.name,
            Record::Deal(d) => &d.title,
        }
    }
}

/// A field the owner can edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Domain,
    Website,
    Industry,
    Size,
    Location,
    Description,
    FitScore,
    FitReason,
    Tags,
    Title,
    Email,
    Linkedin,
    XHandle,
    Notes,
    Value,
    Currency,
    NextStep,
    NextStepAt,
}

impl Field {
    fn label(self) -> &'static str {
        match self {
            Field::Name => "Name",
            Field::Domain => "Domain",
            Field::Website => "Website",
            Field::Industry => "Industry",
            Field::Size => "Size",
            Field::Location => "Location",
            Field::Description => "About",
            Field::FitScore => "Fit score",
            Field::FitReason => "Why it fits",
            Field::Tags => "Tags",
            Field::Title => "Title",
            Field::Email => "Email",
            Field::Linkedin => "LinkedIn",
            Field::XHandle => "X",
            Field::Notes => "Notes",
            Field::Value => "Value",
            Field::Currency => "Currency",
            Field::NextStep => "Next step",
            Field::NextStepAt => "Due",
        }
    }
    fn multiline(self) -> bool {
        matches!(self, Field::Description | Field::FitReason | Field::Notes | Field::NextStep)
    }
    fn hint(self) -> &'static str {
        match self {
            Field::FitScore => "0 to 100",
            Field::Tags => "Comma separated",
            Field::Value => "12,500 or 12.5k",
            Field::Currency => "USD, EUR, GBP…",
            Field::NextStepAt => "2026-10-20, tomorrow, in 3 days",
            Field::NextStep => "What happens next (clear it to drop the step)",
            Field::Domain => "acme.com",
            Field::Website | Field::Linkedin => "https://…",
            _ => "",
        }
    }
}

const COMPANY_FIELDS: [Field; 8] =
    [Field::Domain, Field::Website, Field::Industry, Field::Size, Field::Location, Field::Description, Field::FitReason, Field::Tags];
const CONTACT_FIELDS: [Field; 6] = [Field::Title, Field::Email, Field::Linkedin, Field::XHandle, Field::Notes, Field::Tags];
const DEAL_FIELDS: [Field; 4] = [Field::Value, Field::Currency, Field::NextStep, Field::NextStepAt];

/// A field's text as the editor starts with it.
fn field_text(r: &Record, f: Field) -> String {
    let s = |o: &Option<String>| o.clone().unwrap_or_default();
    match (r, f) {
        (_, Field::Name) => r.name().to_owned(),
        (Record::Deal(d), Field::Title) => d.title.clone(),
        (Record::Company(c), Field::Domain) => s(&c.domain),
        (Record::Company(c), Field::Website) => s(&c.website),
        (Record::Company(c), Field::Industry) => s(&c.industry),
        (Record::Company(c), Field::Size) => s(&c.size),
        (Record::Company(c), Field::Location) => s(&c.location),
        (Record::Company(c), Field::Description) => s(&c.description),
        (Record::Company(c), Field::FitScore) => c.fit_score.map(|n| n.to_string()).unwrap_or_default(),
        (Record::Company(c), Field::FitReason) => s(&c.fit_reason),
        (Record::Company(c), Field::Tags) => c.tags.join(", "),
        (Record::Contact(c), Field::Title) => s(&c.title),
        (Record::Contact(c), Field::Email) => s(&c.email),
        (Record::Contact(c), Field::Linkedin) => s(&c.linkedin_url),
        (Record::Contact(c), Field::XHandle) => s(&c.x_handle),
        (Record::Contact(c), Field::Notes) => s(&c.notes),
        (Record::Contact(c), Field::Tags) => c.tags.join(", "),
        (Record::Deal(d), Field::Value) => d.value_cents.map(|v| model::money(v, "").trim_start_matches('$').to_owned()).unwrap_or_default(),
        (Record::Deal(d), Field::Currency) => d.currency.clone(),
        (Record::Deal(d), Field::NextStep) => s(&d.next_step),
        (Record::Deal(d), Field::NextStepAt) => d.next_step_at.map(|t| t.with_timezone(&Local).format("%Y-%m-%d").to_string()).unwrap_or_default(),
        _ => String::new(),
    }
}

/// A field as the panel shows it (`None`: not set).
fn field_shown(r: &Record, f: Field) -> Option<String> {
    let t = match (r, f) {
        (Record::Deal(d), Field::Value) => d.value_cents.map(|v| model::money(v, &d.currency)),
        // A date without a step (left from before clearing a step also cleared its date): not shown.
        (Record::Deal(d), Field::NextStepAt) if d.next_step.as_deref().is_none_or(|s| s.trim().is_empty()) => None,
        (Record::Deal(d), Field::NextStepAt) => d.next_step_at.map(|t| {
            let (words, _) = model::due_words(t, Utc::now());
            format!("{} · {words}", t.with_timezone(&Local).format("%a %b %-d"))
        }),
        _ => Some(field_text(r, f)),
    };
    t.filter(|t| !t.trim().is_empty())
}

/// The change an edit of `f` to `text` makes: a patch of that one field (an empty text clears it), or why it can't.
pub enum Patch {
    Company(CompanyPatch),
    Contact(ContactPatch),
    Deal(DealPatch),
}

pub fn patch_for(rec: Rec, f: Field, text: &str) -> Result<Patch, String> {
    let t = text.trim().to_owned();
    let some = || Some(t.clone());
    match rec {
        Rec::Company(_) => {
            let mut p = CompanyPatch::default();
            match f {
                Field::Name if t.is_empty() => return Err("A company needs a name.".into()),
                Field::Name => p.name = some(),
                Field::Domain => p.domain = some(),
                Field::Website => p.website = some(),
                Field::Industry => p.industry = some(),
                Field::Size => p.size = some(),
                Field::Location => p.location = some(),
                Field::Description => p.description = some(),
                Field::FitReason => p.fit_reason = some(),
                Field::Tags => p.tags = Some(model::parse_tags(&t)),
                Field::FitScore if t.is_empty() => p.fit_score = Some(None),
                Field::FitScore => match t.parse::<i32>() {
                    Ok(n) if (0..=100).contains(&n) => p.fit_score = Some(Some(n)),
                    _ => return Err("A fit score is a whole number from 0 to 100.".into()),
                },
                _ => return Err("That can't be changed here.".into()),
            }
            Ok(Patch::Company(p))
        }
        Rec::Contact(_) => {
            let mut p = ContactPatch::default();
            match f {
                Field::Name if t.is_empty() => return Err("A contact needs a name.".into()),
                Field::Name => p.name = some(),
                Field::Title => p.title = some(),
                Field::Email if !t.is_empty() && (t.matches('@').count() != 1 || !t.split('@').nth(1).is_some_and(|d| d.contains('.'))) => {
                    return Err("That doesn't look like an email address.".into());
                }
                Field::Email => p.email = some(),
                Field::Linkedin => p.linkedin_url = some(),
                Field::XHandle => p.x_handle = some(),
                Field::Notes => p.notes = some(),
                Field::Tags => p.tags = Some(model::parse_tags(&t)),
                _ => return Err("That can't be changed here.".into()),
            }
            Ok(Patch::Contact(p))
        }
        Rec::Deal(_) => {
            let mut p = DealPatch::default();
            match f {
                Field::Name | Field::Title if t.is_empty() => return Err("A deal needs a title.".into()),
                Field::Name | Field::Title => p.title = some(),
                Field::Value => match model::parse_money(&t) {
                    Some(Some(c)) => p.value_cents = Some(Some(c)),
                    Some(None) => p.value_cents = Some(None),
                    None => return Err("Write an amount, like 12,500 or 12.5k.".into()),
                },
                Field::Currency if t.len() == 3 && t.chars().all(|c| c.is_ascii_alphabetic()) => p.currency = Some(t.to_uppercase()),
                Field::Currency => return Err("A currency is a 3-letter code, like USD.".into()),
                // Clearing the step clears its date too, so no date is left behind without a step.
                Field::NextStep if t.is_empty() => {
                    p.next_step = some();
                    p.next_step_at = Some(None);
                }
                Field::NextStep => p.next_step = some(),
                Field::NextStepAt => match model::parse_due(&t, Local::now().date_naive()) {
                    Some(Some(at)) => p.next_step_at = Some(Some(at)),
                    Some(None) => p.next_step_at = Some(None),
                    None => return Err("Write a date like 2026-10-20, tomorrow or in 3 days.".into()),
                },
                _ => return Err("That can't be changed here.".into()),
            }
            Ok(Patch::Deal(p))
        }
    }
}

enum Editor {
    Line(Entity<InputState>),
    Area(Entity<TextareaState>),
}

impl Editor {
    fn value(&self, cx: &App) -> String {
        match self {
            Editor::Line(s) => s.read(cx).value().to_string(),
            Editor::Area(s) => s.read(cx).value().to_string(),
        }
    }
}

struct Editing {
    field: Field,
    editor: Editor,
    error: Option<String>,
}

pub struct RecordPanel {
    data: Entity<AppData>,
    toasts: Entity<ToastStack>,
    rec: Rec,
    record: Option<Record>,
    /// It no longer exists (deleted elsewhere).
    gone: bool,
    error: Option<String>,
    /// A company's people, a contact's colleagues are not shown.
    contacts: Option<Vec<CrmContact>>,
    /// A company's deals, or a contact's.
    deals: Option<Vec<CrmDeal>>,
    activities: Option<Vec<CrmActivity>>,
    /// Newest first.
    changes: Option<Vec<CrmChange>>,
    editing: Option<Editing>,
    /// A write in flight (save, undo, do-not-contact, delete, stage).
    busy: bool,
    confirm_delete: bool,
    /// Turning do-not-contact on: the reason box. Turning it off: the confirmation.
    dnc_on: Option<Entity<InputState>>,
    dnc_off: bool,
    note: Entity<InputState>,
    stage_menu: Option<usize>,
    menu_focus: FocusHandle,
    focus: FocusHandle,
    scroll: ScrollHandle,
    loading: Option<Task<()>>,
    /// Parts asked for during a read: read after it.
    queued: Parts,
    /// It was opened from another record: the top bar offers Back.
    back: bool,
}

impl EventEmitter<PanelEvent> for RecordPanel {}

impl RecordPanel {
    pub fn new(data: Entity<AppData>, toasts: Entity<ToastStack>, rec: Rec, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&data, |this: &mut Self, _, ev: &DataEvent, cx| match ev {
            DataEvent::Changed(None) => this.reload(Parts::ALL, cx),
            DataEvent::Changed(Some(n)) if n.t.starts_with("crm_") => {
                let id = n.id.as_deref().and_then(|s| s.parse::<Uuid>().ok());
                this.on_notice(&n.t, id, cx)
            }
            // A draft on the timeline may have been decided; teammates' names.
            DataEvent::Updated(Part::Pending | Part::Overview) => cx.notify(),
            _ => {}
        })
        .detach();
        let note = text_input::new_line("Add a note to the timeline", false, window, cx);
        cx.subscribe_in(&note, window, |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } => this.add_note(window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        })
        .detach();
        let mut this = Self {
            data,
            toasts,
            rec,
            record: None,
            gone: false,
            error: None,
            contacts: None,
            deals: None,
            activities: None,
            changes: None,
            editing: None,
            busy: false,
            confirm_delete: false,
            dnc_on: None,
            dnc_off: false,
            note,
            stage_menu: None,
            menu_focus: cx.focus_handle(),
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            loading: None,
            queued: Parts::NONE,
            back: false,
        };
        this.reload(Parts::ALL, cx);
        this
    }

    pub fn rec(&self) -> Rec {
        self.rec
    }

    pub fn set_back(&mut self, back: bool) {
        self.back = back;
    }

    /// Take the keyboard (Escape closes the panel).
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus.focus(window, cx);
    }

    fn client(&self, cx: &App) -> familiar_client::Client {
        self.data.read(cx).client.clone()
    }

    fn toast(&self, tone: Tone, title: impl Into<SharedString>, body: Option<String>, cx: &mut Context<Self>) {
        let title = title.into();
        self.toasts.update(cx, |t, cx| t.push(tone, title, body.map(Into::into), cx));
    }

    /// Read again the `parts` of the panel (one read at a time: notices during a read gather into one more, run after
    /// a breath, since a teammate may be writing a lot).
    fn reload(&mut self, parts: Parts, cx: &mut Context<Self>) {
        if parts.is_empty() {
            return;
        }
        if self.loading.is_some() {
            self.queued = self.queued.or(parts);
            return;
        }
        let client = self.client(cx);
        let rec = self.rec;
        // A contact's deals are their company's where they are the contact: the company comes from the record.
        let contact_company = match &self.record {
            Some(Record::Contact(c)) => c.company_id,
            _ => None,
        };
        let task = Tokio::spawn(cx, async move {
            let acts = match rec {
                Rec::Company(id) => CrmActivityParams { company_id: Some(id), limit: Some(60), ..Default::default() },
                Rec::Contact(id) => CrmActivityParams { contact_id: Some(id), limit: Some(60), ..Default::default() },
                Rec::Deal(id) => CrmActivityParams { deal_id: Some(id), limit: Some(60), ..Default::default() },
            };
            let changes = CrmChangeParams { entity: Some(rec.entity().into()), entity_id: Some(rec.id()), limit: Some(30), ..Default::default() };
            let record = async {
                if !parts.record {
                    return None;
                }
                Some(match rec {
                    Rec::Company(id) => client.crm_company(id).await.map(Record::Company),
                    Rec::Contact(id) => client.crm_contact(id).await.map(Record::Contact),
                    Rec::Deal(id) => client.crm_deal(id).await.map(Record::Deal),
                })
            };
            let contacts = async {
                match rec {
                    Rec::Company(id) if parts.contacts => {
                        let by = CrmListParams { company_id: Some(id), limit: Some(200), sort: Some("name".into()), ..Default::default() };
                        client.crm_contacts(&by).await.ok()
                    }
                    _ => None,
                }
            };
            let deals = async {
                if !parts.deals {
                    return None;
                }
                match rec {
                    Rec::Company(id) => {
                        let by = CrmListParams { company_id: Some(id), limit: Some(200), sort: Some("updated".into()), ..Default::default() };
                        client.crm_deals(&by).await.ok()
                    }
                    Rec::Contact(id) => {
                        // Before the contact itself is in, its company comes from reading it.
                        let company = match contact_company {
                            Some(co) => Some(co),
                            None => client.crm_contact(id).await.ok().and_then(|c| c.company_id),
                        };
                        match company {
                            Some(co) => client
                                .crm_deals(&CrmListParams { company_id: Some(co), limit: Some(200), ..Default::default() })
                                .await
                                .ok()
                                .map(|v| v.into_iter().filter(|d| d.contact_id == Some(id)).collect()),
                            None => Some(Vec::new()),
                        }
                    }
                    Rec::Deal(_) => None,
                }
            };
            let activities = async { if parts.activities { client.crm_activities(&acts).await.ok() } else { None } };
            let changes = async { if parts.record { client.crm_changes(&changes).await.ok() } else { None } };
            tokio::join!(record, contacts, deals, activities, changes)
        });
        self.loading = Some(cx.spawn(async move |this, cx| {
            let Ok((record, contacts, deals, activities, changes)) = task.await else { return };
            let _ = this.update(cx, |p, cx| {
                p.loading = None;
                match record {
                    Some(Ok(r)) => {
                        p.record = Some(r);
                        p.error = None;
                        p.gone = false;
                    }
                    Some(Err(familiar_client::ApiError::Http { status: 404, .. })) => p.gone = true,
                    Some(Err(e)) => p.error = Some(e.message()),
                    None => {}
                }
                if contacts.is_some() {
                    p.contacts = contacts;
                }
                if deals.is_some() {
                    p.deals = deals;
                }
                if activities.is_some() {
                    p.activities = activities;
                }
                if changes.is_some() {
                    p.changes = changes;
                }
                cx.notify();
                let more = std::mem::take(&mut p.queued);
                if !more.is_empty() {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(std::time::Duration::from_millis(700)).await;
                        let _ = this.update(cx, |p, cx| p.reload(more, cx));
                    })
                    .detach();
                }
            });
        }));
    }

    /// What this panel shows of the CRM, for deciding whether a change notice concerns it.
    fn ctx(&self) -> Ctx {
        let (company, contact) = match &self.record {
            Some(Record::Contact(c)) => (c.company_id, None),
            Some(Record::Deal(d)) => (Some(d.company_id), d.contact_id),
            _ => (None, None),
        };
        Ctx {
            rec: Some(self.rec),
            company,
            contact,
            contacts: self.contacts.iter().flatten().map(|c| c.id).collect(),
            deals: self.deals.iter().flatten().map(|d| d.id).collect(),
        }
    }

    /// A change notice: read again only what it touches. A contact or deal the panel doesn't list yet is read first
    /// to see whether it belongs here.
    fn on_notice(&mut self, table: &str, id: Option<Uuid>, cx: &mut Context<Self>) {
        match scope(&self.ctx(), table, id) {
            Scope::Nothing => {}
            Scope::Parts(p) => self.reload(p, cx),
            Scope::Check(kind, id) => {
                let client = self.client(cx);
                let rec = self.rec;
                let task = Tokio::spawn(cx, async move {
                    match kind {
                        Kind::Contact => client.crm_contact(id).await.map(|c| (c.company_id, None)).ok(),
                        Kind::Deal => client.crm_deal(id).await.map(|d| (Some(d.company_id), d.contact_id)).ok(),
                    }
                });
                cx.spawn(async move |this, cx| {
                    let Ok(Some((company, contact))) = task.await else { return };
                    let ours = match rec {
                        Rec::Company(me) => company == Some(me),
                        Rec::Contact(me) => contact == Some(me),
                        Rec::Deal(_) => false,
                    };
                    if ours {
                        let parts = match kind {
                            Kind::Contact => Parts { contacts: true, ..Parts::NONE },
                            Kind::Deal => Parts { deals: true, ..Parts::NONE },
                        };
                        let _ = this.update(cx, |p, cx| p.reload(parts, cx));
                    }
                })
                .detach();
            }
        }
    }

    /// Run a write; `done` gets the answer. The panel is busy meanwhile and reloads after.
    fn write<T: Send + 'static>(
        &mut self,
        call: impl std::future::Future<Output = Result<T, familiar_client::ApiError>> + Send + 'static,
        failed: &'static str,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        cx.notify();
        let task = Tokio::spawn(cx, call);
        cx.spawn(async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
            let _ = this.update(cx, |p, cx| {
                p.busy = false;
                match r {
                    Ok(v) => done(p, v, cx),
                    Err(e) => {
                        if let Some(ed) = p.editing.as_mut() {
                            ed.error = Some(e.clone());
                        }
                        p.toast(Tone::Bad, failed, Some(e), cx);
                    }
                }
                p.reload(Parts::ALL, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn edit(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.record.as_ref() else { return };
        let start = field_text(r, field);
        let editor = if field.multiline() {
            let s = cx.new(|cx| TextareaState::new(window, cx).placeholder(field.hint()).auto_grow(2, 8));
            s.update(cx, |s, cx| {
                s.set_value(start, window, cx);
                s.focus(window, cx);
            });
            cx.subscribe(&s, |_, _, _: &InputEvent, cx| cx.notify()).detach();
            Editor::Area(s)
        } else {
            let s = text_input::new_line(field.hint(), false, window, cx);
            s.update(cx, |s, cx| {
                s.set_value(start, window, cx);
                s.focus(window, cx);
            });
            cx.subscribe_in(&s, window, |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } => this.save(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            })
            .detach();
            Editor::Line(s)
        };
        self.editing = Some(Editing { field, editor, error: None });
        cx.notify();
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(ed) = self.editing.as_mut() else { return };
        let text = ed.editor.value(cx);
        let field = ed.field;
        // Unchanged: just close.
        if self.record.as_ref().is_some_and(|r| field_text(r, field).trim() == text.trim()) {
            self.editing = None;
            cx.notify();
            return;
        }
        let patch = match patch_for(self.rec, field, &text) {
            Ok(p) => p,
            Err(e) => {
                ed.error = Some(e);
                cx.notify();
                return;
            }
        };
        self.send(patch, cx);
    }

    fn send(&mut self, patch: Patch, cx: &mut Context<Self>) {
        let client = self.client(cx);
        let rec = self.rec;
        let id = rec.id();
        let call = async move {
            match patch {
                Patch::Company(p) => client.update_crm_company(id, &p).await.map(Record::Company),
                Patch::Contact(p) => client.update_crm_contact(id, &p).await.map(Record::Contact),
                Patch::Deal(p) => client.update_crm_deal(id, &p).await.map(Record::Deal),
            }
        };
        self.write(call, "Couldn't save that", cx, move |p, r, cx| {
            p.record = Some(r);
            p.editing = None;
            p.dnc_on = None;
            p.dnc_off = false;
            cx.emit(PanelEvent::Saved(rec));
        });
    }

    /// Undo `change`. Undoing how the record was added takes it out of the CRM again: the panel closes.
    fn undo(&mut self, change: Uuid, added: bool, cx: &mut Context<Self>) {
        let client = self.client(cx);
        let rec = self.rec;
        let name = self.record.as_ref().map(|r| r.name().to_owned()).unwrap_or_default();
        self.write(async move { client.undo_crm_change(change).await }, "Couldn't undo it", cx, move |p, _, cx| {
            if added {
                cx.emit(PanelEvent::Removed { rec, name });
            } else {
                p.toast(Tone::Ok, "Undone", None, cx);
                cx.emit(PanelEvent::Saved(rec));
            }
        });
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        let client = self.client(cx);
        let rec = self.rec;
        let name = self.record.as_ref().map(|r| r.name().to_owned()).unwrap_or_default();
        self.confirm_delete = false;
        let call = async move {
            match rec {
                Rec::Company(id) => client.delete_crm_company(id).await,
                Rec::Contact(id) => client.delete_crm_contact(id).await,
                Rec::Deal(id) => client.delete_crm_deal(id).await,
            }
        };
        self.write(call, "Couldn't delete it", cx, move |_, _, cx| cx.emit(PanelEvent::Deleted { rec, name }));
    }

    fn set_stage(&mut self, stage: DealStage, cx: &mut Context<Self>) {
        self.stage_menu = None;
        // Another write is on its way: showing the new stage now would show one that is never sent.
        if self.busy {
            cx.notify();
            return;
        }
        if let Some(Record::Deal(d)) = self.record.as_mut() {
            if d.stage == stage {
                cx.notify();
                return;
            }
            d.stage = stage;
        }
        self.send(Patch::Deal(DealPatch { stage: Some(stage.as_str().to_owned()), ..Default::default() }), cx);
    }

    fn add_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.note.read(cx).value().trim().to_owned();
        if text.is_empty() || self.busy {
            return;
        }
        let (company_id, contact_id, deal_id) = match self.rec {
            Rec::Company(id) => (Some(id), None, None),
            Rec::Contact(id) => (None, Some(id), None),
            Rec::Deal(id) => (None, None, Some(id)),
        };
        let summary = excerpt(&text, 480);
        let body = (text.chars().count() > 480).then(|| text.clone());
        let a = NewActivity { company_id, contact_id, deal_id, kind: Some("note".into()), summary: Some(summary), body, ..Default::default() };
        self.note.update(cx, |s, cx| s.set_value("", window, cx));
        let client = self.client(cx);
        self.write(async move { client.log_crm_activity(&a).await }, "Couldn't add the note", cx, |_, _, _| {});
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if ev.keystroke.key == "escape" {
            if self.editing.is_some() {
                self.cancel_edit(window, cx);
            } else if self.stage_menu.is_some() {
                self.stage_menu = None;
                cx.notify();
            } else {
                cx.emit(PanelEvent::Close);
            }
            cx.stop_propagation();
        }
    }

    // ---- drawing ------------------------------------------------------------------------------------------------

    /// Who made a change: a teammate's mascot and name, or "You".
    fn who(&self, actor: &str, bot: Option<Uuid>, bot_name: Option<&str>, size: f32, cx: &App) -> (Option<AnyElement>, String) {
        if actor != "bot" {
            return (None, "You".to_owned());
        }
        let d = self.data.read(cx);
        match bot.and_then(|b| d.bot(b)) {
            Some(b) => (
                Some(Mascot::new(format!("crm-by-{}", b.id), avatar_of(b), MascotState::Idle, size).still().into_any_element()),
                b.name.clone(),
            ),
            None => (None, bot_name.unwrap_or("A teammate").to_owned()),
        }
    }

    fn section(title: &'static str, count: Option<usize>, theme: &Theme) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .pt(px(6.0))
            .child(div().text_size(px(text::CAPTION)).font_weight(FontWeight::SEMIBOLD).text_color(theme.muted).child(title.to_uppercase()))
            .when_some(count.filter(|n| *n > 0), |el, n| el.child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(n.to_string())))
    }

    /// "Changed by <teammate> · Undo": the newest change that can be undone.
    fn changed_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = Theme::of(cx).clone();
        let c = model::undoable(self.changes.as_deref()?)?.clone();
        let (face, who) = self.who(&c.actor_kind, c.bot_id, c.bot_name.as_deref(), 22.0, cx);
        let bot = c.actor_kind == "bot";
        let (id, added) = (c.id, c.op == "create");
        let this = cx.entity();
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(8.0))
                .rounded(px(RADIUS_CONTROL))
                .bg(if bot { theme.accent_soft } else { theme.sunken })
                .children(face)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(text::SMALL))
                                .truncate()
                                .child(if bot { format!("Changed by {who}") } else { "You changed it".to_owned() }),
                        )
                        .child(
                            div()
                                .text_size(px(text::CAPTION))
                                .text_color(theme.muted)
                                .truncate()
                                .child(format!("{} · {}", model::change_words(&c), ago(Some(c.at)))),
                        ),
                )
                .child(
                    Button::new("crm-undo-change", "Undo")
                        .size(ButtonSize::Small)
                        .icon(icons::UNDO)
                        .disabled(self.busy)
                        .tooltip("Put back what it was before this change")
                        .on_click(move |_, _, cx| this.update(cx, |p, cx| p.undo(id, added, cx))),
                )
                .into_any_element(),
        )
    }

    /// One field: its label and value; click to edit (or the editor while editing).
    fn field_row(&self, r: &Record, f: Field, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let label = div().w(px(92.0)).flex_none().pt(px(2.0)).text_size(px(text::CAPTION)).text_color(theme.muted).child(f.label());
        if let Some(ed) = self.editing.as_ref().filter(|e| e.field == f) {
            let input = match &ed.editor {
                Editor::Line(s) => text_input::field(("crm-edit", f as usize), s, 34.0, window, cx).into_any_element(),
                Editor::Area(s) => text_input::field(("crm-edit", f as usize), s, 64.0, window, cx).into_any_element(),
            };
            let (save, cancel) = (cx.entity(), cx.entity());
            return div()
                .flex()
                .items_start()
                .gap(px(8.0))
                .child(label)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(input)
                        .when_some(ed.error.clone(), |el, e| el.child(div().text_size(px(text::CAPTION)).text_color(theme.bad).child(e)))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .child(
                                    Button::new("crm-edit-save", if self.busy { "Saving…" } else { "Save" })
                                        .primary()
                                        .size(ButtonSize::Small)
                                        .disabled(self.busy)
                                        .on_click(move |_, window, cx| save.update(cx, |p, cx| p.save(window, cx))),
                                )
                                .child(
                                    Button::new("crm-edit-cancel", "Cancel")
                                        .ghost()
                                        .size(ButtonSize::Small)
                                        .on_click(move |_, window, cx| cancel.update(cx, |p, cx| p.cancel_edit(window, cx))),
                                )
                                .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(if f.multiline() {
                                    "Esc to cancel"
                                } else {
                                    "Enter to save · Esc to cancel"
                                })),
                        ),
                )
                .into_any_element();
        }
        let shown = field_shown(r, f);
        let this = cx.entity();
        let value: AnyElement = match (f, &shown) {
            (Field::Tags, Some(_)) => {
                let tags = match r {
                    Record::Company(c) => c.tags.clone(),
                    Record::Contact(c) => c.tags.clone(),
                    Record::Deal(_) => Vec::new(),
                };
                tag_chips(&tags, 8, cx).flex_wrap().into_any_element()
            }
            (_, Some(v)) => div().text_size(px(text::SMALL)).text_color(theme.ink).child(model::revealed(v, f.multiline())).into_any_element(),
            (_, None) => div().text_size(px(text::SMALL)).text_color(theme.muted.opacity(0.7)).child("Add…").into_any_element(),
        };
        let link = shown.as_deref().filter(|_| matches!(f, Field::Website | Field::Linkedin)).and_then(model::safe_url);
        div()
            .id(("crm-field", f as usize))
            .flex()
            .items_start()
            .gap(px(8.0))
            .px(px(8.0))
            .py(px(6.0))
            .mx(px(-8.0))
            .rounded(px(8.0))
            .cursor_pointer()
            .hover(|s| s.bg(theme.hover))
            .on_click(move |_, window, cx| this.update(cx, |p, cx| p.edit(f, window, cx)))
            .child(label)
            .child(div().flex_1().min_w_0().child(value))
            .when_some(link, |el, url| {
                el.child(
                    Button::icon_only(("crm-open", f as usize), icons::LINK)
                        .size(ButtonSize::Small)
                        .tooltip("Open in your browser")
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            cx.open_url(&url)
                        }),
                )
            })
            .into_any_element()
    }

    fn dnc_view(&self, c: &CrmContact, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let set_by = self.changes.as_deref().and_then(model::dnc_set_by);
        let this = cx.entity();
        let on = c.do_not_contact;
        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(12.0))
            .rounded(px(RADIUS_CONTROL))
            .border_1()
            .border_color(if on { theme.bad.opacity(0.4) } else { theme.line })
            .bg(if on { theme.bad_soft.opacity(if theme.is_dark() { 0.55 } else { 1.0 }) } else { theme.surface })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(icon(icons::BLOCK).size(px(16.0)).text_color(if on { theme.bad } else { theme.muted }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child("Do not contact"))
                            .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(if on {
                                "Teammates won't write to them: any draft to them is refused."
                            } else {
                                "Turn on if they asked not to be contacted."
                            })),
                    )
                    .child(Switch::new("crm-dnc", on || self.dnc_on.is_some()).on_toggle(move |want, window, cx| {
                        this.update(cx, |p, cx| {
                            if want {
                                let reason = text_input::new_line("Why (optional): “asked to stop”, “unsubscribed”…", false, window, cx);
                                reason.update(cx, |s, cx| s.focus(window, cx));
                                cx.subscribe_in(&reason, window, |p: &mut Self, _, ev: &InputEvent, _, cx| {
                                    if matches!(ev, InputEvent::PressEnter { .. }) {
                                        p.confirm_dnc(cx)
                                    }
                                })
                                .detach();
                                p.dnc_on = Some(reason);
                                p.dnc_off = false;
                            } else if p.dnc_on.is_some() {
                                p.dnc_on = None;
                            } else {
                                p.dnc_off = true;
                            }
                            cx.notify()
                        })
                    })),
            );
        if on {
            let mut detail = Vec::new();
            if let Some(ch) = set_by {
                let (_, who) = self.who(&ch.actor_kind, ch.bot_id, ch.bot_name.as_deref(), 16.0, cx);
                detail.push(format!("Set by {who} {}", ago(Some(ch.at))));
            } else if let Some(at) = c.dnc_at {
                detail.push(format!("Set {}", ago(Some(at))));
            }
            if let Some(r) = c.dnc_reason.as_deref().filter(|r| !r.trim().is_empty()) {
                detail.push(format!("Reason: {}", model::revealed(&excerpt(r, 160), false)));
            }
            if !detail.is_empty() {
                col = col.child(div().pl(px(26.0)).text_size(px(text::CAPTION)).text_color(theme.ink).child(detail.join(". ")));
            }
        }
        if let Some(reason) = self.dnc_on.as_ref() {
            let (go, cancel) = (cx.entity(), cx.entity());
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .pl(px(26.0))
                    .child(text_input::field("crm-dnc-reason", reason, 34.0, window, cx))
                    .child(
                        div()
                            .flex()
                            .gap(px(6.0))
                            .child(
                                Button::new("crm-dnc-yes", "Mark do-not-contact")
                                    .danger()
                                    .size(ButtonSize::Small)
                                    .disabled(self.busy)
                                    .on_click(move |_, _, cx| go.update(cx, |p, cx| p.confirm_dnc(cx))),
                            )
                            .child(Button::new("crm-dnc-no", "Cancel").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| {
                                cancel.update(cx, |p, cx| {
                                    p.dnc_on = None;
                                    cx.notify()
                                })
                            })),
                    ),
            );
        }
        if self.dnc_off {
            let (go, cancel) = (cx.entity(), cx.entity());
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .pl(px(26.0))
                    .child(div().text_size(px(text::SMALL)).child(format!(
                        "Let teammates contact {} again? Only do this if they asked to hear from you.",
                        model::revealed(&c.name, false)
                    )))
                    .child(
                        div()
                            .flex()
                            .gap(px(6.0))
                            .child(
                                Button::new("crm-dnc-clear", "Allow contact again")
                                    .size(ButtonSize::Small)
                                    .disabled(self.busy)
                                    .on_click(move |_, _, cx| {
                                        go.update(cx, |p, cx| {
                                            let patch = ContactPatch { do_not_contact: Some(false), dnc_reason: Some(String::new()), ..Default::default() };
                                            p.send(Patch::Contact(patch), cx)
                                        })
                                    }),
                            )
                            .child(Button::new("crm-dnc-keep", "Keep it on").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| {
                                cancel.update(cx, |p, cx| {
                                    p.dnc_off = false;
                                    cx.notify()
                                })
                            })),
                    ),
            );
        }
        col.into_any_element()
    }

    fn confirm_dnc(&mut self, cx: &mut Context<Self>) {
        let reason = self.dnc_on.as_ref().map(|r| r.read(cx).value().trim().to_owned()).unwrap_or_default();
        let patch = ContactPatch { do_not_contact: Some(true), dnc_reason: (!reason.is_empty()).then_some(reason), ..Default::default() };
        self.send(Patch::Contact(patch), cx);
    }

    /// A related record: click to open it.
    fn link_row(&self, id: impl Into<gpui::ElementId>, rec: Rec, title: String, detail: Option<String>, trailing: Option<AnyElement>, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        div()
            .id(id.into())
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(8.0))
            .py(px(7.0))
            .mx(px(-8.0))
            .rounded(px(8.0))
            .cursor_pointer()
            .hover(|s| s.bg(theme.hover))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(PanelEvent::Open(rec))))
            .child(icon(rec.glyph()).size(px(15.0)).text_color(theme.muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().text_size(px(text::SMALL)).child(model::revealed(&title, false)))
                    .when_some(detail.filter(|d| !d.is_empty()), |el, d| {
                        el.child(div().truncate().text_size(px(text::CAPTION)).text_color(theme.muted).child(model::revealed(&d, false)))
                    }),
            )
            .children(trailing)
            .into_any_element()
    }

    fn deal_rows(&self, deals: &[CrmDeal], cx: &mut Context<Self>) -> Vec<AnyElement> {
        deals
            .iter()
            .map(|d| {
                let value = d.value_cents.map(|v| model::money(v, &d.currency));
                let detail = [Some(model::stage_label(d.stage).to_owned()), value].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                let trailing = d.contact_do_not_contact.then(|| dnc_badge(cx).into_any_element());
                self.link_row(("crm-rel-deal", d.id.as_u128() as u64), Rec::Deal(d.id), d.title.clone(), Some(detail), trailing, cx)
            })
            .collect()
    }

    fn timeline(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let Some(list) = self.activities.as_ref() else {
            return div().flex().flex_col().gap(px(8.0)).children((0..3).map(|_| Skeleton::new(40.0).radius(RADIUS_CONTROL))).into_any_element();
        };
        let pending: Vec<Uuid> = self.data.read(cx).pending.iter().map(|a| a.id).collect();
        let mut col = div().flex().flex_col();
        if list.is_empty() {
            col = col.child(div().py(px(6.0)).text_size(px(text::SMALL)).text_color(theme.muted).child("Nothing yet."));
        }
        for (i, a) in list.iter().enumerate() {
            let (face, who) = self.who(&a.actor_kind, a.bot_id, a.bot_name.as_deref(), 18.0, cx);
            let waiting = a.approval_id.is_some_and(|id| pending.contains(&id));
            let needs = cx.entity();
            let url = a.url.as_deref().and_then(model::safe_url);
            let last = i + 1 == list.len();
            col = col.child(
                div()
                    .flex()
                    .gap(px(10.0))
                    // The rail: a dot per entry, a line to the next.
                    .child(
                        div()
                            .w(px(12.0))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .items_center()
                            .child(div().mt(px(5.0)).size(px(8.0)).rounded_full().bg(theme.tone(model::kind_tone(a.kind)).0))
                            .when(!last, |el| el.child(div().w(px(1.0)).flex_1().bg(theme.line))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .pb(px(14.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .text_size(px(text::CAPTION))
                                    .text_color(theme.muted)
                                    .child(div().font_weight(FontWeight::MEDIUM).text_color(theme.ink).child(model::kind_label(a.kind)))
                                    .child("·")
                                    .children(face)
                                    .child(div().truncate().child(who))
                                    .child("·")
                                    .child(div().flex_none().child(ago(Some(a.occurred_at)))),
                            )
                            .child(div().text_size(px(text::SMALL)).child(model::revealed(&a.summary, false)))
                            .when_some(a.body.clone().filter(|b| !b.trim().is_empty() && b.trim() != a.summary.trim()), |el, b| {
                                el.child(div().text_size(px(text::CAPTION)).text_color(theme.muted).line_clamp(4).child(model::revealed(&excerpt(&b, 600), true)))
                            })
                            .when(a.approval_id.is_some(), |el| {
                                el.child(if waiting {
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(8.0))
                                        .child(chip(Tone::Warn, "Draft waiting for you", cx))
                                        .child(
                                            Button::new(("crm-draft", i), "Review in Needs you")
                                                .size(ButtonSize::Small)
                                                .icon(icons::ARROW_RIGHT)
                                                .on_click(move |_, _, cx| needs.update(cx, |_, cx| cx.emit(PanelEvent::OpenNeeds))),
                                        )
                                        .into_any_element()
                                } else {
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.0))
                                        .text_size(px(text::CAPTION))
                                        .text_color(theme.muted)
                                        .child(icon(icons::CHECK).size(px(12.0)).text_color(theme.ok))
                                        .child(if matches!(a.kind, ActivityKind::EmailSent | ActivityKind::DmSent | ActivityKind::Post) {
                                            "Sent from a draft you approved"
                                        } else {
                                            "From a draft you reviewed"
                                        })
                                        .into_any_element()
                                })
                            })
                            .when_some(url, |el, url| {
                                let shown = excerpt(url.trim_start_matches("https://").trim_start_matches("http://"), 48);
                                el.child(
                                    div()
                                        .id(("crm-act-url", i))
                                        .flex()
                                        .items_center()
                                        .gap(px(4.0))
                                        .text_size(px(text::CAPTION))
                                        .text_color(theme.accent)
                                        .cursor_pointer()
                                        .on_click(move |_, _, cx| cx.open_url(&url))
                                        .child(icon(icons::LINK).size(px(12.0)).text_color(theme.accent))
                                        .child(shown),
                                )
                            }),
                    ),
            );
        }
        col.into_any_element()
    }

    fn history(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = Theme::of(cx).clone();
        let changes = self.changes.as_ref().filter(|c| !c.is_empty())?;
        let mut col = div().flex().flex_col().gap(px(2.0));
        for c in changes.iter().take(12) {
            let (_, who) = self.who(&c.actor_kind, c.bot_id, c.bot_name.as_deref(), 16.0, cx);
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .py(px(3.0))
                    .text_size(px(text::CAPTION))
                    .child(div().flex_1().min_w_0().truncate().text_color(if c.undone_at.is_some() { theme.muted } else { theme.ink }).when(c.undone_at.is_some(), |el| el.line_through()).child(format!("{who} {}", model::change_words(c))))
                    .child(div().flex_none().text_color(theme.muted).child(ago(Some(c.at)))),
            );
        }
        Some(col.into_any_element())
    }

    fn stage_row(&self, d: &CrmDeal, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let this = cx.entity();
        let at = model::stage_index(d.stage).unwrap_or(0);
        let menu = self.stage_menu.map(|cursor| {
            let (pick, mv, close) = (cx.entity(), cx.entity(), cx.entity());
            menu::popover(
                "crm-panel-stage",
                &self.menu_focus,
                STAGES.iter().enumerate().map(|(i, s)| MenuItem::new(model::stage_label(*s)).detail((i + 1).to_string()).checked(*s == d.stage)).collect(),
                cursor,
                200.0,
                false,
                move |i, _, cx| pick.update(cx, |p, cx| p.set_stage(STAGES[i], cx)),
                move |i, _, cx| {
                    mv.update(cx, |p, cx| {
                        p.stage_menu = Some(i);
                        cx.notify()
                    })
                },
                move |_, cx| {
                    close.update(cx, |p, cx| {
                        p.stage_menu = None;
                        cx.notify()
                    })
                },
                cx,
            )
        });
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(div().w(px(92.0)).flex_none().text_size(px(text::CAPTION)).text_color(theme.muted).child("Stage"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        menu::trigger("crm-panel-stage-btn", model::stage_label(d.stage), false, cx).on_click(move |_, window, cx| {
                            this.update(cx, |p, cx| {
                                if menu::closed_just_now("crm-panel-stage") {
                                    return;
                                }
                                p.stage_menu = if p.stage_menu.is_some() { None } else { Some(at) };
                                p.menu_focus.focus(window, cx);
                                cx.notify()
                            })
                        }),
                    )
                    .children(menu),
            )
            .child(div().text_size(px(text::CAPTION)).text_color(theme.muted).child(format!("since {}", ago(Some(d.stage_changed_at)))))
            .into_any_element()
    }
}

impl Render for RecordPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let rec = self.rec;
        let (back, close) = (cx.entity(), cx.entity());
        let top = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(14.0))
            .py(px(10.0))
            .border_b_1()
            .border_color(theme.line)
            .when(self.back, |el| {
                el.child(
                    div()
                        .id("crm-panel-back")
                        .flex()
                        .items_center()
                        .gap(px(2.0))
                        .pr(px(8.0))
                        .mr(px(2.0))
                        .border_r_1()
                        .border_color(theme.line)
                        .cursor_pointer()
                        .text_size(px(text::CAPTION))
                        .text_color(theme.muted)
                        .hover(|s| s.text_color(theme.ink))
                        .on_click(move |_, _, cx| back.update(cx, |_, cx| cx.emit(PanelEvent::Back)))
                        .child(
                            icon(icons::ALT_ARROW_RIGHT)
                                .size(px(14.0))
                                .text_color(theme.muted)
                                .with_transformation(gpui::Transformation::rotate(gpui::radians(std::f32::consts::PI))),
                        )
                        .child("Back"),
                )
            })
            .child(icon(rec.glyph()).size(px(14.0)).text_color(theme.muted))
            .child(div().text_size(px(text::CAPTION)).font_weight(FontWeight::MEDIUM).text_color(theme.muted).child(rec.noun()))
            .child(div().flex_1())
            .child(
                Button::icon_only("crm-panel-close", icons::CLOSE)
                    .size(ButtonSize::Small)
                    .tooltip("Close (Esc)")
                    .on_click(move |_, _, cx| close.update(cx, |_, cx| cx.emit(PanelEvent::Close))),
            );
        let body = if self.gone {
            div()
                .p(px(20.0))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(div().font_weight(FontWeight::MEDIUM).child("This record was deleted"))
                .child(div().text_size(px(text::SMALL)).text_color(theme.muted).child("Someone deleted it while it was open."))
                .into_any_element()
        } else if let Some(r) = self.record.clone() {
            self.body(&r, window, cx)
        } else if let Some(e) = self.error.clone() {
            div().p(px(20.0)).text_size(px(text::SMALL)).text_color(theme.bad).child(e).into_any_element()
        } else {
            div()
                .p(px(20.0))
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(Skeleton::new(28.0).width(220.0))
                .child(Skeleton::new(16.0).width(160.0))
                .child(Skeleton::new(120.0).radius(RADIUS_CONTROL))
                .into_any_element()
        };
        div()
            .id("crm-panel")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .flex()
            .flex_col()
            .child(top)
            .child(div().id("crm-panel-scroll").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).child(body))
    }
}

impl RecordPanel {
    fn body(&mut self, r: &Record, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let rec = self.rec;
        // Title: click to rename.
        let title_field = if matches!(rec, Rec::Deal(_)) { Field::Title } else { Field::Name };
        let title = if self.editing.as_ref().is_some_and(|e| e.field == title_field) {
            self.field_row(r, title_field, window, cx)
        } else {
            let this = cx.entity();
            div()
                .id("crm-title")
                .text_size(px(text::TITLE + 2.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.ink)
                .cursor_pointer()
                .rounded(px(6.0))
                .hover(|s| s.bg(theme.hover))
                .on_click(move |_, window, cx| this.update(cx, |p, cx| p.edit(title_field, window, cx)))
                .child(model::revealed(r.name(), false))
                .into_any_element()
        };
        let subtitle: Vec<AnyElement> = match r {
            Record::Company(c) => {
                let fit = c.fit_score.map(|s| {
                    let (fg, _) = theme.tone(model::fit_tone(s));
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(div().w(px(64.0)).h(px(6.0)).rounded_full().bg(theme.sunken).child(div().h_full().rounded_full().bg(fg).w(px(64.0 * s.clamp(0, 100) as f32 / 100.0))))
                        .child(div().text_size(px(text::CAPTION)).font_weight(FontWeight::MEDIUM).text_color(fg).child(format!("Fit {s}")))
                        .into_any_element()
                });
                fit.into_iter().collect()
            }
            Record::Contact(c) => {
                let mut v = Vec::new();
                if c.do_not_contact {
                    v.push(dnc_badge(cx).into_any_element());
                }
                v
            }
            Record::Deal(d) => {
                let mut v = vec![chip(model::stage_tone(d.stage), model::stage_label(d.stage), cx).into_any_element()];
                if let Some(val) = d.value_cents {
                    v.push(div().text_size(px(text::SMALL)).font_weight(FontWeight::MEDIUM).child(model::money(val, &d.currency)).into_any_element());
                }
                if d.contact_do_not_contact {
                    v.push(dnc_badge(cx).into_any_element());
                }
                v
            }
        };
        let (by_face, by) = match r {
            Record::Company(c) => self.who(if c.created_by_bot.is_some() { "bot" } else { "user" }, c.created_by_bot, None, 16.0, cx),
            Record::Contact(c) => self.who(if c.created_by_bot.is_some() { "bot" } else { "user" }, c.created_by_bot, None, 16.0, cx),
            Record::Deal(d) => self.who(if d.created_by_bot.is_some() { "bot" } else { "user" }, d.created_by_bot, None, 16.0, cx),
        };
        let created = match r {
            Record::Company(c) => c.created_at,
            Record::Contact(c) => c.created_at,
            Record::Deal(d) => d.created_at,
        };
        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .px(px(20.0))
            .pt(px(16.0))
            .pb(px(28.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(title)
                    .when(!subtitle.is_empty(), |el| el.child(div().flex().items_center().flex_wrap().gap(px(8.0)).children(subtitle)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(px(text::CAPTION))
                            .text_color(theme.muted)
                            .child("Added by")
                            .children(by_face)
                            .child(format!("{by} · {}", ago(Some(created)))),
                    ),
            );
        if let Some(banner) = self.changed_banner(cx) {
            col = col.child(banner);
        }
        if let Record::Contact(c) = r {
            col = col.child(self.dnc_view(c, window, cx));
        }
        // Where it belongs.
        match r {
            Record::Contact(c) => {
                if let Some(co) = c.company_id {
                    col = col.child(self.link_row("crm-rel-company", Rec::Company(co), c.company_name.clone().unwrap_or_else(|| "Company".into()), c.company_domain.clone(), None, cx));
                }
            }
            Record::Deal(d) => {
                col = col.child(self.stage_row(d, cx));
                col = col.child(self.link_row("crm-rel-company", Rec::Company(d.company_id), d.company_name.clone().unwrap_or_else(|| "Company".into()), d.company_domain.clone(), None, cx));
                if let Some(ct) = d.contact_id {
                    let trailing = d.contact_do_not_contact.then(|| dnc_badge(cx).into_any_element());
                    col = col.child(self.link_row("crm-rel-contact", Rec::Contact(ct), d.contact_name.clone().unwrap_or_else(|| "Contact".into()), d.contact_email.clone(), trailing, cx));
                }
            }
            Record::Company(_) => {}
        }
        // Fields.
        let fields: &[Field] = match r {
            Record::Company(_) => &COMPANY_FIELDS,
            Record::Contact(_) => &CONTACT_FIELDS,
            Record::Deal(_) => &DEAL_FIELDS,
        };
        let mut list = div().flex().flex_col().gap(px(2.0));
        if let Record::Company(_) = r {
            list = list.child(self.field_row(r, Field::FitScore, window, cx));
        }
        for f in fields {
            list = list.child(self.field_row(r, *f, window, cx));
        }
        col = col.child(Self::section("Details", None, &theme)).child(list);
        let sources = match r {
            Record::Company(c) => c.source_urls.clone(),
            Record::Contact(c) => c.source_urls.clone(),
            Record::Deal(_) => Vec::new(),
        };
        if !sources.is_empty() {
            let mut s = div().flex().flex_col().gap(px(2.0));
            for (i, u) in sources.iter().enumerate() {
                let url = model::safe_url(u);
                s = s.child(
                    div()
                        .id(("crm-src", i))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .py(px(2.0))
                        .text_size(px(text::CAPTION))
                        .text_color(theme.accent)
                        .when_some(url, |el, url| el.cursor_pointer().on_click(move |_, _, cx| cx.open_url(&url)))
                        .child(icon(icons::LINK).size(px(12.0)).text_color(theme.accent))
                        .child(div().truncate().child(model::revealed(&excerpt(u.trim_start_matches("https://").trim_start_matches("http://"), 60), false))),
                );
            }
            col = col.child(Self::section("Sources", Some(sources.len()), &theme)).child(s);
        }
        // People and deals.
        if let Record::Company(_) = r {
            let people = self.contacts.clone().unwrap_or_default();
            col = col.child(Self::section("People", Some(people.len()), &theme));
            if self.contacts.is_none() {
                col = col.child(Skeleton::new(36.0).radius(RADIUS_CONTROL));
            } else if people.is_empty() {
                col = col.child(div().text_size(px(text::SMALL)).text_color(theme.muted).child("No one yet."));
            }
            for p in &people {
                let trailing = p.do_not_contact.then(|| dnc_badge(cx).into_any_element());
                let row = self.link_row(("crm-rel-ct", p.id.as_u128() as u64), Rec::Contact(p.id), p.name.clone(), p.title.clone(), trailing, cx);
                col = col.child(row);
            }
        }
        if !matches!(r, Record::Deal(_)) {
            let deals = self.deals.clone().unwrap_or_default();
            col = col.child(Self::section("Deals", Some(deals.len()), &theme));
            if self.deals.is_none() {
                col = col.child(Skeleton::new(36.0).radius(RADIUS_CONTROL));
            } else if deals.is_empty() {
                col = col.child(div().text_size(px(text::SMALL)).text_color(theme.muted).child("No deals."));
            }
            col = col.children(self.deal_rows(&deals, cx));
        }
        // Timeline.
        let n = self.activities.as_ref().map(Vec::len);
        col = col
            .child(Self::section("Activity", n, &theme))
            .child(text_input::compact_field("crm-note", &self.note, icons::PEN, window, cx))
            .child(self.timeline(cx));
        if let Some(h) = self.history(cx) {
            col = col.child(Self::section("History", None, &theme)).child(h);
        }
        // Delete.
        let noun = rec.noun().to_lowercase();
        let (ask, yes, no) = (cx.entity(), cx.entity(), cx.entity());
        col = col.child(div().pt(px(8.0)).child(if self.confirm_delete {
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .p(px(12.0))
                .rounded(px(RADIUS_CONTROL))
                .border_1()
                .border_color(theme.bad.opacity(0.5))
                .child(div().text_size(px(text::SMALL)).child(format!(
                    "Delete “{}”? It leaves every list{}. You can undo it.",
                    model::revealed(&excerpt(r.name(), 60), false),
                    if matches!(rec, Rec::Company(_)) { ", with its deals" } else { "" }
                )))
                .child(
                    div()
                        .flex()
                        .gap(px(6.0))
                        .child(
                            Button::new("crm-del-yes", if self.busy { "Deleting…" } else { "Delete" })
                                .danger()
                                .size(ButtonSize::Small)
                                .disabled(self.busy)
                                .on_click(move |_, _, cx| yes.update(cx, |p, cx| p.delete(cx))),
                        )
                        .child(Button::new("crm-del-no", "Cancel").ghost().size(ButtonSize::Small).on_click(move |_, _, cx| {
                            no.update(cx, |p, cx| {
                                p.confirm_delete = false;
                                cx.notify()
                            })
                        })),
                )
                .into_any_element()
        } else {
            Button::new("crm-del", format!("Delete {noun}"))
                .ghost()
                .size(ButtonSize::Small)
                .icon(icons::TRASH)
                .on_click(move |_, _, cx| {
                    ask.update(cx, |p, cx| {
                        p.confirm_delete = true;
                        cx.notify()
                    })
                })
                .into_any_element()
        }));
        col.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_validate() {
        let co = Rec::Company(Uuid::nil());
        assert!(matches!(patch_for(co, Field::FitScore, "82"), Ok(Patch::Company(CompanyPatch { fit_score: Some(Some(82)), .. }))));
        assert!(patch_for(co, Field::FitScore, "120").is_err());
        assert!(patch_for(co, Field::FitScore, "high").is_err());
        assert!(patch_for(co, Field::Name, "  ").is_err());
        // An empty text clears a field.
        assert!(matches!(patch_for(co, Field::Industry, " "), Ok(Patch::Company(CompanyPatch { industry: Some(ref s), .. })) if s.is_empty()));
        match patch_for(co, Field::Tags, "a, b, a") {
            Ok(Patch::Company(p)) => assert_eq!(p.tags, Some(vec!["a".to_owned(), "b".to_owned()])),
            _ => panic!("tags"),
        }
        let ct = Rec::Contact(Uuid::nil());
        assert!(patch_for(ct, Field::Email, "sam@acme").is_err());
        assert!(patch_for(ct, Field::Email, "sam@acme.com").is_ok());
        assert!(patch_for(ct, Field::Email, "").is_ok());
        let dl = Rec::Deal(Uuid::nil());
        assert!(matches!(patch_for(dl, Field::Value, "12.5k"), Ok(Patch::Deal(DealPatch { value_cents: Some(Some(1_250_000)), .. }))));
        assert!(patch_for(dl, Field::Value, "lots").is_err());
        assert!(matches!(patch_for(dl, Field::Currency, "eur"), Ok(Patch::Deal(DealPatch { currency: Some(ref c), .. })) if c == "EUR"));
        assert!(patch_for(dl, Field::Currency, "euro").is_err());
        assert!(patch_for(dl, Field::NextStepAt, "tomorrow").is_ok());
        assert!(patch_for(dl, Field::NextStepAt, "soon").is_err());
        assert!(patch_for(dl, Field::Title, "").is_err());
    }

    #[test]
    fn numbers_and_dates_clear() {
        let co = Rec::Company(Uuid::nil());
        assert!(matches!(patch_for(co, Field::FitScore, " "), Ok(Patch::Company(CompanyPatch { fit_score: Some(None), .. }))));
        let dl = Rec::Deal(Uuid::nil());
        assert!(matches!(patch_for(dl, Field::Value, ""), Ok(Patch::Deal(DealPatch { value_cents: Some(None), .. }))));
        assert!(matches!(patch_for(dl, Field::NextStepAt, ""), Ok(Patch::Deal(DealPatch { next_step_at: Some(None), .. }))));
        // Clearing the step takes its date with it.
        assert!(matches!(
            patch_for(dl, Field::NextStep, ""),
            Ok(Patch::Deal(DealPatch { next_step: Some(ref s), next_step_at: Some(None), .. })) if s.is_empty()
        ));
        assert!(matches!(patch_for(dl, Field::NextStep, "Call"), Ok(Patch::Deal(DealPatch { next_step_at: None, .. }))));
        // Its leftover date isn't shown once the step is gone.
        let at = Some(Utc::now());
        let gone = Record::Deal(CrmDeal { next_step: None, next_step_at: at, ..Default::default() });
        assert_eq!(field_shown(&gone, Field::NextStepAt), None);
        let set = Record::Deal(CrmDeal { next_step: Some("Call".into()), next_step_at: at, ..Default::default() });
        assert!(field_shown(&set, Field::NextStepAt).is_some_and(|s| s.contains("today")));
    }

    #[test]
    fn notices_reach_only_the_panel_they_concern() {
        let n = |x: u128| Uuid::from_u128(x);
        let only = |p: Parts| Scope::Parts(p);
        let record = only(Parts { record: true, ..Parts::NONE });
        // A company panel: its own row, its people and deals; others are read first, the rest ignored.
        let company = Ctx { rec: Some(Rec::Company(n(1))), contacts: vec![n(10)], deals: vec![n(20)], ..Default::default() };
        assert_eq!(scope(&company, "crm_companies", Some(n(1))), record);
        assert_eq!(scope(&company, "crm_companies", Some(n(2))), Scope::Nothing);
        assert_eq!(scope(&company, "crm_contacts", Some(n(10))), only(Parts { contacts: true, ..Parts::NONE }));
        assert_eq!(scope(&company, "crm_contacts", Some(n(11))), Scope::Check(Kind::Contact, n(11)));
        assert_eq!(scope(&company, "crm_deals", Some(n(20))), only(Parts { deals: true, ..Parts::NONE }));
        assert_eq!(scope(&company, "crm_deals", Some(n(21))), Scope::Check(Kind::Deal, n(21)));
        assert_eq!(scope(&company, "crm_activities", Some(n(99))), only(Parts { activities: true, ..Parts::NONE }));
        assert_eq!(scope(&company, "crm_webhook_deliveries", Some(n(99))), Scope::Nothing);
        assert_eq!(scope(&company, "crm_contacts", None), only(Parts::ALL));
        // A deal panel: its row, its company and contact (their names show), nothing else.
        let deal = Ctx { rec: Some(Rec::Deal(n(20))), company: Some(n(1)), contact: Some(n(10)), ..Default::default() };
        assert_eq!(scope(&deal, "crm_deals", Some(n(20))), record);
        assert_eq!(scope(&deal, "crm_deals", Some(n(21))), Scope::Nothing);
        assert_eq!(scope(&deal, "crm_companies", Some(n(1))), record);
        assert_eq!(scope(&deal, "crm_contacts", Some(n(10))), record);
        assert_eq!(scope(&deal, "crm_contacts", Some(n(11))), Scope::Nothing);
        // A contact panel: its row, its company, its deals.
        let contact = Ctx { rec: Some(Rec::Contact(n(10))), company: Some(n(1)), ..Default::default() };
        assert_eq!(scope(&contact, "crm_contacts", Some(n(10))), record);
        assert_eq!(scope(&contact, "crm_contacts", Some(n(11))), Scope::Nothing);
        assert_eq!(scope(&contact, "crm_deals", Some(n(30))), Scope::Check(Kind::Deal, n(30)));
    }

    #[test]
    fn fields_show_and_edit() {
        let d = Record::Deal(CrmDeal { title: "Pilot".into(), value_cents: Some(1_250_000), currency: "USD".into(), ..Default::default() });
        assert_eq!(field_shown(&d, Field::Value).as_deref(), Some("$12,500"));
        assert_eq!(field_text(&d, Field::Value), "12,500");
        assert_eq!(field_shown(&d, Field::NextStep), None);
        let c = Record::Company(CrmCompany { name: "Acme".into(), tags: vec!["a".into(), "b".into()], ..Default::default() });
        assert_eq!(field_text(&c, Field::Tags), "a, b");
        assert_eq!(field_text(&c, Field::Name), "Acme");
    }
}
