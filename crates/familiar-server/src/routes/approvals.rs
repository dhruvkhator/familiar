use axum::{Json, extract::State};
use serde::Deserialize;
use uuid::Uuid;

use super::{Body, Id, Q, Row, one_of};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

const WITH_BOT: &str =
    "(to_jsonb(a) - 'owner_id') || jsonb_build_object('bot_name', b.name, 'bot_slug', b.slug)";

#[derive(Deserialize)]
pub struct ListQuery {
    status: Option<String>,
    bot_id: Option<Uuid>,
}

/// Newest first, max 200. No `status` = every status.
pub async fn list(State(st): State<S>, a: Auth, Q(q): Q<ListQuery>) -> R<Json<Vec<Row>>> {
    if let Some(s) = &q.status {
        one_of(s, "status", &["pending", "approved", "denied", "expired"])?;
    }
    let rows = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select {WITH_BOT} from approvals a join bots b on b.id = a.bot_id
         where a.owner_id = $1 and ($2::text is null or a.status = $2) and ($3::uuid is null or a.bot_id = $3)
         order by a.created_at desc limit 200"
    )))
    .bind(a.user)
    .bind(q.status)
    .bind(q.bot_id)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct Decision {
    decision: String,
    response: Option<String>,
}

pub async fn decide(
    State(st): State<S>,
    a: Auth,
    Id(id): Id,
    Body(d): Body<Decision>,
) -> R<Json<Row>> {
    let status = match d.decision.as_str() {
        "approve" => "approved",
        "deny" => "denied",
        _ => return Err(ApiError::bad("decision must be approve or deny")),
    };
    if d.response
        .as_deref()
        .is_some_and(|r| r.chars().count() > 20_000)
    {
        return Err(ApiError::bad("response too long (max 20000)"));
    }
    let row = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "with a as (
           update approvals set status = $3, decided_by = 'user', response = $4, decided_at = now()
           where id = $1 and owner_id = $2 and status = 'pending' returning *)
         select {WITH_BOT} from a join bots b on b.id = a.bot_id"
    )))
    .bind(id)
    .bind(a.user)
    .bind(status)
    .bind(d.response)
    .fetch_optional(&st.pool)
    .await?;
    match row {
        Some(r) => Ok(Json(r)),
        None => {
            super::owned(&st.pool, "approvals", id, a.user).await?;
            Err(ApiError::conflict("approval is no longer pending"))
        }
    }
}
