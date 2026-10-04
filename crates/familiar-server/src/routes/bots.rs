use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::{Body, Id, Row, found, one_of, text};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

pub const MODELS: [&str; 4] = ["sonnet", "opus", "haiku", "fable"];

/// Bot row joined with its derived status.
pub const BOT: &str = "select (to_jsonb(b) - 'owner_id') || jsonb_build_object('status', s.status, 'last_run_at', s.last_run_at)
    from bots b join bot_status s on s.id = b.id";

async fn bot_json(pool: &PgPool, owner: Uuid, id: Uuid) -> R<Json<Row>> {
    let row = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "{BOT} where b.id = $1 and b.owner_id = $2"
    )))
    .bind(id)
    .bind(owner)
    .fetch_optional(pool)
    .await?;
    found(row)
}

fn valid_slug(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=40).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let s: String = out.trim_matches('-').chars().take(40).collect();
    let s = s.trim_end_matches('-');
    if s.is_empty() { "bot".into() } else { s.into() }
}

pub async fn list(State(st): State<S>, a: Auth) -> R<Json<Vec<Row>>> {
    let rows = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "{BOT} where b.owner_id = $1 order by b.created_at"
    )))
    .bind(a.user)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

pub async fn get(State(st): State<S>, a: Auth, Id(id): Id) -> R<Json<Row>> {
    bot_json(&st.pool, a.user, id).await
}

const ENGINES: [&str; 2] = ["claude", "codex"];
const AVATAR_KEYS: [&str; 5] = ["shape", "color", "eyes", "mouth", "accessory"];

/// `"avatar": null` clears (Some(None)); absent leaves it alone (None).
fn nullable<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<serde_json::Value>>, D::Error> {
    Ok(Some(serde::Deserialize::deserialize(d)?))
}

/// claude: an alias (`sonnet`, ...) or an exact version id (`claude-opus-5-5`); codex: any sane model id.
fn check_model(engine: &str, model: &str) -> R<String> {
    if engine == "codex" {
        let ok = !model.is_empty()
            && model.len() <= 64
            && model
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'));
        if ok {
            Ok(model.to_string())
        } else {
            Err(ApiError::bad(
                "model must match ^[A-Za-z0-9._-]{1,64}$ for the codex engine",
            ))
        }
    } else if MODELS.contains(&model) || exact_claude(model) {
        Ok(model.to_string())
    } else {
        Err(ApiError::bad(format!(
            "model must be one of {} or match ^claude-[a-z0-9.-]{{3,60}}$ for the claude engine",
            MODELS.join(", ")
        )))
    }
}

/// `^claude-[a-z0-9.-]{3,60}$`
fn exact_claude(model: &str) -> bool {
    model.strip_prefix("claude-").is_some_and(|rest| {
        (3..=60).contains(&rest.len()) && rest.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'.' || c == b'-')
    })
}

