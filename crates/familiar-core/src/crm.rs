//! The CRM data layer: companies, contacts, deals, their activity timeline and a change log. This is the one place CRM
//! rows are written: the API's routes and (later) a teammate's tools both call it. Rows travel as JSON built in SQL
//! (`to_jsonb`, `owner_id` stripped), like the API's other rows.
//!
//! Every write runs in one transaction with its `crm_changes` record (the row before and after, who did it) and a call
//! to [`enqueue_webhooks`]. Companies, contacts and deals are soft-deleted so a delete can be undone ([`undo`]).
//!
//! A teammate's writes ([`Actor::Bot`]) follow the trust rules of [`teammate`]: the owner's own edits win, a new company
//! or contact needs `source_urls`, and one run makes at most [`MAX_WRITES_PER_RUN`] changes.

use crate::db::Db;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sqlx::{PgConnection, types::Json};
use uuid::Uuid;

pub mod csv;
pub mod teammate;
pub mod webhooks;

/// The most CRM changes one teammate run may make (a runaway loop stops here; a new run can carry on).
pub const MAX_WRITES_PER_RUN: i64 = 200;

// ---- errors ---------------------------------------------------------------

#[derive(Debug)]
pub enum CrmError {
    /// The input is not acceptable; the message says why, in plain words.
    Invalid(String),
    /// Allowed for the owner only (a teammate tried to clear do-not-contact).
    Forbidden(String),
    NotFound,
    /// A unique key is taken, or an undo isn't possible any more.
    Conflict(String),
    Db(sqlx::Error),
}

pub type Result<T> = std::result::Result<T, CrmError>;

impl std::fmt::Display for CrmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Forbidden(m) | Self::Conflict(m) => f.write_str(m),
            Self::NotFound => f.write_str("not found"),
            Self::Db(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for CrmError {}

impl From<sqlx::Error> for CrmError {
    fn from(e: sqlx::Error) -> Self {
        match &e {
            sqlx::Error::RowNotFound => Self::NotFound,
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                Self::Conflict("a record with the same domain or email already exists".into())
            }
            sqlx::Error::Database(d) if d.is_check_violation() => {
                Self::Invalid(format!("a value is not allowed ({})", d.constraint().unwrap_or("check")))
            }
            sqlx::Error::Database(d) if d.is_foreign_key_violation() => {
                Self::Invalid("a referenced record does not exist".into())
            }
            _ => Self::Db(e),
        }
    }
}

fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(CrmError::Invalid(msg.into()))
}

// ---- who, what ------------------------------------------------------------

/// Who is writing: the owner (API, UI) or a teammate (during a run).
#[derive(Debug, Clone)]
pub enum Actor {
    User,
    Bot { bot: Uuid, run: Option<Uuid> },
}

impl Actor {
    fn kind(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Bot { .. } => "bot",
        }
    }
    fn bot(&self) -> Option<Uuid> {
        match self {
            Self::Bot { bot, .. } => Some(*bot),
            Self::User => None,
        }
    }
    fn run(&self) -> Option<Uuid> {
        match self {
            Self::Bot { run, .. } => *run,
            Self::User => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Company,
    Contact,
    Deal,
    Activity,
}

impl Kind {
    /// `company` / `companies`, `contact(s)`, `deal(s)`, `activity` / `activities`.
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "company" | "companies" => Kind::Company,
            "contact" | "contacts" => Kind::Contact,
            "deal" | "deals" => Kind::Deal,
            "activity" | "activities" => Kind::Activity,
            _ => return None,
        })
    }
    /// The singular name used in the change log and webhook events.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Company => "company",
            Kind::Contact => "contact",
            Kind::Deal => "deal",
            Kind::Activity => "activity",
        }
    }
    fn table(self) -> &'static str {
        match self {
            Kind::Company => "crm_companies",
            Kind::Contact => "crm_contacts",
            Kind::Deal => "crm_deals",
            Kind::Activity => "crm_activities",
        }
    }
    /// The columns a write may set (what an undo restores).
    fn columns(self) -> &'static [&'static str] {
        match self {
            Kind::Company => &[
                "name", "domain", "website", "industry", "size", "location", "description", "fit_score", "fit_reason",
                "tags", "source_urls", "custom", "deleted_at",
            ],
            Kind::Contact => &[
                "company_id", "name", "title", "email", "linkedin_url", "x_handle", "notes", "tags", "source_urls",
                "custom", "do_not_contact", "dnc_reason", "dnc_at", "deleted_at",
            ],
            Kind::Deal => &[
                "company_id", "contact_id", "title", "stage", "stage_changed_at", "value_cents", "currency", "next_step",
                "next_step_at", "deleted_at",
            ],
            Kind::Activity => &[],
        }
    }
}

pub const STAGES: [&str; 8] = ["new", "researching", "contacted", "replied", "meeting", "proposal", "won", "lost"];
pub const ACTIVITY_KINDS: [&str; 10] = [
    "note", "research", "email_sent", "email_received", "dm_sent", "dm_received", "post", "call", "meeting",
    "stage_change",
];

/// What a save did: a new row, a changed row, or an upsert that found nothing to change (no change record, no webhook).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Created,
    Updated,
    Unchanged,
}

/// A save of a company, contact or deal: the row as it is now, what happened, and (for a teammate) the fields it asked
/// to change that were kept because the owner set them ([`teammate`]'s owner-edits-win rule).
#[derive(Debug, Clone)]
pub struct Saved {
    pub row: Value,
    pub outcome: Outcome,
    pub kept: Vec<String>,
}

// ---- normalisers ----------------------------------------------------------

fn valid_host(h: &str) -> bool {
    if h.len() > 253 || !h.contains('.') {
        return false;
    }
    let labels: Vec<&str> = h.split('.').collect();
    let tld = labels[labels.len() - 1];
    labels.iter().all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    }) && tld.len() >= 2
        && tld.bytes().any(|b| b.is_ascii_lowercase())
}

/// `https://www.Acme.com/pricing` -> `acme.com`. None for anything that is not a public-looking host name (spaces, no
/// dot, an IP address, ...).
pub fn domain(s: &str) -> Option<String> {
    let s = s.trim().to_lowercase();
    let (rest, had_scheme) = match s.split_once("://") {
        Some((_, r)) => (r, true),
        None => (s.as_str(), false),
    };
    let host = rest.split(['/', '?', '#']).next()?;
    // user:pass@host only counts in a URL; a bare "a@b.com" is an email, not a domain
    let host = if had_scheme { host.rsplit('@').next()? } else { host };
    let host = host.split(':').next()?.trim_end_matches('.');
    let host = host.strip_prefix("www.").unwrap_or(host);
    // an international name in its ASCII (punycode) form, as DNS and the unique key see it
    let ascii;
    let host = if host.is_ascii() {
        host
    } else {
        ascii = url::Url::parse(&format!("http://{host}/")).ok()?.host_str()?.to_string();
        ascii.as_str()
    };
    valid_host(host).then(|| host.to_string())
}

/// `" Sam@Acme.COM "` -> `sam@acme.com`. Exactly one `@`, something before it, a dotted domain after it, no spaces.
pub fn email(s: &str) -> Option<String> {
    let s = s.trim().to_lowercase();
    if s.len() > 320 || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (local, host) = s.split_once('@')?;
    if local.is_empty() || host.contains('@') || !host.contains('.') {
        return None;
    }
    if host.starts_with('.') || host.ends_with('.') || host.contains("..") {
        return None;
    }
    Some(s)
}

/// The trimmed link when it is a well-formed `http` / `https` URL with a host; None otherwise.
pub fn http_url(s: &str) -> Option<String> {
    let s = s.trim();
    if s.chars().any(char::is_whitespace) {
        return None;
    }
    let u = url::Url::parse(s).ok()?;
    (matches!(u.scheme(), "http" | "https") && u.host_str().is_some()).then(|| s.to_string())
}

/// At most 20 tags of at most 40 characters; trimmed, empty ones dropped, duplicates (any case) dropped.
pub fn tags(v: &[String]) -> std::result::Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for t in v.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        if t.chars().count() > 40 {
            return Err(format!("tag too long (max 40): {t}"));
        }
        if !out.iter().any(|o| o.to_lowercase() == t.to_lowercase()) {
            out.push(t.to_string());
        }
    }
    if out.len() > 20 {
        return Err("too many tags (max 20)".into());
    }
    Ok(out)
}

