//! The CRM API: companies, contacts and deals (one set of handlers, the kind is in the path), the activity timeline,
//! the pipeline board, the change log with undo, and CSV export / import. All writes go through `familiar_core::crm`,
//! as the owner.

use axum::{
    Json,
    body::Bytes,
    extract::{FromRequestParts, State},
    http::{StatusCode, header},
    response::IntoResponse,
};
use familiar_core::{
    crm::{self, Actor, ActivityFilters, ActivityInput, ChangeFilters, CompanyInput, ContactInput, CrmError, DealInput, Filters, Kind},
    db::Db,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use uuid::Uuid;

use super::{Body, Id, Q};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

impl From<CrmError> for ApiError {
    fn from(e: CrmError) -> Self {
        match e {
            CrmError::Invalid(m) | CrmError::Forbidden(m) => ApiError::bad(m),
            CrmError::NotFound => ApiError::NotFound,
            CrmError::Conflict(m) => ApiError::conflict(m),
            CrmError::Db(e) => e.into(),
        }
    }
}

/// A path with a JSON error when it doesn't parse.
#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(ApiError))]
pub struct P<T>(pub T);

fn db(st: &S, a: &Auth) -> Db {
    Db { pool: st.pool.clone(), owner: a.user }
}

/// `companies`, `contacts` or `deals` (anything else is a 404).
fn kind(s: &str) -> R<Kind> {
    match Kind::parse(s) {
        Some(Kind::Activity) | None => Err(ApiError::NotFound),
        Some(k) if s.ends_with('s') => Ok(k),
        _ => Err(ApiError::NotFound),
    }
}

fn input<T: DeserializeOwned>(v: Value) -> R<T> {
    serde_json::from_value(v).map_err(|e| ApiError::bad(format!("invalid body: {e}")))
}

#[derive(Deserialize)]
pub struct ListQuery {
    q: Option<String>,
    tag: Option<String>,
    stage: Option<String>,
    company_id: Option<Uuid>,
    dnc: Option<bool>,
    sort: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

pub async fn list(State(st): State<S>, a: Auth, P(k): P<String>, Q(q): Q<ListQuery>) -> R<Json<Vec<Value>>> {
    let f = Filters { q: q.q, tag: q.tag, stage: q.stage, company_id: q.company_id, dnc: q.dnc, sort: q.sort };
    Ok(Json(crm::search(&db(&st, &a), kind(&k)?, &f, q.limit, q.offset).await?))
}

/// Create a record, or, when it matches one (same domain, same email, ...), update that one: 201 when it is new, else 200.
pub async fn create(State(st): State<S>, a: Auth, P(k): P<String>, Body(v): Body<Value>) -> R<(StatusCode, Json<Value>)> {
    let (db, kind) = (db(&st, &a), kind(&k)?);
    let (row, created) = match kind {
        Kind::Company => crm::upsert_company(&db, &Actor::User, &input::<CompanyInput>(v)?).await?,
        Kind::Contact => crm::upsert_contact(&db, &Actor::User, &input::<ContactInput>(v)?).await?,
        _ => crm::upsert_deal(&db, &Actor::User, &input::<DealInput>(v)?).await?,
    };
    // the joined names come with a read
    let row = crm::get(&db, kind, row["id"].as_str().and_then(|s| s.parse().ok()).unwrap_or_default()).await?;
    Ok((if created { StatusCode::CREATED } else { StatusCode::OK }, Json(row)))
}

pub async fn get(State(st): State<S>, a: Auth, P((k, id)): P<(String, Uuid)>) -> R<Json<Value>> {
    Ok(Json(crm::get(&db(&st, &a), kind(&k)?, id).await?))
}

pub async fn update(
    State(st): State<S>,
    a: Auth,
    P((k, id)): P<(String, Uuid)>,
    Body(v): Body<Value>,
) -> R<Json<Value>> {
    let (db, kind) = (db(&st, &a), kind(&k)?);
    match kind {
        Kind::Company => crm::patch_company(&db, &Actor::User, id, &input::<CompanyInput>(v)?).await?,
        Kind::Contact => crm::patch_contact(&db, &Actor::User, id, &input::<ContactInput>(v)?).await?,
        _ => crm::patch_deal(&db, &Actor::User, id, &input::<DealInput>(v)?).await?,
    };
    Ok(Json(crm::get(&db, kind, id).await?))
}

pub async fn remove(State(st): State<S>, a: Auth, P((k, id)): P<(String, Uuid)>) -> R<StatusCode> {
    let db = db(&st, &a);
    match kind(&k)? {
        Kind::Company => crm::soft_delete_company(&db, &Actor::User, id).await?,
        Kind::Contact => crm::soft_delete_contact(&db, &Actor::User, id).await?,
        _ => crm::soft_delete_deal(&db, &Actor::User, id).await?,
    };
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct ActivityQuery {
    company_id: Option<Uuid>,
    contact_id: Option<Uuid>,
    deal_id: Option<Uuid>,
    limit: Option<i64>,
    offset: Option<i64>,
}

pub async fn activities(State(st): State<S>, a: Auth, Q(q): Q<ActivityQuery>) -> R<Json<Vec<Value>>> {
    let f = ActivityFilters { company_id: q.company_id, contact_id: q.contact_id, deal_id: q.deal_id };
    Ok(Json(crm::activities(&db(&st, &a), &f, q.limit, q.offset).await?))
}

pub async fn log_activity(State(st): State<S>, a: Auth, Body(i): Body<ActivityInput>) -> R<(StatusCode, Json<Value>)> {
    Ok((StatusCode::CREATED, Json(crm::log_activity(&db(&st, &a), &Actor::User, &i).await?)))
}

pub async fn pipeline(State(st): State<S>, a: Auth) -> R<Json<Vec<Value>>> {
    Ok(Json(crm::pipeline(&db(&st, &a)).await?))
}

#[derive(Deserialize)]
pub struct ChangeQuery {
    entity: Option<String>,
    entity_id: Option<Uuid>,
    bot_id: Option<Uuid>,
    limit: Option<i64>,
}

pub async fn changes(State(st): State<S>, a: Auth, Q(q): Q<ChangeQuery>) -> R<Json<Vec<Value>>> {
    let f = ChangeFilters { entity: q.entity, entity_id: q.entity_id, bot_id: q.bot_id };
    Ok(Json(crm::changes(&db(&st, &a), &f, q.limit).await?))
}

/// Undo a change (only the newest of its record: else 409); answers with the new `undo` change.
pub async fn undo(State(st): State<S>, a: Auth, Id(id): Id) -> R<Json<Value>> {
    Ok(Json(crm::undo(&db(&st, &a), id).await?))
}

#[derive(Deserialize)]
pub struct KindQuery {
    kind: String,
    dry_run: Option<bool>,
}

pub async fn export(State(st): State<S>, a: Auth, Q(q): Q<KindQuery>) -> R<impl IntoResponse> {
    let kind = kind(&q.kind).map_err(|_| ApiError::bad("kind must be companies, contacts or deals"))?;
    let db = db(&st, &a);
    let csv = crm::csv::export(kind, &crm::all(&db, kind).await?);
    let name = q.kind;
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}.csv\"")),
        ],
        csv,
    ))
}

/// The CSV is the request body (at most 5 MB and 5000 rows). `dry_run=true` reports what would happen and changes nothing.
pub async fn import(State(st): State<S>, a: Auth, Q(q): Q<KindQuery>, body: Bytes) -> R<Json<crm::csv::ImportResult>> {
    let kind = kind(&q.kind).map_err(|_| ApiError::bad("kind must be companies, contacts or deals"))?;
    if body.len() > crm::csv::MAX_BYTES {
        return Err(ApiError::bad("the CSV is too large (max 5 MB)"));
    }
    let text = std::str::from_utf8(&body).map_err(|_| ApiError::bad("the CSV must be UTF-8 text"))?;
    Ok(Json(crm::csv::import(&db(&st, &a), kind, text, q.dry_run.unwrap_or(false)).await?))
}