fn check_avatar(v: &serde_json::Value) -> R<()> {
    let obj = v
        .as_object()
        .ok_or_else(|| ApiError::bad("avatar must be an object or null"))?;
    for (k, val) in obj {
        if !AVATAR_KEYS.contains(&k.as_str()) {
            return Err(ApiError::bad(format!("avatar.{k} is not a known field")));
        }
        // The avatar builders store the shape / eyes / mouth as small indexes and the rest as strings.
        let ok = match val {
            serde_json::Value::String(s) => s.chars().count() <= 32,
            serde_json::Value::Number(n) => n.as_u64().is_some_and(|n| n < 100),
            _ => false,
        };
        if !ok {
            return Err(ApiError::bad(format!(
                "avatar.{k} must be a string of at most 32 characters or a small whole number"
            )));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct NewBot {
    name: String,
    slug: Option<String>,
    persona: Option<String>,
    model: Option<String>,
    engine: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    avatar: Option<Option<serde_json::Value>>,
}

pub async fn create(
    State(st): State<S>,
    a: Auth,
    Body(b): Body<NewBot>,
) -> R<(StatusCode, Json<Row>)> {
    let name = text(&b.name, "name", 100)?;
    let slug = match b.slug.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if valid_slug(s) => s.to_string(),
        Some(_) => return Err(ApiError::bad("slug must match ^[a-z0-9][a-z0-9-]{0,39}$")),
        None => slugify(&name),
    };
    let engine = one_of(b.engine.as_deref().unwrap_or("claude"), "engine", &ENGINES)?;
    let model = check_model(&engine, b.model.as_deref().unwrap_or("sonnet"))?;
    let avatar = b.avatar.flatten();
    if let Some(v) = &avatar {
        check_avatar(v)?;
    }
    let persona = b.persona.unwrap_or_default();
    if persona.chars().count() > 20_000 {
        return Err(ApiError::bad("persona too long (max 20000)"));
    }
    let id: Uuid = sqlx::query_scalar(
        "insert into bots (owner_id, slug, name, persona, model, engine, avatar)
         values ($1, $2, $3, $4, $5, $6, $7) returning id",
    )
    .bind(a.user)
    .bind(&slug)
    .bind(&name)
    .bind(&persona)
    .bind(&model)
    .bind(&engine)
    .bind(avatar.map(sqlx::types::Json))
    .fetch_one(&st.pool)
    .await
    .map_err(|e| match ApiError::from(e) {
        ApiError::Conflict(_) => {
            ApiError::conflict(format!("a bot with slug '{slug}' already exists"))
        }
        other => other,
    })?;
    Ok((StatusCode::CREATED, bot_json(&st.pool, a.user, id).await?))
}

#[derive(Deserialize)]
pub struct BotPatch {
    name: Option<String>,
    persona: Option<String>,
    model: Option<String>,
    paused: Option<bool>,
    engine: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    avatar: Option<Option<serde_json::Value>>,
}

pub async fn update(
    State(st): State<S>,
    a: Auth,
    Id(id): Id,
    Body(p): Body<BotPatch>,
) -> R<Json<Row>> {
    let name = p
        .name
        .as_deref()
        .map(|n| text(n, "name", 100))
        .transpose()?;
    if p.persona
        .as_deref()
        .is_some_and(|s| s.chars().count() > 20_000)
    {
        return Err(ApiError::bad("persona too long (max 20000)"));
    }
    let engine = p
        .engine
        .as_deref()
        .map(|e| one_of(e, "engine", &ENGINES))
        .transpose()?;
    let set_avatar = p.avatar.is_some();
    let avatar = p.avatar.flatten();
    if let Some(v) = &avatar {
        check_avatar(v)?;
    }
    // the model must be valid for the resulting engine, so look at the current row
    let (cur_engine, cur_model): (String, String) =
        sqlx::query_as("select engine, model from bots where id = $1 and owner_id = $2")
            .bind(id)
            .bind(a.user)
            .fetch_optional(&st.pool)
            .await?
            .ok_or(ApiError::NotFound)?;
    let final_engine = engine.clone().unwrap_or(cur_engine);
    let model = match p.model.as_deref() {
        Some(m) => Some(check_model(&final_engine, m)?),
        None if engine.is_some() => {
            check_model(&final_engine, &cur_model)
                .map_err(|_| ApiError::bad("also set a model valid for the new engine"))?;
            None
        }
        None => None,
    };
    let n = sqlx::query(
        "update bots set name = coalesce($3, name), persona = coalesce($4, persona),
                model = coalesce($5, model), paused = coalesce($6, paused),
                engine = coalesce($7, engine),
                avatar = case when $8 then $9 else avatar end
         where id = $1 and owner_id = $2",
    )
    .bind(id)
    .bind(a.user)
    .bind(name)
    .bind(p.persona)
    .bind(model)
    .bind(p.paused)
    .bind(engine)
    .bind(set_avatar)
    .bind(avatar.map(sqlx::types::Json))
    .execute(&st.pool)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    bot_json(&st.pool, a.user, id).await
}

pub async fn remove(State(st): State<S>, a: Auth, Id(id): Id) -> R<StatusCode> {
    let n = sqlx::query("delete from bots where id = $1 and owner_id = $2")
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
