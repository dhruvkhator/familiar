//! MCP connectors. Secrets (`env` / `headers` maps) are AES-GCM sealed in `secrets_enc` and never returned;
//! responses carry `has_secrets` and `secret_names` ({env:[..], headers:[..]}) only.

use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use uuid::Uuid;

use super::{Body, Id, Row, owned};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

const PRESETS: &str = include_str!("../presets.json");
const VIEW: &str = "to_jsonb(c) - 'owner_id' - 'secrets_enc', c.secrets_enc";

pub async fn presets(_a: Auth) -> R<Json<Value>> {
    serde_json::from_str(PRESETS)
        .map(Json)
        .map_err(|_| ApiError::Internal)
}

#[derive(Deserialize, Default)]
pub struct Secrets {
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

fn valid_name(n: &str) -> R<String> {
    let b = n.as_bytes();
    let ok = (1..=32).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-');
    if !ok || n == "familiar" || n == "browser" {
        return Err(ApiError::bad(
            "name must match ^[a-z0-9][a-z0-9_-]{0,31}$ and not be 'familiar' or 'browser'",
        ));
    }
    Ok(n.to_string())
}

fn transport(t: &str) -> R<String> {
    super::one_of(t, "transport", &["stdio", "http"])
}

fn check_url(u: &str) -> R<String> {
    let u = u.trim();
    match url::Url::parse(u) {
        Ok(p) if matches!(p.scheme(), "http" | "https") => Ok(u.to_string()),
        _ => Err(ApiError::bad("url must be an http(s) URL")),
    }
}

/// Seal secrets; `None` when there is nothing to store (then no key is needed).
fn seal(st: &S, s: &Secrets) -> R<Option<String>> {
    if s.env.is_empty() && s.headers.is_empty() {
        return Ok(None);
    }
    if s.env
        .keys()
        .any(|k| k.is_empty() || k.len() > 128 || k.contains(['=', ' ', '\0']))
        || s.headers
            .keys()
            .any(|k| k.is_empty() || k.len() > 128 || k.contains([':', ' ', '\r', '\n']))
    {
        return Err(ApiError::bad("invalid secret key name"));
    }
    let plain = json!({ "env": s.env, "headers": s.headers }).to_string();
    st.secret()?
        .encrypt(&plain)
        .map(Some)
        .map_err(|_| ApiError::Internal)
}

fn view(st: &S, row: Row, enc: Option<String>) -> Value {
    let mut row = row.0;
    let mut names = json!({ "env": [], "headers": [] });
    if let Some(plain) = enc
        .as_deref()
        .zip(st.secret.as_ref())
        .and_then(|(e, b)| b.decrypt(e).ok())
        && let Ok(v) = serde_json::from_str::<Value>(&plain)
    {
        for k in ["env", "headers"] {
            names[k] = json!(
                v[k].as_object()
                    .map(|m| m.keys().collect::<Vec<_>>())
                    .unwrap_or_default()
            );
        }
    }
    row["has_secrets"] = json!(enc.is_some());
    row["secret_names"] = names;
    row
}

type Fetched = (Row, Option<String>);

async fn linked_views(st: &S, owner: Uuid, bot: Uuid) -> R<Json<Vec<Value>>> {
    let rows: Vec<Fetched> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {VIEW} from connectors c join bot_connectors bc on bc.connector_id = c.id
         where bc.bot_id = $1 and c.owner_id = $2 order by c.name"
    )))
    .bind(bot)
    .bind(owner)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(
        rows.into_iter().map(|(r, e)| view(st, r, e)).collect(),
    ))
}

pub async fn list(State(st): State<S>, a: Auth) -> R<Json<Vec<Value>>> {
    let rows: Vec<Fetched> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {VIEW} from connectors c where c.owner_id = $1 order by c.name"
    )))
    .bind(a.user)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(
        rows.into_iter().map(|(r, e)| view(&st, r, e)).collect(),
    ))
}

#[derive(Deserialize)]
pub struct NewConnector {
    name: String,
    preset: Option<String>,
    transport: String,
    command: Option<String>,
    args: Option<Vec<String>>,
    url: Option<String>,
    secrets: Option<Secrets>,
    enabled: Option<bool>,
}