/// At most 20 links, each a valid `http(s)` URL (see [`http_url`]) of at most 1000 characters; duplicates dropped.
pub fn source_urls(v: &[String]) -> std::result::Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for u in v.iter().map(|u| u.trim()).filter(|u| !u.is_empty()) {
        if u.chars().count() > 1000 {
            return Err("source link too long (max 1000)".into());
        }
        let Some(u) = http_url(u) else {
            return Err(format!("source_urls must be http(s) links: {u}"));
        };
        if !out.contains(&u) {
            out.push(u);
        }
    }
    if out.len() > 20 {
        return Err("too many source_urls (max 20)".into());
    }
    Ok(out)
}

// ---- inputs (every field optional: patch semantics; an empty string clears a text field, `null` a number or date) ----

/// A number or date field that can be cleared: absent = untouched (None), `null` = clear (Some(None)), a value =
/// set it (Some(Some(v))). Goes with `#[serde(default)]`.
pub type Clearable<T> = Option<Option<T>>;

fn clearable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> std::result::Result<Clearable<T>, D::Error> {
    Ok(Some(Option::<T>::deserialize(d)?))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CompanyInput {
    pub name: Option<String>,
    pub domain: Option<String>,
    pub website: Option<String>,
    pub industry: Option<String>,
    pub size: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    /// 0-100; `null` clears it.
    #[serde(default, deserialize_with = "clearable")]
    pub fit_score: Clearable<i32>,
    pub fit_reason: Option<String>,
    pub tags: Option<Vec<String>>,
    pub source_urls: Option<Vec<String>>,
    pub custom: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ContactInput {
    pub company_id: Option<Uuid>,
    pub name: Option<String>,
    pub title: Option<String>,
    pub email: Option<String>,
    pub linkedin_url: Option<String>,
    pub x_handle: Option<String>,
    pub notes: Option<String>,
    pub tags: Option<Vec<String>>,
    pub source_urls: Option<Vec<String>>,
    pub custom: Option<Value>,
    /// Anyone may set true; only the owner may set it back to false.
    pub do_not_contact: Option<bool>,
    pub dnc_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DealInput {
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub title: Option<String>,
    pub stage: Option<String>,
    /// `null` clears it.
    #[serde(default, deserialize_with = "clearable")]
    pub value_cents: Clearable<i64>,
    /// Three letters, e.g. `USD`.
    pub currency: Option<String>,
    pub next_step: Option<String>,
    /// `null` clears it.
    #[serde(default, deserialize_with = "clearable")]
    pub next_step_at: Clearable<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ActivityInput {
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub deal_id: Option<Uuid>,
    pub kind: Option<String>,
    pub summary: Option<String>,
    pub body: Option<String>,
    pub url: Option<String>,
    pub approval_id: Option<Uuid>,
    /// When it happened (default: now).
    pub occurred_at: Option<DateTime<Utc>>,
}

type M = Map<String, Value>;

fn chars_ok(key: &str, s: &str, max: usize) -> Result<()> {
    if s.chars().count() > max {
        return invalid(format!("{key} too long (max {max})"));
    }
    Ok(())
}

/// An optional text field: absent = untouched, empty = cleared, otherwise trimmed and length-checked.
fn put_text(m: &mut M, key: &str, v: &Option<String>, max: usize) -> Result<()> {
    if let Some(v) = v {
        let t = v.trim();
        if t.is_empty() {
            m.insert(key.into(), Value::Null);
        } else {
            chars_ok(key, t, max)?;
            m.insert(key.into(), json!(t));
        }
    }
    Ok(())
}

/// A required text field (when present it must not be empty).
fn put_name(m: &mut M, key: &str, v: &Option<String>, max: usize) -> Result<()> {
    if let Some(v) = v {
        let t = v.trim();
        if t.is_empty() {
            return invalid(format!("{key} must not be empty"));
        }
        chars_ok(key, t, max)?;
        m.insert(key.into(), json!(t));
    }
    Ok(())
}

fn put_url(m: &mut M, key: &str, v: &Option<String>, max: usize) -> Result<()> {
    if let Some(v) = v {
        if v.trim().is_empty() {
            m.insert(key.into(), Value::Null);
        } else {
            chars_ok(key, v.trim(), max)?;
            let Some(u) = http_url(v) else {
                return invalid(format!("{key} must be an http or https link"));
            };
            m.insert(key.into(), json!(u));
        }
    }
    Ok(())
}

fn put_lists(m: &mut M, t: &Option<Vec<String>>, s: &Option<Vec<String>>) -> Result<()> {
    if let Some(t) = t {
        m.insert("tags".into(), json!(tags(t).map_err(CrmError::Invalid)?));
    }
    if let Some(s) = s {
        m.insert("source_urls".into(), json!(source_urls(s).map_err(CrmError::Invalid)?));
    }
    Ok(())
}

fn put_custom(m: &mut M, c: &Option<Value>) -> Result<()> {
    if let Some(c) = c {
        if !c.is_object() {
            return invalid("custom must be a JSON object");
        }
        if c.to_string().len() > 10_000 {
            return invalid("custom too large (max 10000 bytes)");
        }
        m.insert("custom".into(), c.clone());
    }
    Ok(())
}

fn put_ref(m: &mut M, key: &str, v: Option<Uuid>) {
    if let Some(v) = v {
        m.insert(key.into(), json!(v));
    }
}

fn clean_company(i: &CompanyInput, derive_domain: bool) -> Result<M> {
    let mut m = M::new();
    put_name(&mut m, "name", &i.name, 200)?;
    if let Some(d) = &i.domain {
        if d.trim().is_empty() {
            m.insert("domain".into(), Value::Null);
        } else {
            let Some(d) = domain(d) else {
                return invalid("domain must be a host name like acme.com");
            };
            m.insert("domain".into(), json!(d));
        }
    }
    put_url(&mut m, "website", &i.website, 500)?;
    if derive_domain
        && !m.contains_key("domain")
        && let Some(d) = m.get("website").and_then(Value::as_str).and_then(domain)
    {
        m.insert("domain".into(), json!(d));
    }
    put_text(&mut m, "industry", &i.industry, 200)?;
    put_text(&mut m, "size", &i.size, 200)?;
    put_text(&mut m, "location", &i.location, 200)?;
    put_text(&mut m, "description", &i.description, 4000)?;
    match i.fit_score {
        Some(Some(s)) if !(0..=100).contains(&s) => return invalid("fit_score must be between 0 and 100"),
        Some(s) => {
            m.insert("fit_score".into(), json!(s));
        }
        None => {}
    }
    put_text(&mut m, "fit_reason", &i.fit_reason, 2000)?;
    put_lists(&mut m, &i.tags, &i.source_urls)?;
    put_custom(&mut m, &i.custom)?;
    Ok(m)
}

fn clean_contact(i: &ContactInput) -> Result<M> {
    let mut m = M::new();
    put_ref(&mut m, "company_id", i.company_id);
    put_name(&mut m, "name", &i.name, 200)?;
    put_text(&mut m, "title", &i.title, 200)?;
    if let Some(e) = &i.email {
        if e.trim().is_empty() {
            m.insert("email".into(), Value::Null);
        } else {
            let Some(e) = email(e) else {
                return invalid("email must look like name@company.com");
            };
            m.insert("email".into(), json!(e));
        }
    }
    put_url(&mut m, "linkedin_url", &i.linkedin_url, 500)?;
    if let Some(h) = &i.x_handle {
        let h = h.trim().trim_start_matches('@');
        if h.chars().any(char::is_whitespace) {
            return invalid("x_handle must not contain spaces");
        }
        put_text(&mut m, "x_handle", &Some(h.to_string()), 100)?;
    }
    put_text(&mut m, "notes", &i.notes, 4000)?;
    put_lists(&mut m, &i.tags, &i.source_urls)?;
    put_custom(&mut m, &i.custom)?;
    if let Some(b) = i.do_not_contact {
        m.insert("do_not_contact".into(), json!(b));
    }
    put_text(&mut m, "dnc_reason", &i.dnc_reason, 1000)?;
    Ok(m)
}

fn clean_deal(i: &DealInput) -> Result<M> {
    let mut m = M::new();
    put_ref(&mut m, "company_id", i.company_id);
    put_ref(&mut m, "contact_id", i.contact_id);
    put_name(&mut m, "title", &i.title, 200)?;
    if let Some(s) = &i.stage {
        if !STAGES.contains(&s.as_str()) {
            return invalid(format!("stage must be one of: {}", STAGES.join(", ")));
        }
        m.insert("stage".into(), json!(s));
    }
    match i.value_cents {
        Some(Some(v)) if v < 0 => return invalid("value_cents must not be negative"),
        Some(v) => {
            m.insert("value_cents".into(), json!(v));
        }
        None => {}
    }
    if let Some(c) = &i.currency {
        let c = c.trim().to_uppercase();
        if c.len() != 3 || !c.bytes().all(|b| b.is_ascii_uppercase()) {
            return invalid("currency must be three letters, like USD");
        }
        m.insert("currency".into(), json!(c));
    }
    put_text(&mut m, "next_step", &i.next_step, 500)?;
    if let Some(t) = i.next_step_at {
        m.insert("next_step_at".into(), json!(t.map(|t| t.to_rfc3339())));
    }
    Ok(m)
}

fn clean_activity(i: &ActivityInput) -> Result<M> {
    let mut m = M::new();
    put_ref(&mut m, "company_id", i.company_id);
    put_ref(&mut m, "contact_id", i.contact_id);
    put_ref(&mut m, "deal_id", i.deal_id);
    put_ref(&mut m, "approval_id", i.approval_id);
    let Some(kind) = &i.kind else {
        return invalid("kind is required");
    };
    if !ACTIVITY_KINDS.contains(&kind.as_str()) {
        return invalid(format!("kind must be one of: {}", ACTIVITY_KINDS.join(", ")));
    }
    m.insert("kind".into(), json!(kind));
    if i.summary.is_none() {
        return invalid("summary is required");
    }
    put_name(&mut m, "summary", &i.summary, 500)?;
    put_text(&mut m, "body", &i.body, 20_000)?;
    put_url(&mut m, "url", &i.url, 1000)?;
    if let Some(t) = i.occurred_at {
        m.insert("occurred_at".into(), json!(t.to_rfc3339()));
    }
    Ok(m)
}

// ---- webhooks and the change log ------------------------------------------

/// Queue a webhook delivery for every enabled webhook of `owner` that listens to `event`, inside the transaction of
/// the change itself (so a rolled-back write sends nothing). `payload` is `{event, data, previous, actor: {kind,
/// bot_id}}`; [`webhooks::enqueue`] completes it with the delivery `id`, `at` and the teammate's `bot_slug`.
pub async fn enqueue_webhooks(tx: &mut PgConnection, owner: Uuid, event: &str, payload: &Value) -> Result<()> {
    webhooks::enqueue(tx, owner, event, payload).await
}

async fn emit(
    tx: &mut PgConnection,
    owner: Uuid,
    actor: &Actor,
    event: &str,
    data: &Value,
    previous: Option<&Value>,
) -> Result<()> {
    let payload = json!({
        "event": event, "data": data, "previous": previous,
        "actor": { "kind": actor.kind(), "bot_id": actor.bot() },
    });
    enqueue_webhooks(tx, owner, event, &payload).await
}

/// Write the change record; returns it.
#[allow(clippy::too_many_arguments)]
async fn record(
    tx: &mut PgConnection,
    owner: Uuid,
    actor: &Actor,
    kind: Kind,
    id: Uuid,
    op: &str,
    before: Option<&Value>,
    after: Option<&Value>,
) -> Result<Value> {
    let row: Json<Value> = sqlx::query_scalar(
        "insert into crm_changes (owner_id, entity, entity_id, op, before, after, actor_kind, bot_id, run_id)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9) returning to_jsonb(crm_changes) - 'owner_id'",
    )
    .bind(owner)
    .bind(kind.name())
    .bind(id)
    .bind(op)
    .bind(before.map(Json))
    .bind(after.map(Json))
    .bind(actor.kind())
    .bind(actor.bot())
    .bind(actor.run())
    .fetch_one(&mut *tx)
    .await?;
    Ok(row.0)
}

fn id_of(row: &Value) -> Uuid {
    row["id"].as_str().and_then(|s| s.parse().ok()).unwrap_or_default()
}

fn now() -> Value {
    json!(Utc::now().to_rfc3339())
}

// ---- row access -----------------------------------------------------------

/// The row as JSON (without owner_id), locked for update; `live` hides soft-deleted rows.
async fn fetch_locked(tx: &mut PgConnection, kind: Kind, owner: Uuid, id: Uuid, live: bool) -> Result<Option<Value>> {
    let t = kind.table();
    let row: Option<Json<Value>> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select to_jsonb(t) - 'owner_id' from {t} t where t.id = $1 and t.owner_id = $2
         {} for update",
        if live { "and t.deleted_at is null" } else { "" }
    )))
    .bind(id)
    .bind(owner)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(row.map(|r| r.0))
}

/// Referenced rows must exist (and be live) and belong to the owner.
async fn check_refs(tx: &mut PgConnection, owner: Uuid, m: &M) -> Result<()> {
    for (key, table, live) in [
        ("company_id", "crm_companies", true),
        ("contact_id", "crm_contacts", true),
        ("deal_id", "crm_deals", true),
        ("approval_id", "approvals", false),
    ] {
        let Some(id) = m.get(key).and_then(Value::as_str).and_then(|s| s.parse::<Uuid>().ok()) else {
            continue;
        };
        let ok: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "select exists(select 1 from {table} where id = $1 and owner_id = $2 {})",
            if live { "and deleted_at is null" } else { "" }
        )))
        .bind(id)
        .bind(owner)
        .fetch_one(&mut *tx)
        .await?;
        if !ok {
            return invalid(format!("{key} does not match a record"));
        }
    }
    Ok(())
}

