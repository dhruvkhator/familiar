//! Dream runs and the live browser view (frames written by the daemon, input sent back over `familiar_input`).

use axum::{
    Json,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Body, Id, Row, found, one_of, owned};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

/// Queue a dream run now. The daemon swaps this placeholder for the real dream instructions.
pub async fn dream(State(st): State<S>, a: Auth, Id(bot): Id) -> R<(StatusCode, Json<Value>)> {
    let mut tx = st.pool.begin().await?;
    // lock the bot row so two concurrent requests can't both queue a dream
    let _: Uuid =
        sqlx::query_scalar("select id from bots where id = $1 and owner_id = $2 for update")
            .bind(bot)
            .bind(a.user)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApiError::NotFound)?;
    let busy: bool = sqlx::query_scalar(
        "select exists(select 1 from runs where bot_id = $1 and owner_id = $2 and kind = 'dream'
           and status in ('queued', 'running', 'waiting_approval'))",
    )
    .bind(bot)
    .bind(a.user)
    .fetch_one(&mut *tx)
    .await?;
    if busy {
        return Err(ApiError::conflict(
            "a dream run is already queued or running",
        ));
    }
    let thread: Option<Uuid> = sqlx::query_scalar(
        "select id from threads where bot_id = $1 and owner_id = $2 and title = 'Dreams' and source = 'schedule'
         order by created_at limit 1",
    )
    .bind(bot)
    .bind(a.user)
    .fetch_optional(&mut *tx)
    .await?;
    let thread = match thread {
        Some(t) => t,
        None => {
            sqlx::query_scalar(
                "insert into threads (owner_id, bot_id, title, source) values ($1, $2, 'Dreams', 'schedule') returning id",
            )
            .bind(a.user)
            .bind(bot)
            .fetch_one(&mut *tx)
            .await?
        }
    };
    let run: Uuid = sqlx::query_scalar(
        "insert into runs (owner_id, bot_id, thread_id, kind, prompt, status)
         values ($1, $2, $3, 'dream', '(dream)', 'queued') returning id",
    )
    .bind(a.user)
    .bind(bot)
    .bind(thread)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "run_id": run }))))
}

pub async fn info(State(st): State<S>, a: Auth, Id(bot): Id) -> R<Json<Row>> {
    let row = sqlx::query_scalar(
        "select jsonb_build_object('url', url, 'title', title, 'width', width, 'height', height,
                'updated_at', updated_at) from live_frames where bot_id = $1 and owner_id = $2",
    )
    .bind(bot)
    .bind(a.user)
    .fetch_optional(&st.pool)
    .await?;
    found(row)
}

pub async fn frame(State(st): State<S>, a: Auth, Id(bot): Id) -> R<Response> {
    let jpeg: Option<Vec<u8>> =
        sqlx::query_scalar("select jpeg from live_frames where bot_id = $1 and owner_id = $2")
            .bind(bot)
            .bind(a.user)
            .fetch_optional(&st.pool)
            .await?;
    let jpeg = jpeg.ok_or(ApiError::NotFound)?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        jpeg,
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct Input {
    #[serde(rename = "type")]
    kind: String,
    x: Option<i64>,
    y: Option<i64>,
    text: Option<String>,
    key: Option<String>,
    dy: Option<i64>,
    url: Option<String>,
}

pub async fn input(
    State(st): State<S>,
    a: Auth,
    Id(bot): Id,
    Body(i): Body<Input>,
) -> R<(StatusCode, Json<Value>)> {
    owned(&st.pool, "bots", bot, a.user).await?;
    one_of(
        &i.kind,
        "type",
        &["click", "type", "key", "scroll", "navigate"],
    )?;
    let mut msg = json!({ "owner": a.user, "bot": bot, "type": i.kind });
    let need = |v: Option<&str>, f: &str| {
        v.map(str::to_string)
            .ok_or_else(|| ApiError::bad(format!("{f} is required for {}", i.kind)))
    };
    match i.kind.as_str() {
        "click" => {
            let (Some(x), Some(y)) = (i.x, i.y) else {
                return Err(ApiError::bad("x and y are required for click"));
            };
            if x < 0 || y < 0 {
                return Err(ApiError::bad("x and y must be >= 0"));
            }
            msg["x"] = json!(x);
            msg["y"] = json!(y);
        }
        "type" => {
            let t = need(i.text.as_deref(), "text")?;
            if t.chars().count() > 2000 || t.len() > 6000 {
                return Err(ApiError::bad("text too long (max 2000 characters)"));
            }
            msg["text"] = json!(t);
        }
        "key" => {
            let k = need(i.key.as_deref(), "key")?;
            if k.is_empty() || k.chars().count() > 32 {
                return Err(ApiError::bad("key must be 1-32 characters"));
            }
            msg["key"] = json!(k);
        }
        "scroll" => {
            msg["dy"] = json!(i.dy.ok_or_else(|| ApiError::bad("dy is required for scroll"))?);
            if let (Some(x), Some(y)) = (i.x, i.y)
                && x >= 0
                && y >= 0
            {
                msg["x"] = json!(x);
                msg["y"] = json!(y);
            }
        }
        _ => {
            let u = need(i.url.as_deref(), "url")?;
            match url::Url::parse(u.trim()) {
                Ok(p) if matches!(p.scheme(), "http" | "https") && u.len() <= 2000 => {
                    msg["url"] = json!(p.as_str())
                }
                _ => return Err(ApiError::bad("url must be an http(s) URL")),
            }
        }
    }
    sqlx::query("select pg_notify('familiar_input', $1)")
        .bind(msg.to_string())
        .execute(&st.pool)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "ok": true }))))
}
