use axum::{Json, extract::State};

use super::{Id, Row, owned};
use crate::{S, auth::Auth, error::R};

/// Read-only: the daemon mirrors `<workspace>/.claude/skills/*/SKILL.md` here.
pub async fn list(State(st): State<S>, a: Auth, Id(bot): Id) -> R<Json<Vec<Row>>> {
    owned(&st.pool, "bots", bot, a.user).await?;
    let rows = sqlx::query_scalar(
        "select to_jsonb(s) - 'owner_id' from skills s where s.bot_id = $1 and s.owner_id = $2 order by s.name",
    )
    .bind(bot)
    .bind(a.user)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}
