//! CRM webhooks, owner only (a teammate never reaches the API): other CRMs get every change as a signed POST (see
//! `familiar_core::crm::webhooks`). The signing secret is made here, returned once (at create) and stored sealed with
//! `SecretBox`; it is never returned again. The URL is checked when saved, DNS included, and again at every delivery.

use axum::{Json, extract::State, http::StatusCode};
use familiar_core::{crm::webhooks, db::Db};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Body, Id, Q, Row, clamp};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

/// A webhook without its secret, with its newest delivery.
const VIEW: &str = "select (to_jsonb(w) - 'owner_id' - 'secret_enc') || jsonb_build_object('last_delivery',
       (select jsonb_build_object('id', d.id, 'event', d.event, 'status', d.status, 'attempts', d.attempts,
                                  'last_error', d.last_error, 'created_at', d.created_at, 'delivered_at', d.delivered_at)
        from crm_webhook_deliveries d where d.webhook_id = w.id order by d.created_at desc limit 1))
     from crm_webhooks w";

/// The URL when it may receive webhooks: the address rules, after DNS.
async fn url_ok(u: &str) -> R<String> {
    let parsed = webhooks::check_url(u).map_err(ApiError::bad)?;
    webhooks::resolve(&parsed).await.map_err(ApiError::bad)?;
    Ok(u.trim().to_string())
}

async fn view(st: &S, owner: Uuid, id: Uuid) -> R<Value> {
    let row: Option<Row> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("{VIEW} where w.id = $1 and w.owner_id = $2")))
        .bind(id)
        .bind(owner)
        .fetch_optional(&st.pool)
        .await?;
    row.map(|r| r.0).ok_or(ApiError::NotFound)
}

pub async fn list(State(st): State<S>, a: Auth) -> R<Json<Vec<Row>>> {
    let rows = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("{VIEW} where w.owner_id = $1 order by w.created_at")))
        .bind(a.user)
        .fetch_all(&st.pool)
        .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct NewWebhook {
    url: String,
    events: Vec<String>,
    enabled: Option<bool>,
}

/// 201 with the webhook and its `secret`: the only time the secret is shown.
pub async fn create(State(st): State<S>, a: Auth, Body(b): Body<NewWebhook>) -> R<(StatusCode, Json<Value>)> {
    let events = webhooks::events(&b.events).map_err(ApiError::bad)?;
    let url = url_ok(&b.url).await?;
    let secret = webhooks::new_secret();
    let sealed = st.secret()?.encrypt(&secret).map_err(|_| ApiError::Internal)?;
    let mut tx = st.pool.begin().await?;
    // one at a time per owner, so the cap holds
    sqlx::query("select 1 from users where id = $1 for update").bind(a.user).execute(&mut *tx).await?;
    let n: i64 = sqlx::query_scalar("select count(*) from crm_webhooks where owner_id = $1")
        .bind(a.user)
        .fetch_one(&mut *tx)
        .await?;
    if n >= webhooks::MAX_WEBHOOKS {
        return Err(ApiError::bad(format!("at most {} webhooks", webhooks::MAX_WEBHOOKS)));
    }
    let id: Uuid = sqlx::query_scalar(
        "insert into crm_webhooks (owner_id, url, secret_enc, events, enabled) values ($1, $2, $3, $4, $5) returning id",
    )
    .bind(a.user)
    .bind(&url)
    .bind(sealed)
    .bind(&events)
    .bind(b.enabled.unwrap_or(true))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut row = view(&st, a.user, id).await?;
    row["secret"] = json!(secret);
    Ok((StatusCode::CREATED, Json(row)))
}

pub async fn get(State(st): State<S>, a: Auth, Id(id): Id) -> R<Json<Value>> {
    Ok(Json(view(&st, a.user, id).await?))
}

#[derive(Deserialize)]
pub struct WebhookPatch {
    url: Option<String>,
    events: Option<Vec<String>>,
    enabled: Option<bool>,
}

/// Turning a webhook off fails the deliveries still waiting (nothing is sent later by surprise).
pub async fn update(State(st): State<S>, a: Auth, Id(id): Id, Body(p): Body<WebhookPatch>) -> R<Json<Value>> {
    let events = p.events.as_deref().map(webhooks::events).transpose().map_err(ApiError::bad)?;
    let url = match p.url.as_deref() {
        Some(u) => Some(url_ok(u).await?),
        None => None,
    };
    let mut tx = st.pool.begin().await?;
    let n = sqlx::query(
        "update crm_webhooks set url = coalesce($3, url), events = coalesce($4, events), enabled = coalesce($5, enabled)
         where id = $1 and owner_id = $2",
    )
    .bind(id)
    .bind(a.user)
    .bind(url)
    .bind(events)
    .bind(p.enabled)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    if p.enabled == Some(false) {
        sqlx::query(
            "update crm_webhook_deliveries set status = 'failed', last_error = 'the webhook was turned off'
             where webhook_id = $1 and owner_id = $2 and status = 'pending'",
        )
        .bind(id)
        .bind(a.user)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Json(view(&st, a.user, id).await?))
}

pub async fn remove(State(st): State<S>, a: Auth, Id(id): Id) -> R<StatusCode> {
    let n = sqlx::query("delete from crm_webhooks where id = $1 and owner_id = $2")
        .bind(id)
        .bind(a.user)
        .execute(&st.pool)
        .await?
        .rows_affected();
    if n == 0 { Err(ApiError::NotFound) } else { Ok(StatusCode::NO_CONTENT) }
}

/// Send a `ping` now and answer with its delivery (`status` delivered or failed, `last_error`). Never retried.
pub async fn test(State(st): State<S>, a: Auth, Id(id): Id) -> R<Json<Value>> {
    let db = Db { pool: st.pool.clone(), owner: a.user };
    Ok(Json(webhooks::test(&db, st.secret()?, id).await?))
}

#[derive(Deserialize)]
pub struct DeliveryQuery {
    limit: Option<i64>,
}

/// Newest first (default 50, at most 200), with their payloads.
pub async fn deliveries(State(st): State<S>, a: Auth, Id(id): Id, Q(q): Q<DeliveryQuery>) -> R<Json<Vec<Row>>> {
    super::owned(&st.pool, "crm_webhooks", id, a.user).await?;
    let rows = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "{} where d.webhook_id = $1 and d.owner_id = $2 order by d.created_at desc, d.id limit $3",
        webhooks::DELIVERY_VIEW
    )))
    .bind(id)
    .bind(a.user)
    .bind(clamp(q.limit, 50, 200))
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}