async fn insert_row(tx: &mut PgConnection, kind: Kind, owner: Uuid, m: &M) -> Result<Value> {
    let t = kind.table();
    let cols: Vec<&str> = m.keys().map(String::as_str).collect();
    let list = cols.join(", ");
    let from = cols.iter().map(|c| format!("r.{c}")).collect::<Vec<_>>().join(", ");
    let row: Json<Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "insert into {t} (owner_id, {list}) select $1, {from} from jsonb_populate_record(null::{t}, $2) r
         returning to_jsonb({t}) - 'owner_id'"
    )))
    .bind(owner)
    .bind(Json(m))
    .fetch_one(&mut *tx)
    .await?;
    Ok(row.0)
}

/// Set the columns of `m` (names from this module only). Unless `force`, only when one of them would change: None =
/// nothing changed.
async fn update_row(tx: &mut PgConnection, kind: Kind, owner: Uuid, id: Uuid, m: &M, force: bool) -> Result<Option<Value>> {
    let t = kind.table();
    let cols: Vec<&str> = m.keys().map(String::as_str).collect();
    let sets = cols.iter().map(|c| format!("{c} = r.{c}")).collect::<Vec<_>>().join(", ");
    let old = cols.iter().map(|c| format!("t.{c}")).collect::<Vec<_>>().join(", ");
    let new = cols.iter().map(|c| format!("r.{c}")).collect::<Vec<_>>().join(", ");
    let changed = if force { String::new() } else { format!("and ({old}) is distinct from ({new})") };
    let row: Option<Json<Value>> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "update {t} t set {sets}, updated_at = now() from jsonb_populate_record(null::{t}, $3) r
         where t.id = $1 and t.owner_id = $2 {changed} returning to_jsonb(t) - 'owner_id'"
    )))
    .bind(id)
    .bind(owner)
    .bind(Json(m))
    .fetch_optional(&mut *tx)
    .await?;
    Ok(row.map(|r| r.0))
}

