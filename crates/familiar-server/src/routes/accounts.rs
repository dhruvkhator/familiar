//! Connected accounts (`familiar_core::google`): the owner's Gmail and Google Calendar, connected with "Connect Google"
//! (OAuth: PKCE, a loopback redirect on this computer, the owner's own browser) or with a Gmail app password
//! (IMAP/SMTP). Refresh tokens and app passwords are sealed with `SecretBox` and never returned; responses carry the
//! address, the scopes, the status and which teammates may use it.
//!
//! ```text
//! GET    /api/accounts                       {google: {configured, setup_url, scopes}, app_password: {url}, accounts: [..]}
//! POST   /api/accounts/google/connect        → {flow_id, auth_url}  (the app opens auth_url in the system browser)
//! GET    /api/accounts/google/connect/{id}   → {status: waiting|connected|failed|cancelled, error?, account_id?}
//! DELETE /api/accounts/google/connect/{id}   cancel
//! POST   /api/accounts/gmail                 {email, app_password} → the account (checked with Gmail first)
//! DELETE /api/accounts/{id}                  → {revoked}  (OAuth: revoked at Google, then deleted)
//! PUT    /api/accounts/{id}/bots             {bot_ids}  who may use it
//! GET|PUT /api/bots/{id}/accounts            {account_ids}  the same, per teammate
//! ```
//!
//! The sign-in runs where the API runs: in the desktop app that is the owner's computer, which the loopback redirect
//! needs. A server without an OAuth client (the standalone one) answers 503 to `connect`.

use std::time::{Duration, Instant};

use axum::{Json, extract::State, http::StatusCode};
use familiar_core::google::{self, oauth};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Body, Id, owned};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

/// One "Connect Google" in progress (or just finished), in memory.
pub struct Connect {
    owner: Uuid,
    status: &'static str,
    error: Option<String>,
    account_id: Option<Uuid>,
    cancel: tokio::sync::watch::Sender<bool>,
    at: Instant,
}

const VIEW: &str = "to_jsonb(a) - 'owner_id' - 'secret_enc'
    || jsonb_build_object('bot_ids', coalesce((select jsonb_agg(ba.bot_id order by ba.bot_id) from bot_accounts ba
                                                where ba.account_id = a.id), '[]'::jsonb))";

async fn views(st: &S, owner: Uuid, bot: Option<Uuid>) -> R<Vec<Value>> {
    let rows: Vec<(sqlx::types::Json<Value>,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {VIEW} from accounts a where a.owner_id = $1
           and ($2::uuid is null or exists(select 1 from bot_accounts ba where ba.account_id = a.id and ba.bot_id = $2))
         order by a.kind"
    )))
    .bind(owner)
    .bind(bot)
    .fetch_all(&st.pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0.0).collect())
}

async fn view(st: &S, owner: Uuid, id: Uuid) -> R<Value> {
    let row: Option<(sqlx::types::Json<Value>,)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("select {VIEW} from accounts a where a.id = $1 and a.owner_id = $2")))
            .bind(id)
            .bind(owner)
            .fetch_optional(&st.pool)
            .await?;
    row.map(|r| r.0.0).ok_or(ApiError::NotFound)
}

pub async fn list(State(st): State<S>, a: Auth) -> R<Json<Value>> {
    let accounts = views(&st, a.user, None).await?;
    Ok(Json(json!({
        "google": { "configured": st.google.oauth.is_some(), "setup_url": google::SETUP_DOC, "scopes": google::SCOPES },
        "app_password": { "url": google::APP_PASSWORD_URL },
        "accounts": accounts,
    })))
}

fn prune(map: &mut std::collections::HashMap<Uuid, Connect>) {
    map.retain(|_, c| c.status == "waiting" || c.at.elapsed() < Duration::from_secs(600));
}

