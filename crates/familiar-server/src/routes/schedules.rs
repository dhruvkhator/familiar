use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use std::str::FromStr;

use super::{Body, Id, Row, found, one_of, owned, text};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

const KINDS: [&str; 2] = ["scheduled", "proactive"];

/// Standard 5-field cron (minute hour day-of-month month day-of-week); returns it whitespace-normalised.
fn cron(s: &str) -> R<String> {
    let fields: Vec<&str> = s.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(ApiError::bad(
            "cron must have 5 fields: minute hour day-of-month month day-of-week",
        ));
    }
    let expr = fields.join(" ");
    croner::Cron::from_str(&expr).map_err(|e| ApiError::bad(format!("invalid cron: {e}")))?;
    Ok(expr)
}

pub async fn list(State(st): State<S>, a: Auth, Id(bot): Id) -> R<Json<Vec<Row>>> {
    owned(&st.pool, "bots", bot, a.user).await?;
    let rows = sqlx::query_scalar(
        "select to_jsonb(s) - 'owner_id' from schedules s where s.bot_id = $1 and s.owner_id = $2
         order by s.prompt, s.id",
    )
    .bind(bot)
    .bind(a.user)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct NewSchedule {
    cron: String,
    prompt: String,
    kind: Option<String>,
    enabled: Option<bool>,
    gate_command: Option<String>,
}

pub async fn create(
    State(st): State<S>,
    a: Auth,
    Id(bot): Id,
    Body(b): Body<NewSchedule>,
) -> R<(StatusCode, Json<Row>)> {
    let cron = cron(&b.cron)?;
    let prompt = text(&b.prompt, "prompt", 100_000)?;
    let kind = one_of(b.kind.as_deref().unwrap_or("scheduled"), "kind", &KINDS)?;
    let gate = gate(b.gate_command.as_deref())?;
    // thread_id is null: the DB trigger creates the schedule's thread
    let row: Option<Row> = sqlx::query_scalar(
        "insert into schedules (owner_id, bot_id, cron, prompt, kind, enabled, gate_command)
         select $2, x.id, $3, $4, $5, $6, $7 from bots x where x.id = $1 and x.owner_id = $2
         returning to_jsonb(schedules) - 'owner_id'",
    )
    .bind(bot)
    .bind(a.user)
    .bind(cron)
    .bind(prompt)
    .bind(kind)
    .bind(b.enabled.unwrap_or(true))
    .bind(gate)
    .fetch_optional(&st.pool)
    .await?;
    // the trigger-created thread belongs to the schedule surface
    if let Some(r) = row.as_ref().map(|r| &r.0)
        && let Some(id) = r["id"].as_str().and_then(|s| s.parse::<uuid::Uuid>().ok())
    {
        sqlx::query(
            "update threads set source = 'schedule' where schedule_id = $1 and owner_id = $2",
        )
        .bind(id)
        .bind(a.user)
        .execute(&st.pool)
        .await?;
    }
    Ok((StatusCode::CREATED, found(row)?))
}

#[derive(Deserialize)]
pub struct SchedulePatch {
    cron: Option<String>,
    prompt: Option<String>,
    kind: Option<String>,
    enabled: Option<bool>,
    gate_command: Option<String>,
}

pub async fn update(
    State(st): State<S>,
    a: Auth,
    Id(id): Id,
    Body(p): Body<SchedulePatch>,
) -> R<Json<Row>> {
    let cron = p.cron.as_deref().map(cron).transpose()?;
    let prompt = p
        .prompt
        .as_deref()
        .map(|s| text(s, "prompt", 100_000))
        .transpose()?;
    let kind = p
        .kind
        .as_deref()
        .map(|k| one_of(k, "kind", &KINDS))
        .transpose()?;
    let gate_set = p.gate_command.is_some();
    let gate = gate(p.gate_command.as_deref())?;
    let row = sqlx::query_scalar(
        "update schedules set cron = coalesce($3, cron), prompt = coalesce($4, prompt),
                kind = coalesce($5, kind), enabled = coalesce($6, enabled),
                gate_command = case when $7 then $8 else gate_command end
         where id = $1 and owner_id = $2 returning to_jsonb(schedules) - 'owner_id'",
    )
    .bind(id)
    .bind(a.user)
    .bind(cron)
    .bind(prompt)
    .bind(kind)
    .bind(p.enabled)
    .bind(gate_set)
    .bind(gate)
    .fetch_optional(&st.pool)
    .await?;
    found(row)
}

pub async fn remove(State(st): State<S>, a: Auth, Id(id): Id) -> R<StatusCode> {
    let n = sqlx::query("delete from schedules where id = $1 and owner_id = $2")
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

/// Optional shell gate; empty string clears it.
fn gate(g: Option<&str>) -> R<Option<String>> {
    let g = g.map(str::trim).filter(|g| !g.is_empty());
    if g.is_some_and(|g| g.chars().count() > 2000) {
        return Err(ApiError::bad("gate_command too long (max 2000)"));
    }
    Ok(g.map(str::to_string))
}