/// A teammate run's write budget ([`MAX_WRITES_PER_RUN`]). The run's row is locked, so parallel tool calls of one run
/// count one after the other.
async fn budget(tx: &mut PgConnection, actor: &Actor) -> Result<()> {
    let Actor::Bot { run: Some(run), .. } = actor else { return Ok(()) };
    sqlx::query("select 1 from runs where id = $1 for no key update").bind(run).execute(&mut *tx).await?;
    let made: i64 = sqlx::query_scalar("select count(*) from crm_changes where run_id = $1")
        .bind(run)
        .fetch_one(&mut *tx)
        .await?;
    if made >= MAX_WRITES_PER_RUN {
        return invalid(format!(
            "This run already made {MAX_WRITES_PER_RUN} CRM changes, the most one run may make. Stop changing the CRM \
             now: finish with a summary of what you did and what is left (a later run can carry on)."
        ));
    }
    Ok(())
}

/// Create (`id` None) or change one company, contact or deal and log it. `m` holds only validated columns. A teammate's
/// change goes through [`teammate::owner_edits_win`] first.
async fn save(tx: &mut PgConnection, owner: Uuid, actor: &Actor, kind: Kind, id: Option<Uuid>, m: M) -> Result<Saved> {
    save_noted(tx, owner, actor, kind, id, m, None).await
}

/// [`save`]; a deal's stage move also goes on its timeline as a `stage_change` (with `note` as its body), whichever way
/// the stage was changed.
#[allow(clippy::too_many_arguments)]
async fn save_noted(
    tx: &mut PgConnection,
    owner: Uuid,
    actor: &Actor,
    kind: Kind,
    id: Option<Uuid>,
    mut m: M,
    note: Option<&str>,
) -> Result<Saved> {
    budget(tx, actor).await?;
    check_refs(tx, owner, &m).await?;
    let name = kind.name();
    let Some(id) = id else {
        if let Some(bot) = actor.bot() {
            // A teammate says where the facts came from, so the owner can check them.
            let sourced = m.get("source_urls").and_then(Value::as_array).is_some_and(|s| !s.is_empty());
            if matches!(kind, Kind::Company | Kind::Contact) && !sourced {
                return invalid(format!(
                    "a new {name} needs source_urls: the public pages its facts come from (a company's own website \
                     counts for an inbound lead)"
                ));
            }
            m.insert("created_by_bot".into(), json!(bot));
        }
        if m.get("do_not_contact") == Some(&json!(true)) {
            m.insert("dnc_at".into(), now());
        }
        let row = insert_row(tx, kind, owner, &m).await?;
        record(tx, owner, actor, kind, id_of(&row), "create", None, Some(&row)).await?;
        emit(tx, owner, actor, &format!("{name}.created"), &row, None).await?;
        if row["do_not_contact"] == json!(true) {
            emit(tx, owner, actor, "contact.do_not_contact", &row, None).await?;
        }
        return Ok(Saved { row, outcome: Outcome::Created, kept: vec![] });
    };
    let before = fetch_locked(tx, kind, owner, id, true).await?.ok_or(CrmError::NotFound)?;
    let kept = if actor.bot().is_some() {
        let fields: Vec<String> = m.keys().cloned().collect();
        let held = teammate::owner_held(tx, owner, kind, id, &fields).await?;
        teammate::owner_edits_win(kind, &before, &mut m, &held)
    } else {
        vec![]
    };
    let unchanged = |row: Value, kept: Vec<String>| Ok(Saved { row, outcome: Outcome::Unchanged, kept });
    let mut now_dnc = false;
    let mut stage_moved = false;
    if kind == Kind::Contact
        && let Some(want) = m.get("do_not_contact").and_then(Value::as_bool)
    {
        let was = before["do_not_contact"] == json!(true);
        if was && !want {
            if actor.bot().is_some() {
                return Err(CrmError::Forbidden("only the owner can clear do-not-contact".into()));
            }
            m.insert("dnc_at".into(), Value::Null);
            m.entry("dnc_reason").or_insert(Value::Null);
        } else if want && !was {
            m.insert("dnc_at".into(), now());
            now_dnc = true;
        }
    }
    if kind == Kind::Deal
        && let Some(stage) = m.get("stage")
        && *stage != before["stage"]
    {
        m.insert("stage_changed_at".into(), now());
        stage_moved = true;
    }
    if m.is_empty() {
        return unchanged(before, kept);
    }
    let Some(after) = update_row(tx, kind, owner, id, &m, false).await? else {
        return unchanged(before, kept);
    };
    record(tx, owner, actor, kind, id, "update", Some(&before), Some(&after)).await?;
    emit(tx, owner, actor, &format!("{name}.updated"), &after, Some(&before)).await?;
    if now_dnc {
        emit(tx, owner, actor, "contact.do_not_contact", &after, Some(&before)).await?;
    }
    if stage_moved {
        emit(tx, owner, actor, "deal.stage_changed", &after, Some(&before)).await?;
        let uuid = |k: &str| after[k].as_str().and_then(|s| s.parse::<Uuid>().ok());
        let a = ActivityInput {
            company_id: uuid("company_id"),
            contact_id: uuid("contact_id"),
            deal_id: Some(id),
            kind: Some("stage_change".into()),
            summary: Some(format!(
                "Stage: {} -> {}",
                before["stage"].as_str().unwrap_or("?"),
                after["stage"].as_str().unwrap_or("?")
            )),
            body: note.map(str::to_string),
            ..Default::default()
        };
        log_activity_in(tx, owner, actor, &a).await?;
    }
    Ok(Saved { row: after, outcome: Outcome::Updated, kept })
}

async fn find_one(tx: &mut PgConnection, sql: &str, owner: Uuid, a: &str) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string())).bind(owner).bind(a).fetch_optional(&mut *tx).await?)
}

// ---- companies ------------------------------------------------------------

/// The live company with this (already normalised) domain, else, when no domain is given, this name (any case).
pub async fn find_company_in(tx: &mut PgConnection, owner: Uuid, domain: Option<&str>, name: Option<&str>) -> Result<Option<Uuid>> {
    if let Some(d) = domain {
        return find_one(tx, "select id from crm_companies where owner_id = $1 and deleted_at is null and domain = $2", owner, d).await;
    }
    let Some(n) = name else { return Ok(None) };
    find_one(
        tx,
        "select id from crm_companies where owner_id = $1 and deleted_at is null and lower(name) = lower($2)
         order by created_at limit 1",
        owner,
        n,
    )
    .await
}

/// Create the company, or update the one it matches (same domain; or, without a domain, the same name). Several writes
/// in one transaction (a CSV import) use this form; see [`upsert_company`].
pub async fn upsert_company_in(tx: &mut PgConnection, owner: Uuid, actor: &Actor, i: &CompanyInput) -> Result<(Value, Outcome)> {
    write_company_in(tx, owner, actor, None, i).await.map(|s| (s.row, s.outcome))
}

/// Change the company `id`, or (None) create one / update the one it matches, as [`upsert_company_in`] does.
pub async fn write_company_in(tx: &mut PgConnection, owner: Uuid, actor: &Actor, id: Option<Uuid>, i: &CompanyInput) -> Result<Saved> {
    if id.is_some() {
        return save(tx, owner, actor, Kind::Company, id, clean_company(i, false)?).await;
    }
    let m = clean_company(i, true)?;
    let domain = m.get("domain").and_then(Value::as_str);
    let name = m.get("name").and_then(Value::as_str);
    let found = find_company_in(tx, owner, domain, name).await?;
    if found.is_none() && name.is_none() {
        return invalid("name is required");
    }
    save(tx, owner, actor, Kind::Company, found, m).await
}

/// Returns the company and whether it was created.
pub async fn upsert_company(db: &Db, actor: &Actor, i: &CompanyInput) -> Result<(Value, bool)> {
    let mut tx = db.pool.begin().await?;
    let (row, o) = upsert_company_in(&mut tx, db.owner, actor, i).await?;
    tx.commit().await?;
    Ok((row, o == Outcome::Created))
}

pub async fn patch_company(db: &Db, actor: &Actor, id: Uuid, i: &CompanyInput) -> Result<Value> {
    patch(db, actor, Kind::Company, id, clean_company(i, false)?).await
}

pub async fn soft_delete_company(db: &Db, actor: &Actor, id: Uuid) -> Result<Value> {
    soft_delete(db, actor, Kind::Company, id).await
}

