//! Webhook triggers. The URL secret is shown once (create/rotate); only its sha256 is stored.

use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::{Body, Id, Row, found, one_of, owned, text};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

const KINDS: [&str; 2] = ["scheduled", "proactive"];
const VIEW: &str = "to_jsonb(t) - 'owner_id' - 'token_hash'";
const PER_MINUTE: usize = 30;

fn new_token() -> (String, String) {
    let mut raw = [0u8; 32];
    rand::fill(&mut raw);
    let token = URL_SAFE_NO_PAD.encode(raw);
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    (token, hash)
}

fn hook_url(st: &S, headers: &HeaderMap, token: &str) -> String {
    let base = st.public_url.clone().unwrap_or_else(|| {
        let host = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("localhost");
        let scheme = headers
            .get("x-forwarded-proto")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("http");
        format!("{scheme}://{host}")
    });
    format!("{base}/hooks/{token}")
}

pub async fn list(State(st): State<S>, a: Auth, Id(bot): Id) -> R<Json<Vec<Row>>> {
    owned(&st.pool, "bots", bot, a.user).await?;
    let rows = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select {VIEW} from triggers t where t.bot_id = $1 and t.owner_id = $2 order by t.created_at"
    )))
    .bind(bot)
    .bind(a.user)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct NewTrigger {
    name: String,
    prompt: String,
    kind: Option<String>,
}

pub async fn create(
    State(st): State<S>,
    a: Auth,
    headers: HeaderMap,
    Id(bot): Id,
    Body(b): Body<NewTrigger>,
) -> R<(StatusCode, Json<Value>)> {
    let name = text(&b.name, "name", 100)?;
    let prompt = text(&b.prompt, "prompt", 20_000)?;
    let kind = one_of(b.kind.as_deref().unwrap_or("scheduled"), "kind", &KINDS)?;
    owned(&st.pool, "bots", bot, a.user).await?;
    let (token, hash) = new_token();
    let mut tx = st.pool.begin().await?;
    let thread: Uuid = sqlx::query_scalar(
        "insert into threads (owner_id, bot_id, title, source) values ($1, $2, $3, 'trigger') returning id",
    )
    .bind(a.user)
    .bind(bot)
    .bind(format!("Webhook: {name}"))
    .fetch_one(&mut *tx)
    .await?;
    let mut row: Value = sqlx::query_scalar::<_, Row>(sqlx::AssertSqlSafe(format!(
        "with t as (insert into triggers (owner_id, bot_id, thread_id, name, token_hash, prompt, kind)
           values ($1, $2, $3, $4, $5, $6, $7) returning *) select {VIEW} from t"
    )))
    .bind(a.user)
    .bind(bot)
    .bind(thread)
    .bind(name)
    .bind(hash)
    .bind(prompt)
    .bind(kind)
    .fetch_one(&mut *tx)
    .await?
    .0;
    tx.commit().await?;
    row["url"] = json!(hook_url(&st, &headers, &token));
    Ok((StatusCode::CREATED, Json(row)))
}

#[derive(Deserialize)]
pub struct TriggerPatch {
    name: Option<String>,
    prompt: Option<String>,
    kind: Option<String>,
    enabled: Option<bool>,
}

pub async fn update(
    State(st): State<S>,
    a: Auth,
    Id(id): Id,
    Body(p): Body<TriggerPatch>,
) -> R<Json<Row>> {
    let name = p
        .name
        .as_deref()
        .map(|n| text(n, "name", 100))
        .transpose()?;
    let prompt = p
        .prompt
        .as_deref()
        .map(|n| text(n, "prompt", 20_000))
        .transpose()?;
    let kind = p
        .kind
        .as_deref()
        .map(|k| one_of(k, "kind", &KINDS))
        .transpose()?;
    let row = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "with t as (update triggers set name = coalesce($3, name), prompt = coalesce($4, prompt),
            kind = coalesce($5, kind), enabled = coalesce($6, enabled)
          where id = $1 and owner_id = $2 returning *) select {VIEW} from t"
    )))
    .bind(id)
    .bind(a.user)
    .bind(name)
    .bind(prompt)
    .bind(kind)
    .bind(p.enabled)
    .fetch_optional(&st.pool)
    .await?;
    found(row)
}

pub async fn remove(State(st): State<S>, a: Auth, Id(id): Id) -> R<StatusCode> {
    let n = sqlx::query("delete from triggers where id = $1 and owner_id = $2")
        .bind(id)
        .bind(a.user)
        .execute(&st.pool)
        .await?
        .rows_affected();
    if n == 0 {
        Err(ApiError::NotFound)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

/// New URL secret; the old URL stops working immediately.
pub async fn rotate(
    State(st): State<S>,
    a: Auth,
    headers: HeaderMap,
    Id(id): Id,
) -> R<Json<Value>> {
    let (token, hash) = new_token();
    let row: Option<Row> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "with t as (update triggers set token_hash = $3 where id = $1 and owner_id = $2 returning *)
         select {VIEW} from t"
    )))
    .bind(id)
    .bind(a.user)
    .bind(hash)
    .fetch_optional(&st.pool)
    .await?;
    let mut row = row.ok_or(ApiError::NotFound)?.0;
    row["url"] = json!(hook_url(&st, &headers, &token));
    Ok(Json(row))
}

/// Public webhook endpoint: the token in the URL is the credential.
#[allow(clippy::type_complexity)]
pub async fn fire(
    State(st): State<S>,
    axum::extract::Path(token): axum::extract::Path<String>,
    body: Bytes,
) -> R<(StatusCode, Json<Value>)> {
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    let t: Option<(Uuid, Uuid, Uuid, Option<Uuid>, String, String, bool)> = sqlx::query_as(
        "select id, owner_id, bot_id, thread_id, prompt, kind, enabled from triggers where token_hash = $1",
    )
    .bind(&hash)
    .fetch_optional(&st.pool)
    .await?;
    let (id, owner, bot, thread, prompt, kind, _) = t.filter(|t| t.6).ok_or(ApiError::NotFound)?;

    {
        let mut m = st.hooks.lock().unwrap();
        let now = Instant::now();
        m.retain(|_, q| {
            q.back()
                .is_some_and(|t| now.duration_since(*t) < Duration::from_secs(60))
        });
        let q = m.entry(id).or_default();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            q.pop_front();
        }
        if q.len() >= PER_MINUTE {
            return Err(ApiError::TooMany);
        }
        q.push_back(now);
    }

    let payload = String::from_utf8_lossy(&body);
    let full = format!("{prompt}\n\n--- webhook payload ---\n{payload}");
    let mut tx = st.pool.begin().await?;
    let thread = match thread {
        Some(t) => t,
        None => {
            // the thread was deleted: start a fresh one
            let t: Uuid = sqlx::query_scalar(
                "insert into threads (owner_id, bot_id, title, source) values ($1, $2, 'Webhook', 'trigger') returning id",
            )
            .bind(owner)
            .bind(bot)
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query("update triggers set thread_id = $2 where id = $1")
                .bind(id)
                .bind(t)
                .execute(&mut *tx)
                .await?;
            t
        }
    };
    let run: Uuid = sqlx::query_scalar(
        "insert into runs (owner_id, bot_id, thread_id, kind, prompt, status) values ($1, $2, $3, $4, $5, 'queued') returning id",
    )
    .bind(owner)
    .bind(bot)
    .bind(thread)
    .bind(kind)
    .bind(full)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("update triggers set last_fired_at = now() where id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "run_id": run }))))
}
