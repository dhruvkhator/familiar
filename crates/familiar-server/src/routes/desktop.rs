//! "Stop desktop control" (the app's tray): every desktop request still waiting is denied here at once, and the daemon
//! is told (over `familiar_input`) to take the desktop away from the teammate using it and cancel that run.

use axum::{Json, extract::State, http::StatusCode};
use serde_json::{Value, json};

use crate::{S, auth::Auth, error::R};

pub async fn stop(State(st): State<S>, a: Auth) -> R<(StatusCode, Json<Value>)> {
    let denied = sqlx::query(
        "update approvals set status = 'denied', decided_by = 'user', decided_at = now(),
                response = 'You stopped desktop control.'
         where owner_id = $1 and status = 'pending' and tool_name like 'mcp\\_\\_desktop\\_\\_%'",
    )
    .bind(a.user)
    .execute(&st.pool)
    .await?
    .rows_affected();
    sqlx::query("select pg_notify('familiar_input', $1)")
        .bind(json!({ "type": "desktop_stop", "owner": a.user }).to_string())
        .execute(&st.pool)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "denied": denied }))))
}
