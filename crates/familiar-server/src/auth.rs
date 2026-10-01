//! Owner auth: argon2id password -> random bearer token (only its sha256 is stored).

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use axum::{
    Json,
    extract::{FromRequestParts, State},
    http::{StatusCode, header, request::Parts},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::{
    S,
    error::{ApiError, R},
    routes::Body,
};

const MIN_PASSWORD: usize = 10;
const SESSION_DAYS: i32 = 30;

pub struct Auth {
    pub user: Uuid,
    pub token_hash: String,
}

fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn bearer(parts: &Parts) -> Option<String> {
    let v = parts.headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_string())
}

/// EventSource / <img> / links can't set headers, so `/api/stream` and artifact downloads also accept `?token=`.
fn query_token(parts: &Parts) -> Option<String> {
    let p = parts.uri.path();
    let dl = (p.starts_with("/api/artifacts/") && p.ends_with("/download"))
        || (p.starts_with("/api/bots/") && p.ends_with("/live.jpg"));
    if p != "/api/stream" && !dl {
        return None;
    }
    parts
        .uri
        .query()?
        .split('&')
        .find_map(|kv| kv.strip_prefix("token="))
        .map(str::to_string)
}

impl FromRequestParts<S> for Auth {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, st: &S) -> R<Self> {
        let token = bearer(parts)
            .or_else(|| query_token(parts))
            .ok_or(ApiError::Unauthorized)?;
        let token_hash = hash_token(&token);
        let user: Option<Uuid> = sqlx::query_scalar(
            "select user_id from sessions where token_hash = $1 and expires_at > now()",
        )
        .bind(&token_hash)
        .fetch_optional(&st.pool)
        .await?;
        Ok(Auth {
            user: user.ok_or(ApiError::Unauthorized)?,
            token_hash,
        })
    }
}

async fn hash_password(pw: String) -> R<String> {
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(pw.as_bytes())
            .map(|h| h.to_string())
    })
    .await
    .map_err(|_| ApiError::Internal)?
    .map_err(|e| {
        tracing::error!("argon2 hash: {e}");
        ApiError::Internal
    })
}

async fn verify_password(pw: String, phc: String) -> bool {
    tokio::task::spawn_blocking(move || match PasswordHash::new(&phc) {
        Ok(h) => Argon2::default().verify_password(pw.as_bytes(), &h).is_ok(),
        Err(_) => false,
    })
    .await
    .unwrap_or(false)
}

/// Insert a new session (and sweep expired ones); returns the raw token.
async fn new_session(conn: &mut sqlx::PgConnection, user: Uuid) -> R<String> {
    let mut raw = [0u8; 32];
    rand::fill(&mut raw);
    let token = URL_SAFE_NO_PAD.encode(raw);
    sqlx::query(
        "insert into sessions (token_hash, user_id, expires_at) values ($1, $2, now() + make_interval(days => $3))",
    )
    .bind(hash_token(&token))
    .bind(user)
    .bind(SESSION_DAYS)
    .execute(&mut *conn)
    .await?;
    sqlx::query("delete from sessions where expires_at < now()")
        .execute(&mut *conn)
        .await?;
    Ok(token)
}

#[derive(Deserialize)]
pub struct Creds {
    email: String,
    password: String,
}

pub async fn state(State(st): State<S>) -> R<Json<Value>> {
    let any: bool = sqlx::query_scalar("select exists(select 1 from users)")
        .fetch_one(&st.pool)
        .await?;
    Ok(Json(json!({ "setup_needed": !any })))
}

pub async fn setup(State(st): State<S>, Body(c): Body<Creds>) -> R<Json<Value>> {
    let email = c.email.trim().to_lowercase();
    if email.len() < 3
        || email.len() > 254
        || !email.contains('@')
        || email.contains(char::is_whitespace)
    {
        return Err(ApiError::bad("invalid email"));
    }
    if c.password.chars().count() < MIN_PASSWORD {
        return Err(ApiError::bad(format!(
            "password must be at least {MIN_PASSWORD} characters"
        )));
    }
    if c.password.len() > 1024 {
        return Err(ApiError::bad("password too long"));
    }
    let hash = hash_password(c.password).await?;
    let mut tx = st.pool.begin().await?;
    // Serialise concurrent setups: the loser then sees the winner's row and gets 409.
    sqlx::query("select pg_advisory_xact_lock(7264001)")
        .execute(&mut *tx)
        .await?;
    let id: Option<Uuid> = sqlx::query_scalar(
        "insert into users (email, password_hash) select $1, $2 where not exists (select 1 from users) returning id",
    )
    .bind(&email)
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await?;
    let id = id.ok_or_else(|| ApiError::conflict("setup already completed"))?;
    let token = new_session(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Json(
        json!({ "token": token, "user": { "id": id, "email": email } }),
    ))
}

