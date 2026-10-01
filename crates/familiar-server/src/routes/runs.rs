use axum::{Json, extract::State};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{Id, Q, Row, clamp, found, owned, parse_ts};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

#[derive(Deserialize)]
pub struct RunQuery {
    limit: Option<i64>,
    before: Option<String>,
}

/// Runs of a thread or bot, newest first. `before` is an RFC3339 `created_at` cursor.
async fn runs_where(st: &S, a: &Auth, col: &str, id: uuid::Uuid, q: RunQuery) -> R<Json<Vec<Row>>> {
    let before: Option<DateTime<Utc>> = q.before.as_deref().map(parse_ts).transpose()?;
    let rows = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select to_jsonb(r) - 'owner_id' from runs r where r.{col} = $1 and r.owner_id = $2
           and ($3::timestamptz is null or r.created_at < $3) order by r.created_at desc, r.id desc limit $4"
    )))
    .bind(id)
    .bind(a.user)
    .bind(before)
    .bind(clamp(q.limit, 50, 200))
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

pub async fn for_thread(
    State(st): State<S>,
    a: Auth,
    Id(id): Id,
    Q(q): Q<RunQuery>,
) -> R<Json<Vec<Row>>> {
    owned(&st.pool, "threads", id, a.user).await?;
    runs_where(&st, &a, "thread_id", id, q).await
}

pub async fn for_bot(
    State(st): State<S>,
    a: Auth,
    Id(id): Id,
    Q(q): Q<RunQuery>,
) -> R<Json<Vec<Row>>> {
    owned(&st.pool, "bots", id, a.user).await?;
    runs_where(&st, &a, "bot_id", id, q).await
}

pub async fn get(State(st): State<S>, a: Auth, Id(id): Id) -> R<Json<Row>> {
    let row = sqlx::query_scalar(
        "select to_jsonb(r) - 'owner_id' from runs r where id = $1 and owner_id = $2",
    )
    .bind(id)
    .bind(a.user)
    .fetch_optional(&st.pool)
    .await?;
    found(row)
}

#[derive(Deserialize)]
pub struct EventQuery {
    after_seq: Option<i32>,
    limit: Option<i64>,
}

pub async fn events(
    State(st): State<S>,
    a: Auth,
    Id(id): Id,
    Q(q): Q<EventQuery>,
) -> R<Json<Vec<Row>>> {
    owned(&st.pool, "runs", id, a.user).await?;
    let rows = sqlx::query_scalar(
        "select to_jsonb(e) - 'owner_id' from events e where e.run_id = $1 and e.owner_id = $2 and e.seq > $3
         order by e.seq limit $4",
    )
    .bind(id)
    .bind(a.user)
    .bind(q.after_seq.unwrap_or(-1))
    .bind(clamp(q.limit, 1000, 5000))
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

pub async fn cancel(State(st): State<S>, a: Auth, Id(id): Id) -> R<Json<Row>> {
    let row = sqlx::query_scalar(
        "update runs set status = 'cancelled', finished_at = now()
         where id = $1 and owner_id = $2 and status in ('queued', 'running', 'waiting_approval')
         returning to_jsonb(runs) - 'owner_id'",
    )
    .bind(id)
    .bind(a.user)
    .fetch_optional(&st.pool)
    .await?;
    match row {
        Some(r) => Ok(Json(r)),
        None => {
            owned(&st.pool, "runs", id, a.user).await?;
            Err(ApiError::conflict(
                "run is not cancellable (already finished)",
            ))
        }
    }
}
