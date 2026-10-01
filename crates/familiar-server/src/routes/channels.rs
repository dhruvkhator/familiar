//! Chat channels (Telegram). The bot token is sealed in `config_enc` and never returned.

use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use uuid::Uuid;

use super::{Body, Id, Row, owned};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

const VIEW: &str = "jsonb_build_object('id', c.id, 'kind', c.kind, 'bound', c.chat_id is not null,
    'pair_code', case when c.chat_id is null then c.pair_code end, 'default_bot_id', c.default_bot_id,
    'enabled', c.enabled, 'created_at', c.created_at)";
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

pub async fn list(State(st): State<S>, a: Auth) -> R<Json<Vec<Row>>> {
    let rows = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select {VIEW} from channels c where c.owner_id = $1 order by c.created_at"
    )))
    .bind(a.user)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

fn pair_code() -> String {
    let mut raw = [0u8; 8];
    rand::fill(&mut raw);
    raw.iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect()
}

/// Ask Telegram `getMe`; any failure is reported as a 400 without echoing the token.
async fn check_token(st: &S, token: &str) -> R<()> {
    if token.len() > 200 || !token.contains(':') || token.contains(['/', '?', '#', ' ']) {
        return Err(ApiError::bad("that doesn't look like a Telegram bot token"));
    }
    let resp = st
        .http
        .get(format!("https://api.telegram.org/bot{token}/getMe"))
        .send()
        .await
        .map_err(|_| ApiError::bad("could not reach Telegram to verify the token"))?;
    if !resp.status().is_success() {
        return Err(ApiError::bad("Telegram rejected the bot token"));
    }
    let ok = resp
        .json::<serde_json::Value>()
        .await
        .ok()
        .is_some_and(|v| v["ok"] == true);
    if ok {
        Ok(())
    } else {
        Err(ApiError::bad("Telegram rejected the bot token"))
    }
}

fn seal_token(st: &S, token: &str) -> R<String> {
    st.secret()?
        .encrypt(&serde_json::json!({ "token": token }).to_string())
        .map_err(|_| ApiError::Internal)
}

#[derive(Deserialize)]
pub struct NewChannel {
    kind: String,
    token: String,
    default_bot_id: Option<Uuid>,
}

pub async fn create(
    State(st): State<S>,
    a: Auth,
    Body(b): Body<NewChannel>,
) -> R<(StatusCode, Json<Row>)> {
    super::one_of(&b.kind, "kind", &["telegram"])?;
    st.secret()?;
    let token = b.token.trim();
    if let Some(bot) = b.default_bot_id {
        owned(&st.pool, "bots", bot, a.user).await?;
    }
    check_token(&st, token).await?;
    let enc = seal_token(&st, token)?;
    let row = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "with c as (insert into channels (owner_id, kind, config_enc, pair_code, default_bot_id)
           values ($1, $2, $3, $4, $5) returning *) select {VIEW} from c"
    )))
    .bind(a.user)
    .bind(&b.kind)
    .bind(enc)
    .bind(pair_code())
    .bind(b.default_bot_id)
    .fetch_one(&st.pool)
    .await
    .map_err(|e| match ApiError::from(e) {
        ApiError::Conflict(_) => ApiError::conflict("a channel of this kind already exists"),
        o => o,
    })?;
    Ok((StatusCode::CREATED, Json(row)))
}

#[derive(Deserialize)]
pub struct ChannelPatch {
    token: Option<String>,
    default_bot_id: Option<Uuid>,
    enabled: Option<bool>,
}

pub async fn update(
    State(st): State<S>,
    a: Auth,
    Id(id): Id,
    Body(p): Body<ChannelPatch>,
) -> R<Json<Row>> {
    owned(&st.pool, "channels", id, a.user).await?;
    if let Some(bot) = p.default_bot_id {
        owned(&st.pool, "bots", bot, a.user).await?;
    }
    let enc = match p.token.as_deref().map(str::trim) {
        Some(t) => {
            st.secret()?;
            check_token(&st, t).await?;
            Some(seal_token(&st, t)?)
        }
        None => None,
    };
    let row = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "with c as (update channels set config_enc = coalesce($3, config_enc),
            default_bot_id = coalesce($4, default_bot_id), enabled = coalesce($5, enabled)
          where id = $1 and owner_id = $2 returning *) select {VIEW} from c"
    )))
    .bind(id)
    .bind(a.user)
    .bind(enc)
    .bind(p.default_bot_id)
    .bind(p.enabled)
    .fetch_optional(&st.pool)
    .await?;
    super::found(row)
}

pub async fn remove(State(st): State<S>, a: Auth, Id(id): Id) -> R<StatusCode> {
    let n = sqlx::query("delete from channels where id = $1 and owner_id = $2")
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