/// Per-process brute-force slowdown: the delay grows with recent failures for this email and globally.
fn login_delay(st: &S, key: &str) -> Duration {
    let mut m = st.fails.lock().unwrap();
    let now = Instant::now();
    m.retain(|_, (_, t)| now.duration_since(*t) < Duration::from_secs(900));
    let n = ["*", key]
        .iter()
        .filter_map(|k| m.get(*k))
        .map(|(n, _)| *n)
        .max()
        .unwrap_or(0);
    // three free attempts, then 0.5s, 1s, 2s ... capped at 8s
    if n < 3 {
        Duration::ZERO
    } else {
        Duration::from_millis((250u64 << (n - 2).min(5)).min(8000))
    }
}

fn note_login(st: &S, key: &str, ok: bool) {
    let mut m = st.fails.lock().unwrap();
    if m.len() > 10_000 {
        m.clear();
    }
    if ok {
        m.remove(key);
        return;
    }
    for k in ["*", key] {
        let e = m.entry(k.to_string()).or_insert((0, Instant::now()));
        *e = (e.0 + 1, Instant::now());
    }
}

pub async fn login(State(st): State<S>, Body(c): Body<Creds>) -> R<Json<Value>> {
    let email = c.email.trim().to_lowercase();
    let delay = login_delay(&st, &email);
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let row: Option<(Uuid, String)> =
        sqlx::query_as("select id, password_hash from users where email = $1")
            .bind(&email)
            .fetch_optional(&st.pool)
            .await?;
    let user = match row {
        Some((id, phc)) => verify_password(c.password, phc).await.then_some(id),
        None => {
            // burn a hash so timing doesn't reveal which emails exist
            let _ = hash_password(c.password).await;
            None
        }
    };
    note_login(&st, &email, user.is_some());
    let id = user.ok_or(ApiError::Unauthorized)?;
    let mut conn = st.pool.acquire().await?;
    let token = new_session(&mut conn, id).await?;
    Ok(Json(
        json!({ "token": token, "user": { "id": id, "email": email } }),
    ))
}

pub async fn logout(State(st): State<S>, a: Auth) -> R<StatusCode> {
    sqlx::query("delete from sessions where token_hash = $1")
        .bind(&a.token_hash)
        .execute(&st.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn me(State(st): State<S>, a: Auth) -> R<Json<Value>> {
    let email: String = sqlx::query_scalar("select email from users where id = $1")
        .bind(a.user)
        .fetch_one(&st.pool)
        .await?;
    Ok(Json(json!({ "id": a.user, "email": email })))
}

#[derive(Deserialize)]
pub struct AccountUpdate {
    current_password: String,
    email: Option<String>,
    new_password: Option<String>,
}

/// Change email and/or password. Needs the current password (same slowdown as login); a password
/// change signs out every other session of this user.
pub async fn update_account(
    State(st): State<S>,
    a: Auth,
    Body(c): Body<AccountUpdate>,
) -> R<Json<Value>> {
    let (cur_email, phc): (String, String) =
        sqlx::query_as("select email, password_hash from users where id = $1")
            .bind(a.user)
            .fetch_one(&st.pool)
            .await?;
    let new_email = match c.email.as_deref().map(|e| e.trim().to_lowercase()) {
        Some(e) if e != cur_email => {
            if e.len() < 3 || e.len() > 254 || !e.contains('@') || e.contains(char::is_whitespace)
            {
                return Err(ApiError::bad("invalid email"));
            }
            Some(e)
        }
        _ => None,
    };
    if let Some(p) = &c.new_password {
        if p.chars().count() < MIN_PASSWORD {
            return Err(ApiError::bad(format!(
                "password must be at least {MIN_PASSWORD} characters"
            )));
        }
        if p.len() > 1024 {
            return Err(ApiError::bad("password too long"));
        }
    }
    let key = format!("acct:{}", a.user);
    let delay = login_delay(&st, &key);
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let ok = verify_password(c.current_password, phc).await;
    note_login(&st, &key, ok);
    if !ok {
        return Err(ApiError::Unauthorized);
    }
    let new_hash = match c.new_password {
        Some(p) => Some(hash_password(p).await?),
        None => None,
    };
    let mut tx = st.pool.begin().await?;
    if let Some(e) = &new_email {
        let r = sqlx::query("update users set email = $1 where id = $2")
            .bind(e)
            .bind(a.user)
            .execute(&mut *tx)
            .await;
        if let Err(sqlx::Error::Database(d)) = &r
            && d.is_unique_violation()
        {
            return Err(ApiError::conflict("that email is already in use"));
        }
        r?;
    }
    if let Some(h) = &new_hash {
        sqlx::query("update users set password_hash = $1 where id = $2")
            .bind(h)
            .bind(a.user)
            .execute(&mut *tx)
            .await?;
        sqlx::query("delete from sessions where user_id = $1 and token_hash <> $2")
            .bind(a.user)
            .bind(&a.token_hash)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Json(
        json!({ "id": a.user, "email": new_email.unwrap_or(cur_email) }),
    ))
}
