use axum::{Json, body::Bytes, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{Body, Id, Q, Row, clamp, found, owned, parse_ts, text};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

const THREAD: &str = "to_jsonb(t) - 'owner_id' - 'claude_session_id' - 'codex_thread_id'";

pub async fn list(State(st): State<S>, a: Auth, Id(bot): Id) -> R<Json<Vec<Row>>> {
    owned(&st.pool, "bots", bot, a.user).await?;
    let rows = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select {THREAD} from threads t where t.bot_id = $1 and t.owner_id = $2 order by t.updated_at desc limit 200"
    )))
    .bind(bot)
    .bind(a.user)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct NewThread {
    title: Option<String>,
}

pub async fn create(
    State(st): State<S>,
    a: Auth,
    Id(bot): Id,
    body: Bytes,
) -> R<(StatusCode, Json<Row>)> {
    // body is optional: no body (or `{}`) makes "New thread"
    let body: NewThread = if body.iter().all(u8::is_ascii_whitespace) {
        NewThread { title: None }
    } else {
        serde_json::from_slice(&body).map_err(|e| ApiError::bad(e.to_string()))?
    };
    let title = body
        .title
        .filter(|t| !t.trim().is_empty())
        .map(|t| text(&t, "title", 200))
        .transpose()?;
    let row = sqlx::query_scalar(
        "insert into threads (owner_id, bot_id, title)
         select $2, b.id, coalesce($3, 'New thread') from bots b where b.id = $1 and b.owner_id = $2
         returning to_jsonb(threads) - 'owner_id' - 'claude_session_id' - 'codex_thread_id'",
    )
    .bind(bot)
    .bind(a.user)
    .bind(title)
    .fetch_optional(&st.pool)
    .await?;
    Ok((StatusCode::CREATED, found(row)?))
}

#[derive(Deserialize)]
pub struct Rename {
    title: String,
}

pub async fn rename(
    State(st): State<S>,
    a: Auth,
    Id(id): Id,
    Body(b): Body<Rename>,
) -> R<Json<Row>> {
    let title = text(&b.title, "title", 200)?;
    let row = sqlx::query_scalar(
        "update threads set title = $3, updated_at = now() where id = $1 and owner_id = $2
         returning to_jsonb(threads) - 'owner_id' - 'claude_session_id' - 'codex_thread_id'",
    )
    .bind(id)
    .bind(a.user)
    .bind(title)
    .fetch_optional(&st.pool)
    .await?;
    found(row)
}

pub async fn remove(State(st): State<S>, a: Auth, Id(id): Id) -> R<StatusCode> {
    let n = sqlx::query("delete from threads where id = $1 and owner_id = $2")
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

#[derive(Deserialize)]
pub struct MessageQuery {
    before: Option<String>,
    limit: Option<i64>,
}

/// The newest `limit` messages older than `before` (RFC3339 `created_at`), returned oldest first.
pub async fn messages(
    State(st): State<S>,
    a: Auth,
    Id(id): Id,
    Q(q): Q<MessageQuery>,
) -> R<Json<Vec<Row>>> {
    owned(&st.pool, "threads", id, a.user).await?;
    let before: Option<DateTime<Utc>> = q.before.as_deref().map(parse_ts).transpose()?;
    let rows = sqlx::query_scalar(
        "select to_jsonb(m) - 'owner_id' from (
            select * from messages where thread_id = $1 and owner_id = $2 and ($3::timestamptz is null or created_at < $3)
            order by created_at desc, id desc limit $4) m
         order by m.created_at, m.id",
    )
    .bind(id)
    .bind(a.user)
    .bind(before)
    .bind(clamp(q.limit, 100, 500))
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct NewMessage {
    content: String,
}

/// Inserting a user message queues a chat run via the DB trigger.
pub async fn post_message(
    State(st): State<S>,
    a: Auth,
    Id(id): Id,
    Body(b): Body<NewMessage>,
) -> R<(StatusCode, Json<Row>)> {
    if b.content.trim().is_empty() {
        return Err(ApiError::bad("content must not be empty"));
    }
    if b.content.chars().count() > 100_000 {
        return Err(ApiError::bad("content too long (max 100000 characters)"));
    }
    let row = sqlx::query_scalar(
        "insert into messages (owner_id, thread_id, role, content)
         select $2, t.id, 'user', $3 from threads t where t.id = $1 and t.owner_id = $2
         returning to_jsonb(messages) - 'owner_id'",
    )
    .bind(id)
    .bind(a.user)
    .bind(b.content)
    .fetch_optional(&st.pool)
    .await?;
    Ok((StatusCode::CREATED, found(row)?))
}