// ---- contacts -------------------------------------------------------------

/// The live contact with this email, else (no contact has that email) this LinkedIn link, else this name at this
/// company (`company` None = no company).
pub async fn find_contact_in(
    tx: &mut PgConnection,
    owner: Uuid,
    email: Option<&str>,
    linkedin: Option<&str>,
    name: Option<&str>,
    company: Option<Uuid>,
) -> Result<Option<Uuid>> {
    if let Some(e) = email {
        let found = find_one(tx, "select id from crm_contacts where owner_id = $1 and deleted_at is null and email = $2", owner, e).await?;
        if found.is_some() {
            return Ok(found);
        }
    }
    if let Some(l) = linkedin {
        let found = find_one(
            tx,
            "select id from crm_contacts where owner_id = $1 and deleted_at is null and lower(linkedin_url) = lower($2)
             order by created_at limit 1",
            owner,
            l,
        )
        .await?;
        if found.is_some() {
            return Ok(found);
        }
    }
    let Some(n) = name else { return Ok(None) };
    Ok(sqlx::query_scalar(
        "select id from crm_contacts where owner_id = $1 and deleted_at is null and lower(name) = lower($2)
           and company_id is not distinct from $3 order by created_at limit 1",
    )
    .bind(owner)
    .bind(n)
    .bind(company)
    .fetch_optional(&mut *tx)
    .await?)
}

/// Create the contact, or update the one it matches (email, else LinkedIn link, else name + company).
pub async fn upsert_contact_in(tx: &mut PgConnection, owner: Uuid, actor: &Actor, i: &ContactInput) -> Result<(Value, Outcome)> {
    write_contact_in(tx, owner, actor, None, i).await.map(|s| (s.row, s.outcome))
}

/// Change the contact `id`, or (None) create one / update the one it matches, as [`upsert_contact_in`] does.
pub async fn write_contact_in(tx: &mut PgConnection, owner: Uuid, actor: &Actor, id: Option<Uuid>, i: &ContactInput) -> Result<Saved> {
    let m = clean_contact(i)?;
    if id.is_some() {
        return save(tx, owner, actor, Kind::Contact, id, m).await;
    }
    let get = |k: &str| m.get(k).and_then(Value::as_str);
    let found = find_contact_in(tx, owner, get("email"), get("linkedin_url"), get("name"), i.company_id).await?;
    if found.is_none() && !m.contains_key("name") {
        return invalid("name is required");
    }
    save(tx, owner, actor, Kind::Contact, found, m).await
}

/// Returns the contact and whether it was created.
pub async fn upsert_contact(db: &Db, actor: &Actor, i: &ContactInput) -> Result<(Value, bool)> {
    let mut tx = db.pool.begin().await?;
    let (row, o) = upsert_contact_in(&mut tx, db.owner, actor, i).await?;
    tx.commit().await?;
    Ok((row, o == Outcome::Created))
}

/// `do_not_contact: false` on a contact that has it set fails for a teammate ([`CrmError::Forbidden`]).
pub async fn patch_contact(db: &Db, actor: &Actor, id: Uuid, i: &ContactInput) -> Result<Value> {
    patch(db, actor, Kind::Contact, id, clean_contact(i)?).await
}

pub async fn soft_delete_contact(db: &Db, actor: &Actor, id: Uuid) -> Result<Value> {
    soft_delete(db, actor, Kind::Contact, id).await
}

// ---- deals ----------------------------------------------------------------

/// Create the deal, or update the one at the same company with the same title (any case).
pub async fn upsert_deal_in(tx: &mut PgConnection, owner: Uuid, actor: &Actor, i: &DealInput) -> Result<(Value, Outcome)> {
    write_deal_in(tx, owner, actor, None, i).await.map(|s| (s.row, s.outcome))
}

/// Change the deal `id`, or (None) create one / update the one it matches, as [`upsert_deal_in`] does.
pub async fn write_deal_in(tx: &mut PgConnection, owner: Uuid, actor: &Actor, id: Option<Uuid>, i: &DealInput) -> Result<Saved> {
    let m = clean_deal(i)?;
    if id.is_some() {
        return save(tx, owner, actor, Kind::Deal, id, m).await;
    }
    let found: Option<Uuid> = match (i.company_id, m.get("title").and_then(Value::as_str)) {
        (Some(c), Some(t)) => {
            sqlx::query_scalar(
                "select id from crm_deals where owner_id = $1 and deleted_at is null and company_id = $2
                   and lower(title) = lower($3) order by created_at limit 1",
            )
            .bind(owner)
            .bind(c)
            .bind(t)
            .fetch_optional(&mut *tx)
            .await?
        }
        _ => None,
    };
    if found.is_none() && !(m.contains_key("title") && m.contains_key("company_id")) {
        return invalid("title and company_id are required");
    }
    save(tx, owner, actor, Kind::Deal, found, m).await
}

/// Returns the deal and whether it was created.
pub async fn upsert_deal(db: &Db, actor: &Actor, i: &DealInput) -> Result<(Value, bool)> {
    let mut tx = db.pool.begin().await?;
    let (row, o) = upsert_deal_in(&mut tx, db.owner, actor, i).await?;
    tx.commit().await?;
    Ok((row, o == Outcome::Created))
}

pub async fn patch_deal(db: &Db, actor: &Actor, id: Uuid, i: &DealInput) -> Result<Value> {
    patch(db, actor, Kind::Deal, id, clean_deal(i)?).await
}

pub async fn soft_delete_deal(db: &Db, actor: &Actor, id: Uuid) -> Result<Value> {
    soft_delete(db, actor, Kind::Deal, id).await
}

/// Move a deal to a stage: sets `stage_changed_at` and logs a `stage_change` activity (with `note` as its body).
/// Already in that stage: nothing happens.
pub async fn move_deal(db: &Db, actor: &Actor, deal: Uuid, stage: &str, note: Option<&str>) -> Result<Value> {
    let mut tx = db.pool.begin().await?;
    let saved = move_deal_in(&mut tx, db.owner, actor, deal, stage, note).await?;
    tx.commit().await?;
    Ok(saved.row)
}

/// [`move_deal`] inside a transaction.
pub async fn move_deal_in(tx: &mut PgConnection, owner: Uuid, actor: &Actor, deal: Uuid, stage: &str, note: Option<&str>) -> Result<Saved> {
    let m = clean_deal(&DealInput { stage: Some(stage.to_string()), ..Default::default() })?;
    save_noted(tx, owner, actor, Kind::Deal, Some(deal), m, note).await
}

// ---- generic patch / delete -----------------------------------------------

async fn patch(db: &Db, actor: &Actor, kind: Kind, id: Uuid, m: M) -> Result<Value> {
    let mut tx = db.pool.begin().await?;
    let saved = save(&mut tx, db.owner, actor, kind, Some(id), m).await?;
    tx.commit().await?;
    Ok(saved.row)
}

async fn soft_delete(db: &Db, actor: &Actor, kind: Kind, id: Uuid) -> Result<Value> {
    let mut tx = db.pool.begin().await?;
    let before = fetch_locked(&mut tx, kind, db.owner, id, true).await?.ok_or(CrmError::NotFound)?;
    let m: M = [("deleted_at".to_string(), now())].into_iter().collect();
    let after = update_row(&mut tx, kind, db.owner, id, &m, true).await?.ok_or(CrmError::NotFound)?;
    record(&mut tx, db.owner, actor, kind, id, "delete", Some(&before), Some(&after)).await?;
    emit(&mut tx, db.owner, actor, &format!("{}.updated", kind.name()), &after, Some(&before)).await?;
    tx.commit().await?;
    Ok(after)
}

// ---- activities -----------------------------------------------------------

