use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;

use super::{Body, Id, Q, Row, found, one_of, owned, text};

const STATUSES: [&str; 3] = ["active", "proposed", "rejected"];
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

#[derive(Deserialize)]
pub struct ListQuery {
    status: Option<String>,
}

/// All statuses by default; proposed first, then newest.
pub async fn list(
    State(st): State<S>,
    a: Auth,
    Id(bot): Id,
    Q(q): Q<ListQuery>,
) -> R<Json<Vec<Row>>> {
    owned(&st.pool, "bots", bot, a.user).await?;
    if let Some(s) = &q.status {
        one_of(s, "status", &STATUSES)?;
    }
    let rows = sqlx::query_scalar(
        "select to_jsonb(m) - 'owner_id' from memories m where m.bot_id = $1 and m.owner_id = $2
           and ($3::text is null or m.status = $3)
         order by (m.status = 'proposed') desc, m.created_at desc, m.id",
    )
    .bind(bot)
    .bind(a.user)
    .bind(q.status)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct MemoryPatch {
    content: Option<String>,
    status: Option<String>,
}

pub async fn update(
    State(st): State<S>,
    a: Auth,
    Id(id): Id,
    Body(p): Body<MemoryPatch>,
) -> R<Json<Row>> {
    let content = p
        .content
        .as_deref()
        .map(|c| text(c, "content", 2000))
        .transpose()?;
    let status = p
        .status
        .as_deref()
        .map(|s| one_of(s, "status", &STATUSES))
        .transpose()?;
    let row = sqlx::query_scalar(
        "update memories set content = coalesce($3, content), status = coalesce($4, status), updated_at = now()
         where id = $1 and owner_id = $2 returning to_jsonb(memories) - 'owner_id'",
    )
    .bind(id)
    .bind(a.user)
    .bind(content)
    .bind(status)
    .fetch_optional(&st.pool)
    .await?;
    found(row)
}

#[derive(Deserialize)]
pub struct NewMemory {
    content: String,
}

pub async fn create(
    State(st): State<S>,
    a: Auth,
    Id(bot): Id,
    Body(b): Body<NewMemory>,
) -> R<(StatusCode, Json<Row>)> {
    let content = text(&b.content, "content", 10_000)?;
    let row = sqlx::query_scalar(
        "insert into memories (owner_id, bot_id, content, source)
         select $2, x.id, $3, 'user' from bots x where x.id = $1 and x.owner_id = $2
         returning to_jsonb(memories) - 'owner_id'",
    )
    .bind(bot)
    .bind(a.user)
    .bind(content)
    .fetch_optional(&st.pool)
    .await?;
    Ok((StatusCode::CREATED, found(row)?))
}

pub async fn remove(State(st): State<S>, a: Auth, Id(id): Id) -> R<StatusCode> {
    let n = sqlx::query("delete from memories where id = $1 and owner_id = $2")
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