/// Start "Connect Google": a loopback listener, PKCE and `state`; the sign-in URL for the app to open.
pub async fn connect(State(st): State<S>, a: Auth) -> R<(StatusCode, Json<Value>)> {
    let client = st.google.oauth.clone().ok_or_else(|| {
        ApiError::Unavailable(format!("This build of Familiar has no Google sign-in set up. One-time setup: {}", google::SETUP_DOC))
    })?;
    st.secret()?;
    let loopback = oauth::Loopback::bind().await.map_err(|_| ApiError::Internal)?;
    let pkce = oauth::pkce();
    let state = google::random_token();
    let redirect = loopback.redirect_uri.clone();
    let auth_url = oauth::auth_url(&client, &redirect, &state, &pkce.challenge);
    let id = Uuid::new_v4();
    let (cancel, mut cancelled) = tokio::sync::watch::channel(false);
    {
        let mut map = st.connects.lock().unwrap();
        prune(&mut map);
        // One sign-in at a time per owner: a new one ends the one before.
        for c in map.values_mut().filter(|c| c.owner == a.user && c.status == "waiting") {
            let _ = c.cancel.send(true);
            c.status = "cancelled";
            c.at = Instant::now();
        }
        map.insert(id, Connect { owner: a.user, status: "waiting", error: None, account_id: None, cancel, at: Instant::now() });
    }
    let (st2, owner) = (st.clone(), a.user);
    tokio::spawn(async move {
        let stop = async move {
            let _ = cancelled.wait_for(|c| *c).await;
        };
        let outcome = match loopback.wait(&state, oauth::SIGN_IN_TIMEOUT, stop).await {
            Err(oauth::FlowError::Cancelled) => return,
            Err(e) => Err(e.message()),
            Ok(code) => store_oauth(&st2, owner, &client, &code, &pkce.verifier, &redirect).await,
        };
        let mut map = st2.connects.lock().unwrap();
        if let Some(c) = map.get_mut(&id).filter(|c| c.status == "waiting") {
            c.at = Instant::now();
            match outcome {
                Ok(account) => {
                    c.status = "connected";
                    c.account_id = Some(account);
                }
                Err(e) => {
                    c.status = "failed";
                    c.error = Some(e);
                }
            }
        }
    });
    Ok((StatusCode::CREATED, Json(json!({ "flow_id": id, "auth_url": auth_url }))))
}

/// Trade the code, seal the refresh token, store the account; a different Google account replacing the old one gets
/// the old one's access revoked.
async fn store_oauth(st: &S, owner: Uuid, client: &google::OAuthClient, code: &str, verifier: &str, redirect: &str) -> Result<Uuid, String> {
    let http = google::http();
    let (email, grant) = oauth::finish(&http, client, code, verifier, redirect).await?;
    let secret = st.secret().map_err(|_| "Familiar's secret key isn't set".to_owned())?;
    let sealed = secret.encrypt(&grant.refresh_token).map_err(|_| "could not seal the token".to_owned())?;
    let saved = google::save_account(&st.pool, owner, google::KIND_OAUTH, &email, &grant.scopes, &sealed)
        .await
        .map_err(|_| "could not save the account".to_owned())?;
    if let Some((_, old)) = saved.replaced
        && let Ok(token) = secret.decrypt(&old)
    {
        oauth::revoke(&http, client, &token).await;
    }
    Ok(saved.id)
}

fn flow(st: &S, owner: Uuid, id: Uuid) -> R<Value> {
    let map = st.connects.lock().unwrap();
    let c = map.get(&id).filter(|c| c.owner == owner).ok_or(ApiError::NotFound)?;
    Ok(json!({ "status": c.status, "error": c.error, "account_id": c.account_id }))
}

pub async fn connect_status(State(st): State<S>, a: Auth, Id(id): Id) -> R<Json<Value>> {
    flow(&st, a.user, id).map(Json)
}

pub async fn connect_cancel(State(st): State<S>, a: Auth, Id(id): Id) -> R<StatusCode> {
    let mut map = st.connects.lock().unwrap();
    let c = map.get_mut(&id).filter(|c| c.owner == a.user).ok_or(ApiError::NotFound)?;
    if c.status == "waiting" {
        let _ = c.cancel.send(true);
        c.status = "cancelled";
        c.at = Instant::now();
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct AppPassword {
    email: String,
    app_password: String,
}

impl std::fmt::Debug for AppPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppPassword").field("email", &self.email).finish_non_exhaustive()
    }
}

/// Google's app passwords are 16 letters, shown in groups of four.
fn app_password(s: &str) -> Option<String> {
    let p: String = s.chars().filter(|c| *c != ' ' && *c != '\u{a0}').collect();
    (p.len() == 16 && p.chars().all(|c| c.is_ascii_alphanumeric())).then_some(p)
}

/// Connect Gmail with an app password: checked with Gmail (an IMAP login over TLS) before it is stored.
pub async fn connect_app_password(State(st): State<S>, a: Auth, Body(b): Body<AppPassword>) -> R<(StatusCode, Json<Value>)> {
    let email = google::mime::plain_address(&b.email).ok_or_else(|| ApiError::bad("email must be a Gmail address"))?;
    let password = app_password(&b.app_password)
        .ok_or_else(|| ApiError::bad("an app password is the 16 letters Google showed you (spaces don't matter)"))?;
    let secret = st.secret()?;
    if let Err(e) = google::imap::verify(&st.google.mail, &email, &password).await {
        return Err(ApiError::bad(format!("Gmail didn't accept it: {}", e.message())));
    }
    let sealed = secret.encrypt(&password).map_err(|_| ApiError::Internal)?;
    let saved = google::save_account(&st.pool, a.user, google::KIND_APP_PASSWORD, &email, &[], &sealed)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok((StatusCode::CREATED, Json(view(&st, a.user, saved.id).await?)))
}