/// Add an entry to the timeline. A deal's company and contact are filled in when not given, so it also shows on theirs.
pub async fn log_activity_in(tx: &mut PgConnection, owner: Uuid, actor: &Actor, i: &ActivityInput) -> Result<Value> {
    let mut m = clean_activity(i)?;
    if !(m.contains_key("company_id") || m.contains_key("contact_id") || m.contains_key("deal_id")) {
        return invalid("an activity needs a company_id, contact_id or deal_id");
    }
    budget(tx, actor).await?;
    check_refs(tx, owner, &m).await?;
    if let Some(deal) = i.deal_id {
        let d: Option<(Uuid, Option<Uuid>)> =
            sqlx::query_as("select company_id, contact_id from crm_deals where id = $1 and owner_id = $2")
                .bind(deal)
                .bind(owner)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some((company, contact)) = d {
            m.entry("company_id").or_insert(json!(company));
            if let Some(c) = contact {
                m.entry("contact_id").or_insert(json!(c));
            }
        }
    }
    m.insert("actor_kind".into(), json!(actor.kind()));
    if let Some(bot) = actor.bot() {
        m.insert("bot_id".into(), json!(bot));
    }
    let row = insert_row(tx, Kind::Activity, owner, &m).await?;
    record(tx, owner, actor, Kind::Activity, id_of(&row), "create", None, Some(&row)).await?;
    emit(tx, owner, actor, "activity.created", &row, None).await?;
    Ok(row)
}

pub async fn log_activity(db: &Db, actor: &Actor, i: &ActivityInput) -> Result<Value> {
    let mut tx = db.pool.begin().await?;
    let row = log_activity_in(&mut tx, db.owner, actor, i).await?;
    tx.commit().await?;
    Ok(row)
}

// ---- undo -----------------------------------------------------------------

/// Undo one change of a company, contact or deal: a create is soft-deleted, an update restores what it replaced, a
/// delete is brought back. Only the newest change of that record that is not itself undone can be undone (else
/// [`CrmError::Conflict`]). Records an `undo` change and returns it.
pub async fn undo(db: &Db, change_id: Uuid) -> Result<Value> {
    let owner = db.owner;
    let mut tx = db.pool.begin().await?;
    let ch: Option<(String, Uuid, String, Option<Json<Value>>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "select entity, entity_id, op, before, undone_at from crm_changes where id = $1 and owner_id = $2 for update",
    )
    .bind(change_id)
    .bind(owner)
    .fetch_optional(&mut *tx)
    .await?;
    let (entity, id, op, before, undone) = ch.ok_or(CrmError::NotFound)?;
    let kind = Kind::parse(&entity).ok_or(CrmError::NotFound)?;
    if kind == Kind::Activity {
        return invalid("timeline entries can't be undone");
    }
    if op == "undo" {
        return Err(CrmError::Conflict("an undo can't be undone".into()));
    }
    if undone.is_some() {
        return Err(CrmError::Conflict("this change was already undone".into()));
    }
    let newest: Option<Uuid> = sqlx::query_scalar(
        "select id from crm_changes where owner_id = $1 and entity = $2 and entity_id = $3
           and undone_at is null and op <> 'undo' order by at desc, id desc limit 1",
    )
    .bind(owner)
    .bind(&entity)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    if newest != Some(change_id) {
        return Err(CrmError::Conflict("only the newest change to a record can be undone".into()));
    }
    let current = fetch_locked(&mut tx, kind, owner, id, false).await?.ok_or(CrmError::NotFound)?;
    let m: M = match op.as_str() {
        "create" => [("deleted_at".to_string(), now())].into_iter().collect(),
        "delete" => [("deleted_at".to_string(), Value::Null)].into_iter().collect(),
        _ => {
            let before = before.map(|b| b.0).unwrap_or(Value::Null);
            kind.columns()
                .iter()
                .filter_map(|c| before.get(*c).map(|v| (c.to_string(), v.clone())))
                .collect()
        }
    };
    let after = update_row(&mut tx, kind, owner, id, &m, true).await?.ok_or(CrmError::NotFound)?;
    sqlx::query("update crm_changes set undone_at = now() where id = $1")
        .bind(change_id)
        .execute(&mut *tx)
        .await?;
    let actor = Actor::User;
    let undo = record(&mut tx, owner, &actor, kind, id, "undo", Some(&current), Some(&after)).await?;
    emit(&mut tx, owner, &actor, &format!("{}.updated", kind.name()), &after, Some(&current)).await?;
    tx.commit().await?;
    Ok(undo)
}

// ---- queries --------------------------------------------------------------

const COMPANY_SEL: &str = "select to_jsonb(x) - 'owner_id' from crm_companies x";
const CONTACT_SEL: &str = "select (to_jsonb(x) - 'owner_id') || jsonb_build_object('company_name', co.name, 'company_domain', co.domain)
     from crm_contacts x left join crm_companies co on co.id = x.company_id";
// a deal of a deleted company is hidden with it
const DEAL_SEL: &str = "select (to_jsonb(x) - 'owner_id') || jsonb_build_object('company_name', co.name,
         'company_domain', co.domain, 'contact_name', ct.name, 'contact_email', ct.email,
         'contact_do_not_contact', coalesce(ct.do_not_contact, false))
     from crm_deals x join crm_companies co on co.id = x.company_id and co.deleted_at is null
     left join crm_contacts ct on ct.id = x.contact_id";

fn select_for(kind: Kind) -> Result<&'static str> {
    match kind {
        Kind::Company => Ok(COMPANY_SEL),
        Kind::Contact => Ok(CONTACT_SEL),
        Kind::Deal => Ok(DEAL_SEL),
        Kind::Activity => invalid("activities are read with `activities`"),
    }
}

/// One live company, contact or deal (with its joined names).
pub async fn get(db: &Db, kind: Kind, id: Uuid) -> Result<Value> {
    let sel = select_for(kind)?;
    let row: Option<Json<Value>> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "{sel} where x.id = $1 and x.owner_id = $2 and x.deleted_at is null"
    )))
    .bind(id)
    .bind(db.owner)
    .fetch_optional(&db.pool)
    .await?;
    row.map(|r| r.0).ok_or(CrmError::NotFound)
}

/// What [`search`] filters by; a filter that doesn't apply to the kind is ignored.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    /// Text to look for in the main fields.
    pub q: Option<String>,
    /// Companies and contacts.
    pub tag: Option<String>,
    /// Companies and contacts: every one of these tags too (any case).
    pub tags: Vec<String>,
    /// Deals.
    pub stage: Option<String>,
    /// Contacts and deals.
    pub company_id: Option<Uuid>,
    /// Deals.
    pub contact_id: Option<Uuid>,
    /// Contacts: only those (not) marked do-not-contact.
    pub dnc: Option<bool>,
    /// `updated` (default), `name` or `fit` (companies; else as `updated`).
    pub sort: Option<String>,
}

