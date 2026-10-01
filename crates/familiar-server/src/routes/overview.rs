use axum::{Json, extract::State};
use serde_json::{Value, json};

use super::{Row, bots::BOT};
use crate::{S, auth::Auth, error::R};

pub async fn get(State(st): State<S>, a: Auth) -> R<Json<Value>> {
    let bots = sqlx::query_scalar::<_, Row>(sqlx::AssertSqlSafe(format!(
        "{BOT} where b.owner_id = $1 order by b.created_at"
    )))
    .bind(a.user)
    .fetch_all(&st.pool);
    let pending = sqlx::query_scalar::<_, i64>(
        "select count(*) from approvals where owner_id = $1 and status = 'pending'",
    )
    .bind(a.user)
    .fetch_one(&st.pool);
    let devices = sqlx::query_scalar::<_, Row>(
        "select (to_jsonb(d) - 'owner_id') || jsonb_build_object('online', d.last_seen_at > now() - interval '3 minutes')
         from devices d where d.owner_id = $1 order by d.name, d.id",
    )
    .bind(a.user)
    .fetch_all(&st.pool);
    let (bots, pending, devices) = tokio::try_join!(bots, pending, devices)?;
    Ok(Json(
        json!({ "bots": bots, "pending_approvals": pending, "devices": devices }),
    ))
}