/// Disconnect: an OAuth account's access is revoked at Google first (best effort), then the account and its secret are
/// deleted either way.
pub async fn remove(State(st): State<S>, a: Auth, Id(id): Id) -> R<Json<Value>> {
    let row: Option<(String, String)> = sqlx::query_as("select kind, secret_enc from accounts where id = $1 and owner_id = $2")
        .bind(id)
        .bind(a.user)
        .fetch_optional(&st.pool)
        .await?;
    let (kind, sealed) = row.ok_or(ApiError::NotFound)?;
    let mut revoked = false;
    if kind == google::KIND_OAUTH
        && let (Some(client), Some(secret)) = (st.google.oauth.as_ref(), st.secret.as_ref())
        && let Ok(token) = secret.decrypt(&sealed)
    {
        revoked = oauth::revoke(&google::http(), client, &token).await;
    }
    sqlx::query("delete from accounts where id = $1 and owner_id = $2").bind(id).bind(a.user).execute(&st.pool).await?;
    Ok(Json(json!({ "kind": kind, "revoked": revoked })))
}

#[derive(Deserialize)]
pub struct Bots {
    bot_ids: Vec<Uuid>,
}

/// Who may use this account (the whole set).
pub async fn set_bots(State(st): State<S>, a: Auth, Id(id): Id, Body(b): Body<Bots>) -> R<Json<Value>> {
    owned(&st.pool, "accounts", id, a.user).await?;
    let mut ids = b.bot_ids;
    ids.sort();
    ids.dedup();
    let mut tx = st.pool.begin().await?;
    let found: i64 = sqlx::query_scalar("select count(*) from bots where owner_id = $1 and id = any($2)")
        .bind(a.user)
        .bind(&ids)
        .fetch_one(&mut *tx)
        .await?;
    if found != ids.len() as i64 {
        return Err(ApiError::NotFound);
    }
    sqlx::query("delete from bot_accounts where account_id = $1").bind(id).execute(&mut *tx).await?;
    sqlx::query("insert into bot_accounts (bot_id, account_id) select unnest($1::uuid[]), $2").bind(&ids).bind(id).execute(&mut *tx).await?;
    sqlx::query("update accounts set updated_at = now() where id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(view(&st, a.user, id).await?))
}

pub async fn for_bot(State(st): State<S>, a: Auth, Id(bot): Id) -> R<Json<Vec<Value>>> {
    owned(&st.pool, "bots", bot, a.user).await?;
    Ok(Json(views(&st, a.user, Some(bot)).await?))
}

#[derive(Deserialize)]
pub struct Accounts {
    account_ids: Vec<Uuid>,
}

/// Which accounts this teammate may use (the whole set).
pub async fn set_for_bot(State(st): State<S>, a: Auth, Id(bot): Id, Body(b): Body<Accounts>) -> R<Json<Vec<Value>>> {
    owned(&st.pool, "bots", bot, a.user).await?;
    let mut ids = b.account_ids;
    ids.sort();
    ids.dedup();
    let mut tx = st.pool.begin().await?;
    let found: i64 = sqlx::query_scalar("select count(*) from accounts where owner_id = $1 and id = any($2)")
        .bind(a.user)
        .bind(&ids)
        .fetch_one(&mut *tx)
        .await?;
    if found != ids.len() as i64 {
        return Err(ApiError::NotFound);
    }
    sqlx::query(
        "update accounts set updated_at = now() where owner_id = $2
           and (id = any($1) or id in (select account_id from bot_accounts where bot_id = $3))",
    )
    .bind(&ids)
    .bind(a.user)
    .bind(bot)
    .execute(&mut *tx)
    .await?;
    sqlx::query("delete from bot_accounts where bot_id = $1").bind(bot).execute(&mut *tx).await?;
    sqlx::query("insert into bot_accounts (bot_id, account_id) select $1, unnest($2::uuid[])").bind(bot).bind(&ids).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(views(&st, a.user, Some(bot)).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_passwords_are_sixteen_letters() {
        assert_eq!(app_password("abcd efgh ijkl mnop").as_deref(), Some("abcdefghijklmnop"));
        assert_eq!(app_password("abcdefghijklmnop").as_deref(), Some("abcdefghijklmnop"));
        for bad in ["short", "abcd efgh ijkl mnop q", "abcd-efgh-ijkl-mnop", "abcd\r\nefghijklmnop", ""] {
            assert_eq!(app_password(bad), None, "{bad:?}");
        }
        let b = AppPassword { email: "a@b.io".into(), app_password: "SECRETSECRETSECR".into() };
        assert!(!format!("{b:?}").contains("SECRET"));
    }
}