pub async fn create(
    State(st): State<S>,
    a: Auth,
    Body(b): Body<NewConnector>,
) -> R<(StatusCode, Json<Value>)> {
    let name = valid_name(b.name.trim())?;
    let transport = transport(&b.transport)?;
    let command = b
        .command
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());
    let url = b
        .url
        .as_deref()
        .filter(|u| !u.trim().is_empty())
        .map(check_url)
        .transpose()?;
    match transport.as_str() {
        "stdio" if command.is_none() => {
            return Err(ApiError::bad("command is required for stdio connectors"));
        }
        "http" if url.is_none() => {
            return Err(ApiError::bad("url is required for http connectors"));
        }
        _ => {}
    }
    let enc = b
        .secrets
        .as_ref()
        .map(|s| seal(&st, s))
        .transpose()?
        .flatten();
    let row: Fetched = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "with c as (insert into connectors (owner_id, name, preset, transport, command, args, url, secrets_enc, enabled)
           values ($1, $2, $3, $4, $5, $6, $7, $8, $9) returning *)
         select {VIEW} from c"
    )))
    .bind(a.user)
    .bind(&name)
    .bind(b.preset)
    .bind(&transport)
    .bind(command)
    .bind(sqlx::types::Json(b.args.unwrap_or_default()))
    .bind(url)
    .bind(enc)
    .bind(b.enabled.unwrap_or(true))
    .fetch_one(&st.pool)
    .await
    .map_err(|e| match ApiError::from(e) {
        ApiError::Conflict(_) => ApiError::conflict(format!("a connector named '{name}' already exists")),
        o => o,
    })?;
    Ok((StatusCode::CREATED, Json(view(&st, row.0, row.1))))
}

#[derive(Deserialize)]
pub struct ConnectorPatch {
    name: Option<String>,
    preset: Option<String>,
    transport: Option<String>,
    command: Option<String>,
    args: Option<Vec<String>>,
    url: Option<String>,
    secrets: Option<Secrets>,
    enabled: Option<bool>,
}

pub async fn update(
    State(st): State<S>,
    a: Auth,
    Id(id): Id,
    Body(p): Body<ConnectorPatch>,
) -> R<Json<Value>> {
    let name = p
        .name
        .as_deref()
        .map(|n| valid_name(n.trim()))
        .transpose()?;
    let transport = p.transport.as_deref().map(transport).transpose()?;
    let url = p.url.as_deref().map(check_url).transpose()?;
    let replace = p.secrets.is_some();
    let enc = p
        .secrets
        .as_ref()
        .map(|s| seal(&st, s))
        .transpose()?
        .flatten();
    let row: Option<Fetched> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "with c as (update connectors set name = coalesce($3, name), preset = coalesce($4, preset),
            transport = coalesce($5, transport), command = coalesce($6, command), args = coalesce($7, args),
            url = coalesce($8, url), secrets_enc = case when $9 then $10 else secrets_enc end,
            enabled = coalesce($11, enabled)
          where id = $1 and owner_id = $2 returning *)
         select {VIEW} from c"
    )))
    .bind(id)
    .bind(a.user)
    .bind(name)
    .bind(p.preset)
    .bind(transport)
    .bind(p.command)
    .bind(p.args.map(sqlx::types::Json))
    .bind(url)
    .bind(replace)
    .bind(enc)
    .bind(p.enabled)
    .fetch_optional(&st.pool)
    .await?;
    let (r, e) = row.ok_or(ApiError::NotFound)?;
    Ok(Json(view(&st, r, e)))
}

pub async fn remove(State(st): State<S>, a: Auth, Id(id): Id) -> R<StatusCode> {
    let n = sqlx::query("delete from connectors where id = $1 and owner_id = $2")
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

pub async fn for_bot(State(st): State<S>, a: Auth, Id(bot): Id) -> R<Json<Vec<Value>>> {
    owned(&st.pool, "bots", bot, a.user).await?;
    linked_views(&st, a.user, bot).await
}

#[derive(Deserialize)]
pub struct LinkSet {
    connector_ids: Vec<Uuid>,
}

/// Replace the bot's connector set.
pub async fn set_for_bot(
    State(st): State<S>,
    a: Auth,
    Id(bot): Id,
    Body(b): Body<LinkSet>,
) -> R<Json<Vec<Value>>> {
    owned(&st.pool, "bots", bot, a.user).await?;
    let mut ids = b.connector_ids;
    ids.sort();
    ids.dedup();
    let mut tx = st.pool.begin().await?;
    let found: i64 =
        sqlx::query_scalar("select count(*) from connectors where owner_id = $1 and id = any($2)")
            .bind(a.user)
            .bind(&ids)
            .fetch_one(&mut *tx)
            .await?;
    if found != ids.len() as i64 {
        return Err(ApiError::NotFound);
    }
    sqlx::query("delete from bot_connectors where bot_id = $1")
        .bind(bot)
        .execute(&mut *tx)
        .await?;
    sqlx::query("insert into bot_connectors (bot_id, connector_id) select $1, unnest($2::uuid[])")
        .bind(bot)
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    linked_views(&st, a.user, bot).await
}
