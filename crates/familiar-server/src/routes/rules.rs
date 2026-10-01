use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use uuid::Uuid;

use super::{Body, Id, Q, Row, found, one_of, owned, text};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

#[derive(Deserialize)]
pub struct ListQuery {
    bot_id: Option<Uuid>,
    all: Option<String>,
}

/// No `bot_id` = global rules only; `bot_id` = that bot's own rules only; `all=1` = everything.
pub async fn list(State(st): State<S>, a: Auth, Q(q): Q<ListQuery>) -> R<Json<Vec<Row>>> {
    let all = matches!(q.all.as_deref(), Some("1" | "true"));
    let rows = sqlx::query_scalar(
        "select to_jsonb(r) - 'owner_id' from rules r
         where r.owner_id = $1 and ($2 or r.bot_id is not distinct from $3::uuid)
         order by r.created_at, r.id",
    )
    .bind(a.user)
    .bind(all)
    .bind(q.bot_id)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct NewRule {
    bot_id: Option<Uuid>,
    pattern: String,
    decision: String,
    note: Option<String>,
}

pub async fn create(
    State(st): State<S>,
    a: Auth,
    Body(b): Body<NewRule>,
) -> R<(StatusCode, Json<Row>)> {
    let pattern = text(&b.pattern, "pattern", 500)?;
    let decision = one_of(&b.decision, "decision", &["allow", "deny", "ask", "review"])?;
    let note = b
        .note
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    if note.as_deref().is_some_and(|n| n.chars().count() > 1000) {
        return Err(ApiError::bad("note too long (max 1000)"));
    }
    if let Some(bot) = b.bot_id {
        owned(&st.pool, "bots", bot, a.user).await?;
    }
    let row = sqlx::query_scalar(
        "insert into rules (owner_id, bot_id, pattern, decision, note) values ($1, $2, $3, $4, $5)
         returning to_jsonb(rules) - 'owner_id'",
    )
    .bind(a.user)
    .bind(b.bot_id)
    .bind(pattern)
    .bind(decision)
    .bind(note)
    .fetch_optional(&st.pool)
    .await?;
    Ok((StatusCode::CREATED, found(row)?))
}

pub async fn remove(State(st): State<S>, a: Auth, Id(id): Id) -> R<StatusCode> {
    let n = sqlx::query("delete from rules where id = $1 and owner_id = $2")
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