fn like(q: &Option<String>) -> Option<String> {
    let q = q.as_deref()?.trim();
    (!q.is_empty()).then(|| format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")))
}

async fn list(db: &Db, kind: Kind, f: &Filters, limit: i64, offset: i64) -> Result<Vec<Value>> {
    let sel = select_for(kind)?;
    let sort = f.sort.as_deref().unwrap_or("updated");
    if !["updated", "name", "fit"].contains(&sort) {
        return invalid("sort must be one of: updated, name, fit");
    }
    let name_col = if kind == Kind::Deal { "title" } else { "name" };
    let order = match (sort, kind) {
        ("name", _) => format!("lower(x.{name_col}), x.id"),
        ("fit", Kind::Company) => "x.fit_score desc nulls last, x.id".to_string(),
        _ => "x.updated_at desc, x.id".to_string(),
    };
    let q = like(&f.q);
    let base = "x.owner_id = $1 and x.deleted_at is null";
    // every wanted tag is on the record (any case)
    let tags: Vec<String> =
        f.tag.iter().chain(&f.tags).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
    let has_tags = "not exists (select 1 from unnest($3::text[]) w
                                where not exists (select 1 from unnest(x.tags) g where lower(g) = lower(w)))";
    let rows: Vec<Json<Value>> = match kind {
        Kind::Company => {
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "{sel} where {base}
                   and ($2::text is null or x.name ilike $2 or x.domain ilike $2 or x.industry ilike $2
                        or x.location ilike $2 or x.description ilike $2)
                   and {has_tags}
                 order by {order} limit $4 offset $5"
            )))
            .bind(db.owner)
            .bind(q)
            .bind(&tags)
            .bind(limit)
            .bind(offset)
            .fetch_all(&db.pool)
            .await?
        }
        Kind::Contact => {
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "{sel} where {base}
                   and ($2::text is null or x.name ilike $2 or x.email ilike $2 or x.title ilike $2
                        or x.notes ilike $2 or co.name ilike $2)
                   and {has_tags}
                   and ($4::uuid is null or x.company_id = $4)
                   and ($5::bool is null or x.do_not_contact = $5)
                 order by {order} limit $6 offset $7"
            )))
            .bind(db.owner)
            .bind(q)
            .bind(&tags)
            .bind(f.company_id)
            .bind(f.dnc)
            .bind(limit)
            .bind(offset)
            .fetch_all(&db.pool)
            .await?
        }
        _ => {
            if let Some(s) = &f.stage
                && !STAGES.contains(&s.as_str())
            {
                return invalid(format!("stage must be one of: {}", STAGES.join(", ")));
            }
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "{sel} where {base}
                   and ($2::text is null or x.title ilike $2 or x.next_step ilike $2 or co.name ilike $2)
                   and ($3::text is null or x.stage = $3)
                   and ($4::uuid is null or x.company_id = $4)
                   and ($7::uuid is null or x.contact_id = $7)
                 order by {order} limit $5 offset $6"
            )))
            .bind(db.owner)
            .bind(q)
            .bind(f.stage.as_deref())
            .bind(f.company_id)
            .bind(limit)
            .bind(offset)
            .bind(f.contact_id)
            .fetch_all(&db.pool)
            .await?
        }
    };
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// Live companies, contacts or deals matching the filters; `limit` is at most 200 (default 50).
pub async fn search(db: &Db, kind: Kind, f: &Filters, limit: Option<i64>, offset: Option<i64>) -> Result<Vec<Value>> {
    list(db, kind, f, limit.unwrap_or(50).clamp(1, 200), offset.unwrap_or(0).max(0)).await
}

/// Every live row of a kind, by name (for the CSV export).
pub async fn all(db: &Db, kind: Kind) -> Result<Vec<Value>> {
    let f = Filters { sort: Some("name".into()), ..Default::default() };
    list(db, kind, &f, 100_000, 0).await
}

/// The deals shown per stage on the board (the count and value cover all of them).
pub const PIPELINE_DEALS_PER_STAGE: i64 = 100;

/// One entry per stage, in order: `{stage, count, value_cents, deals: [{id, title, company_id, company_name, contact_id,
/// contact_name, stage_changed_at, value_cents, currency, next_step, next_step_at}]}`.
pub async fn pipeline(db: &Db) -> Result<Vec<Value>> {
    let totals: Vec<(String, i64, i64)> = sqlx::query_as(
        "select d.stage, count(*)::int8, coalesce(sum(d.value_cents), 0)::int8
         from crm_deals d join crm_companies co on co.id = d.company_id and co.deleted_at is null
         where d.owner_id = $1 and d.deleted_at is null group by d.stage",
    )
    .bind(db.owner)
    .fetch_all(&db.pool)
    .await?;
    let deals: Vec<Json<Value>> = sqlx::query_scalar(
        "select to_jsonb(x) - 'rn' from (
           select d.id, d.stage, d.title, d.company_id, co.name as company_name, d.contact_id, ct.name as contact_name,
                  coalesce(ct.do_not_contact, false) as contact_do_not_contact,
                  d.stage_changed_at, d.value_cents, d.currency, d.next_step, d.next_step_at,
                  row_number() over (partition by d.stage order by d.updated_at desc, d.id) as rn
           from crm_deals d join crm_companies co on co.id = d.company_id and co.deleted_at is null
           left join crm_contacts ct on ct.id = d.contact_id
           where d.owner_id = $1 and d.deleted_at is null) x
         where x.rn <= $2 order by x.rn",
    )
    .bind(db.owner)
    .bind(PIPELINE_DEALS_PER_STAGE)
    .fetch_all(&db.pool)
    .await?;
    Ok(STAGES
        .iter()
        .map(|s| {
            let (count, value) = totals.iter().find(|t| t.0 == *s).map_or((0, 0), |t| (t.1, t.2));
            let of: Vec<&Value> = deals.iter().map(|d| &d.0).filter(|d| d["stage"] == *s).collect();
            json!({ "stage": s, "count": count, "value_cents": value, "deals": of })
        })
        .collect())
}

#[derive(Debug, Clone, Default)]
pub struct ActivityFilters {
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub deal_id: Option<Uuid>,
}

