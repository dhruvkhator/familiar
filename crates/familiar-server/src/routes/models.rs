//! `GET /api/models`: the models the owner's newest computer can run (`devices.info.models`, see familiar-core's
//! `models.rs`), for the model pickers. Empty lists until a computer has reported.

use axum::{Json, extract::State};
use serde_json::{Value, json};

use crate::{S, auth::Auth, error::R};

pub async fn get(State(st): State<S>, a: Auth) -> R<Json<Value>> {
    let models: Option<Value> = sqlx::query_scalar::<_, sqlx::types::Json<Value>>(
        "select info->'models' from devices
         where owner_id = $1 and jsonb_typeof(info->'models') = 'object'
         order by last_seen_at desc nulls last limit 1",
    )
    .bind(a.user)
    .fetch_optional(&st.pool)
    .await?
    .map(|j| j.0);
    let mut out = json!({ "claude": { "plan": null, "models": [] }, "codex": { "models": [] } });
    if let Some(m) = models {
        for key in ["claude", "codex"] {
            if m[key]["models"].is_array() {
                out[key] = m[key].clone();
            }
        }
    }
    Ok(Json(out))
}
