//! Dream runs and the live browser view (frames written by the daemon, input sent back over `familiar_input`).
//!
//! In the desktop app the daemon runs in this process ([`familiar_core::browser::local`]), and the view also streams:
//! `GET /api/bots/{id}/live/stream?w=&h=` sends each new frame as soon as the page changes (screencast), and the
//! sign-in window opens on this computer (`/live/window`). Both answer only on this computer (loopback) and only for
//! the owner; a hosted API answers 503 (its daemon runs elsewhere), and clients fall back to `/live.jpg`.
//!
//! Stream format (`application/x-familiar-frames`), repeated: a 4-byte big-endian length, that many bytes of JSON
//! header `{seq, vw, vh, url, title}` (`vw`×`vh` = the page's viewport in CSS pixels, which input coordinates use),
//! a 4-byte big-endian length, that many bytes of JPEG. A ping (`{"ping":true}`, no picture) every 5 s of quiet.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use axum::{
    Json,
    body::Body as RespBody,
    extract::{ConnectInfo, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use familiar_core::browser::{Frame, WindowError};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Body, Id, Q, Row, found, one_of, owned};
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
        &["click", "move", "type", "key", "scroll", "navigate"],
    )?;
    let mut msg = json!({ "owner": a.user, "bot": bot, "type": i.kind });
    let need = |v: Option<&str>, f: &str| {
        v.map(str::to_string)
            .ok_or_else(|| ApiError::bad(format!("{f} is required for {}", i.kind)))
    };
    match i.kind.as_str() {
        "click" | "move" => {
            let (Some(x), Some(y)) = (i.x, i.y) else {
                return Err(ApiError::bad(format!("x and y are required for {}", i.kind)));
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

/// A quiet stream sends this often a ping (so a viewer that went away is noticed and the screencast stops).
const PING_EVERY: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
pub struct Size {
    w: Option<u32>,
    h: Option<u32>,
}

/// The daemon in this process, for a request from this computer by the owner of `bot`.
async fn local_for(st: &S, a: &Auth, bot: Uuid, peer: SocketAddr) -> R<familiar_core::browser::Local> {
    owned(&st.pool, "bots", bot, a.user).await?;
    // From another computer (a hosted API) these are simply not offered: the app falls back to polling and take over.
    let local = peer.ip().is_loopback().then(|| familiar_core::browser::local(a.user)).flatten();
    local.ok_or_else(|| ApiError::Unavailable("this works in the Familiar desktop app only".into()))
}

/// One frame (or a ping) in the stream's format.
pub fn encode(header: &Value, jpeg: &[u8]) -> Vec<u8> {
    let head = header.to_string();
    let mut out = Vec::with_capacity(8 + head.len() + jpeg.len());
    out.extend_from_slice(&(head.len() as u32).to_be_bytes());
    out.extend_from_slice(head.as_bytes());
    out.extend_from_slice(&(jpeg.len() as u32).to_be_bytes());
    out.extend_from_slice(jpeg);
    out
}

fn encode_frame(f: &Frame) -> Vec<u8> {
    encode(&json!({ "seq": f.seq, "vw": f.view_width, "vh": f.view_height, "url": f.url, "title": f.title }), &f.jpeg)
}

/// The live view as a stream of frames: each one as soon as the page changes, only the newest (a slow reader skips
/// frames instead of falling behind). The screencast runs while at least one stream is open.
pub async fn stream(
    State(st): State<S>,
    a: Auth,
    Id(bot): Id,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Q(size): Q<Size>,
) -> R<Response> {
    let local = local_for(&st, &a, bot, peer).await?;
    let viewer = local.watch(bot, size.w.unwrap_or(0), size.h.unwrap_or(0));
    let frames = futures_util::stream::unfold(viewer, |mut v| async move {
        tokio::select! {
            r = v.frames.changed() => {
                r.ok()?;
                // None: the daemon stopped; the client reconnects (or falls back) on its own.
                let f = v.frames.borrow_and_update().clone()?;
                Some((Ok::<_, Infallible>(encode_frame(&f)), v))
            }
            _ = tokio::time::sleep(PING_EVERY) => Some((Ok(encode(&json!({ "ping": true }), &[])), v)),
        }
    });
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-familiar-frames"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        RespBody::from_stream(frames),
    )
        .into_response())
}

/// Is the bot's sign-in window open? (`available`: this API can open one, i.e. it runs in the desktop app.)
pub async fn window_state(
    State(st): State<S>,
    a: Auth,
    Id(bot): Id,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> R<Json<Value>> {
    match local_for(&st, &a, bot, peer).await {
        Ok(local) => Ok(Json(json!({ "available": true, "open": local.window_open(bot) }))),
        Err(ApiError::Unavailable(_)) => Ok(Json(json!({ "available": false, "open": false }))),
        Err(e) => Err(e),
    }
}

#[derive(Deserialize)]
pub struct OpenWindow {
    url: Option<String>,
}

/// "Sign in in a window": the bot's browser profile in a normal Chrome window on this computer, at `url`.
pub async fn open_window(
    State(st): State<S>,
    a: Auth,
    Id(bot): Id,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Body(b): Body<OpenWindow>,
) -> R<(StatusCode, Json<Value>)> {
    let local = local_for(&st, &a, bot, peer).await?;
    let url = match b.url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        None => None,
        Some(u) => match url::Url::parse(u) {
            Ok(p) if matches!(p.scheme(), "http" | "https") && u.len() <= 2000 => Some(p.to_string()),
            _ => return Err(ApiError::bad("url must be an http(s) URL")),
        },
    };
    match local.open_window(bot, url.as_deref()).await {
        Ok(()) => Ok((StatusCode::ACCEPTED, Json(json!({ "open": true })))),
        Err(WindowError::Busy) => {
            let name: String = sqlx::query_scalar("select name from bots where id = $1 and owner_id = $2")
                .bind(bot)
                .bind(a.user)
                .fetch_one(&st.pool)
                .await?;
            Err(ApiError::conflict(format!(
                "{name} is using its browser right now. Try again when its run is done, or stop the run."
            )))
        }
        Err(WindowError::NoBrowser) => Err(ApiError::conflict(
            "Install Google Chrome or Microsoft Edge to sign in in a window.",
        )),
        Err(WindowError::Failed(e)) => Err(ApiError::conflict(format!("Couldn't open the window: {e}"))),
    }
}

/// "Done": close the bot's sign-in window (Chrome saves the session first). Needed on macOS, where closing the last
/// window leaves Chrome running.
pub async fn close_window(
    State(st): State<S>,
    a: Auth,
    Id(bot): Id,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> R<Json<Value>> {
    let local = local_for(&st, &a, bot, peer).await?;
    Ok(Json(json!({ "closed": local.close_window(bot).await })))
}