/// The timeline, newest first, with the teammate's name (`bot_name`); `limit` at most 200 (default 50).
pub async fn activities(db: &Db, f: &ActivityFilters, limit: Option<i64>, offset: Option<i64>) -> Result<Vec<Value>> {
    let rows: Vec<Json<Value>> = sqlx::query_scalar(
        "select (to_jsonb(x) - 'owner_id') || jsonb_build_object('bot_name', b.name)
         from crm_activities x left join bots b on b.id = x.bot_id
         where x.owner_id = $1 and ($2::uuid is null or x.company_id = $2) and ($3::uuid is null or x.contact_id = $3)
           and ($4::uuid is null or x.deal_id = $4)
         order by x.occurred_at desc, x.id limit $5 offset $6",
    )
    .bind(db.owner)
    .bind(f.company_id)
    .bind(f.contact_id)
    .bind(f.deal_id)
    .bind(limit.unwrap_or(50).clamp(1, 200))
    .bind(offset.unwrap_or(0).max(0))
    .fetch_all(&db.pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

#[derive(Debug, Clone, Default)]
pub struct ChangeFilters {
    /// `company` | `contact` | `deal` | `activity`
    pub entity: Option<String>,
    pub entity_id: Option<Uuid>,
    pub bot_id: Option<Uuid>,
}

/// The change log, newest first, with the teammate's name (`bot_name`); `limit` at most 200 (default 50).
pub async fn changes(db: &Db, f: &ChangeFilters, limit: Option<i64>) -> Result<Vec<Value>> {
    if let Some(e) = &f.entity
        && Kind::parse(e).is_none_or(|k| k.name() != e.as_str())
    {
        return invalid("entity must be one of: company, contact, deal, activity");
    }
    let rows: Vec<Json<Value>> = sqlx::query_scalar(
        "select (to_jsonb(x) - 'owner_id') || jsonb_build_object('bot_name', b.name)
         from crm_changes x left join bots b on b.id = x.bot_id
         where x.owner_id = $1 and ($2::text is null or x.entity = $2) and ($3::uuid is null or x.entity_id = $3)
           and ($4::uuid is null or x.bot_id = $4)
         order by x.at desc, x.id desc limit $5",
    )
    .bind(db.owner)
    .bind(f.entity.as_deref())
    .bind(f.entity_id)
    .bind(f.bot_id)
    .bind(limit.unwrap_or(50).clamp(1, 200))
    .fetch_all(&db.pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains() {
        assert_eq!(domain("https://www.Acme.com/pricing").as_deref(), Some("acme.com"));
        assert_eq!(domain("  ACME.com ").as_deref(), Some("acme.com"));
        assert_eq!(domain("http://user:pw@sub.acme.co.uk:8080/x?y#z").as_deref(), Some("sub.acme.co.uk"));
        assert_eq!(domain("www.acme.com.").as_deref(), Some("acme.com"));
        // international names in their ASCII form
        assert_eq!(domain("https://www.München.de/kontakt").as_deref(), Some("xn--mnchen-3ya.de"));
        assert_eq!(domain("xn--mnchen-3ya.de").as_deref(), Some("xn--mnchen-3ya.de"));
        assert_eq!(domain("münchen"), None);
        for bad in [
            "", "   ", "acme", "localhost", "not a domain", "sam@acme.com", "https://", "1.2.3.4", "-a.com", "a..com",
            "acme.c", "ac me.com", "ex_ample.com", "https:///path",
        ] {
            assert_eq!(domain(bad), None, "{bad:?}");
        }
        assert_eq!(domain(&format!("{}.com", "a".repeat(64))), None);
    }

    #[test]
    fn emails() {
        assert_eq!(email(" Sam@Acme.COM ").as_deref(), Some("sam@acme.com"));
        assert_eq!(email("first.last+tag@mail.acme.io").as_deref(), Some("first.last+tag@mail.acme.io"));
        for bad in ["", "sam", "sam@", "@acme.com", "sam@acme", "a@b@acme.com", "sam @acme.com", "sam@.com", "sam@acme.", "sam@a..com"] {
            assert_eq!(email(bad), None, "{bad:?}");
        }
        assert_eq!(email(&format!("{}@acme.com", "a".repeat(320))), None);
    }

    #[test]
    fn urls() {
        assert_eq!(http_url(" https://acme.com/x?y=1 ").as_deref(), Some("https://acme.com/x?y=1"));
        assert_eq!(http_url("http://localhost:3000").as_deref(), Some("http://localhost:3000"));
        for bad in ["", "acme.com", "ftp://acme.com", "javascript:alert(1)", "file:///etc/passwd", "https://", "https://a b.com", "mailto:a@b.com"] {
            assert_eq!(http_url(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn tag_lists() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(tags(&v(&[" b2b ", "B2B", "", "saas", "b2b"])).unwrap(), v(&["b2b", "saas"]));
        let many: Vec<String> = (0..21).map(|i| format!("t{i}")).collect();
        assert!(tags(&many).is_err());
        assert_eq!(tags(&many[..20]).unwrap().len(), 20);
        assert!(tags(&["x".repeat(41)]).is_err());
        assert_eq!(tags(&["x".repeat(40)]).unwrap().len(), 1);
    }

    #[test]
    fn source_url_lists() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            source_urls(&v(&["https://a.com/x", " https://a.com/x ", "http://b.org"])).unwrap(),
            v(&["https://a.com/x", "http://b.org"])
        );
        assert!(source_urls(&v(&["ftp://a.com"])).is_err());
        assert!(source_urls(&v(&["not a url"])).is_err());
        let many: Vec<String> = (0..21).map(|i| format!("https://a.com/{i}")).collect();
        assert!(source_urls(&many).is_err());
        assert_eq!(source_urls(&many[..20]).unwrap().len(), 20);
        assert!(source_urls(&[format!("https://a.com/{}", "x".repeat(1000))]).is_err());
    }

    #[test]
    fn company_validation() {
        let c = |f: fn(&mut CompanyInput)| {
            let mut i = CompanyInput::default();
            f(&mut i);
            clean_company(&i, true)
        };
        assert!(c(|i| i.name = Some("  ".into())).is_err());
        assert!(c(|i| i.name = Some("x".repeat(201))).is_err());
        assert!(c(|i| i.domain = Some("nope".into())).is_err());
        assert!(c(|i| i.website = Some("ftp://x.com".into())).is_err());
        assert!(c(|i| i.fit_score = Some(Some(101))).is_err());
        assert!(c(|i| i.fit_score = Some(Some(-1))).is_err());
        assert!(c(|i| i.description = Some("x".repeat(4001))).is_err());
        assert!(c(|i| i.custom = Some(json!([1]))).is_err());
        let m = c(|i| {
            i.name = Some(" Acme ".into());
            i.website = Some("https://www.Acme.com/about".into());
            i.fit_score = Some(Some(100));
        })
        .unwrap();
        assert_eq!((m["name"].as_str(), m["domain"].as_str(), m["fit_score"].as_i64()), (Some("Acme"), Some("acme.com"), Some(100)));
        // a patch does not guess a domain from the website
        let m = clean_company(&CompanyInput { website: Some("https://acme.com".into()), ..Default::default() }, false).unwrap();
        assert!(!m.contains_key("domain"));
        // an empty text field clears
        let m = clean_company(&CompanyInput { industry: Some(" ".into()), ..Default::default() }, false).unwrap();
        assert_eq!(m["industry"], Value::Null);
    }

    #[test]
    fn contact_deal_activity_validation() {
        assert!(clean_contact(&ContactInput { email: Some("nope".into()), ..Default::default() }).is_err());
        assert!(clean_contact(&ContactInput { name: Some("".into()), ..Default::default() }).is_err());
        assert!(clean_contact(&ContactInput { x_handle: Some("a b".into()), ..Default::default() }).is_err());
        let m = clean_contact(&ContactInput { email: Some("A@B.io".into()), x_handle: Some("@sam".into()), ..Default::default() }).unwrap();
        assert_eq!((m["email"].as_str(), m["x_handle"].as_str()), (Some("a@b.io"), Some("sam")));

        assert!(clean_deal(&DealInput { stage: Some("nope".into()), ..Default::default() }).is_err());
        assert!(clean_deal(&DealInput { value_cents: Some(Some(-1)), ..Default::default() }).is_err());
        assert!(clean_deal(&DealInput { currency: Some("US".into()), ..Default::default() }).is_err());
        assert!(clean_deal(&DealInput { next_step: Some("x".repeat(501)), ..Default::default() }).is_err());
        let m = clean_deal(&DealInput { currency: Some("eur".into()), stage: Some("won".into()), ..Default::default() }).unwrap();
        assert_eq!((m["currency"].as_str(), m["stage"].as_str()), (Some("EUR"), Some("won")));

        let a = |f: fn(&mut ActivityInput)| {
            let mut i = ActivityInput { kind: Some("note".into()), summary: Some("hi".into()), ..Default::default() };
            f(&mut i);
            clean_activity(&i)
        };
        assert!(a(|_| {}).is_ok());
        assert!(a(|i| i.kind = Some("sms".into())).is_err());
        assert!(a(|i| i.kind = None).is_err());
        assert!(a(|i| i.summary = Some("x".repeat(501))).is_err());
        assert!(a(|i| i.summary = None).is_err());
        assert!(a(|i| i.body = Some("x".repeat(20_001))).is_err());
        assert!(a(|i| i.url = Some("javascript:1".into())).is_err());
    }

    #[test]
    fn numbers_and_dates_clear_with_null() {
        // absent = untouched, null = clear, a value = set
        let c: CompanyInput = serde_json::from_value(json!({ "name": "Acme" })).unwrap();
        assert_eq!(c.fit_score, None);
        assert!(!clean_company(&c, false).unwrap().contains_key("fit_score"));
        let c: CompanyInput = serde_json::from_value(json!({ "fit_score": null })).unwrap();
        assert_eq!(c.fit_score, Some(None));
        assert_eq!(clean_company(&c, false).unwrap()["fit_score"], Value::Null);
        let c: CompanyInput = serde_json::from_value(json!({ "fit_score": 70 })).unwrap();
        assert_eq!(clean_company(&c, false).unwrap()["fit_score"], json!(70));
        assert!(serde_json::from_value::<CompanyInput>(json!({ "fit_score": "high" })).is_err());

        let d: DealInput = serde_json::from_value(json!({ "value_cents": null, "next_step_at": null })).unwrap();
        assert_eq!((d.value_cents, d.next_step_at), (Some(None), Some(None)));
        let m = clean_deal(&d).unwrap();
        assert_eq!((m["value_cents"].clone(), m["next_step_at"].clone()), (Value::Null, Value::Null));
        let d: DealInput = serde_json::from_value(json!({ "value_cents": 500, "next_step_at": "2026-11-01T10:00:00Z" })).unwrap();
        let m = clean_deal(&d).unwrap();
        assert_eq!((m["value_cents"].as_i64(), m["next_step_at"].as_str()), (Some(500), Some("2026-11-01T10:00:00+00:00")));
        let m = clean_deal(&serde_json::from_value(json!({ "title": "Pilot" })).unwrap()).unwrap();
        assert!(!m.contains_key("value_cents") && !m.contains_key("next_step_at"));
        // clearing needs no range check, setting still has one
        assert!(clean_deal(&DealInput { value_cents: Some(None), ..Default::default() }).is_ok());
        assert!(clean_company(&CompanyInput { fit_score: Some(Some(101)), ..Default::default() }, false).is_err());
    }

    #[test]
    fn kinds() {
        assert_eq!(Kind::parse("companies"), Some(Kind::Company));
        assert_eq!(Kind::parse("deal"), Some(Kind::Deal));
        assert_eq!(Kind::parse("nope"), None);
        assert_eq!(like(&Some("50%_off".into())).as_deref(), Some("%50\\%\\_off%"));
        assert_eq!(like(&Some("  ".into())), None);
    }
}
