//! The owner's Gmail (and Google Calendar), connected once in Integrations → Accounts and used by teammates through
//! Familiar's own tools ([`tools`]). Two ways to connect, both on this PC only, with no third-party token broker:
//!
//! - **Connect Google** (OAuth, [`oauth`]): the installed-app flow with PKCE (S256), a `state`, and a loopback redirect
//!   on `127.0.0.1:<random port>` that answers one redirect and closes, opened in the owner's own browser. Least scopes
//!   ([`SCOPES`]). The refresh token is sealed with `SecretBox` in `accounts.secret_enc`; access tokens live in memory
//!   only ([`Tokens`]), refreshed with backoff; Disconnect revokes at Google and deletes.
//! - **Gmail with an App Password** ([`imap`], [`smtp`]): no Google Cloud project. IMAP and SMTP over implicit TLS
//!   only (993 and 465: no plaintext, no STARTTLS to downgrade), the 16-character app password sealed the same way.
//!   Calendar needs OAuth.
//!
//! Secrets are never logged and never returned by the API. Which teammates may use an account is the owner's choice
//! (`bot_accounts`); the tools refuse everyone else.

pub mod api;
pub mod imap;
pub mod mime;
pub mod oauth;
pub mod smtp;
mod tls;
pub mod tools;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::Config;
use crate::db::Db;

/// What Familiar asks Google for, least first: read mail, drafts and sending (`gmail.compose` covers both), and
/// calendar events.
pub const SCOPES: [&str; 3] = [
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/gmail.compose",
    "https://www.googleapis.com/auth/calendar.events",
];
pub const SCOPE_READ: &str = SCOPES[0];
pub const SCOPE_COMPOSE: &str = SCOPES[1];
pub const SCOPE_CALENDAR: &str = SCOPES[2];
/// Sending only (not asked for any more: compose covers it); an account that has it may still send.
pub const SCOPE_SEND: &str = "https://www.googleapis.com/auth/gmail.send";

/// The one-time Google Cloud setup, for builds without Familiar's OAuth client.
pub const SETUP_DOC: &str = "https://github.com/dhruvkhator/familiar/blob/main/docs/google-oauth-setup.md";
/// Where an app password is made (needs 2-Step Verification).
pub const APP_PASSWORD_URL: &str = "https://myaccount.google.com/apppasswords";

/// `accounts.kind`: connected with OAuth.
pub const KIND_OAUTH: &str = "google";
/// `accounts.kind`: Gmail over IMAP/SMTP with an app password.
pub const KIND_APP_PASSWORD: &str = "gmail_app_password";

/// Familiar's OAuth client (a Google "Desktop app" client) and Google's endpoints.
#[derive(Clone)]
pub struct OAuthClient {
    pub client_id: String,
    /// Not confidential for a desktop client (Google's own words), still never logged.
    pub client_secret: String,
    pub auth_url: String,
    pub token_url: String,
    pub revoke_url: String,
    pub gmail_url: String,
    pub calendar_url: String,
}

impl std::fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthClient").field("client_id", &self.client_id).finish_non_exhaustive()
    }
}

/// The Gmail servers for app-password accounts.
#[derive(Clone, Debug)]
pub struct MailServers {
    pub imap: (String, u16),
    pub smtp: (String, u16),
    /// One more certificate authority to trust (DER), for tests.
    pub extra_root: Option<Vec<u8>>,
}

impl Default for MailServers {
    fn default() -> Self {
        MailServers { imap: ("imap.gmail.com".into(), 993), smtp: ("smtp.gmail.com".into(), 465), extra_root: None }
    }
}

/// Everything Familiar needs to reach Google: the OAuth client (None when this build has none and config.toml sets
/// none) and the mail servers.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    pub oauth: Option<OAuthClient>,
    pub mail: MailServers,
}

/// Endpoints must be https; plain http only to this computer (tests).
fn endpoint(v: Option<&str>, default: &str) -> String {
    let v = v.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(default);
    let ok = match url::Url::parse(v) {
        Ok(u) if u.scheme() == "https" => true,
        Ok(u) if u.scheme() == "http" => matches!(u.host_str(), Some("127.0.0.1" | "localhost")),
        _ => false,
    };
    if ok { v.trim_end_matches('/').to_owned() } else { default.to_owned() }
}

fn host_port(v: Option<&str>, default: (&str, u16)) -> (String, u16) {
    v.and_then(|s| s.trim().rsplit_once(':'))
        .and_then(|(h, p)| Some((h.trim_matches(['[', ']']).to_owned(), p.parse().ok()?)))
        .filter(|(h, _)| !h.is_empty())
        .unwrap_or((default.0.to_owned(), default.1))
}

impl Settings {
    /// From `[google]` in config.toml, else the client this build carries (`FAMILIAR_GOOGLE_CLIENT_ID` / `_SECRET`
    /// at build time; empty = none).
    pub fn from_config(cfg: &Config) -> Settings {
        let g = cfg.google.clone().unwrap_or_default();
        let pick = |own: Option<String>, built: Option<&'static str>| {
            own.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).or_else(|| built.map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned))
        };
        let id = pick(g.client_id.clone(), option_env!("FAMILIAR_GOOGLE_CLIENT_ID"));
        let secret = pick(g.client_secret.clone(), option_env!("FAMILIAR_GOOGLE_CLIENT_SECRET"));
        let oauth = id.map(|client_id| OAuthClient {
            client_id,
            client_secret: secret.unwrap_or_default(),
            auth_url: endpoint(g.auth_url.as_deref(), "https://accounts.google.com/o/oauth2/v2/auth"),
            token_url: endpoint(g.token_url.as_deref(), "https://oauth2.googleapis.com/token"),
            revoke_url: endpoint(g.revoke_url.as_deref(), "https://oauth2.googleapis.com/revoke"),
            gmail_url: endpoint(g.gmail_url.as_deref(), "https://gmail.googleapis.com"),
            calendar_url: endpoint(g.calendar_url.as_deref(), "https://www.googleapis.com/calendar/v3"),
        });
        let extra_root = g.tls_root_pem.as_deref().and_then(|p| std::fs::read(p).ok()).and_then(|pem| tls::pem_to_der(&pem));
        let mail = MailServers {
            imap: host_port(g.imap.as_deref(), ("imap.gmail.com", 993)),
            smtp: host_port(g.smtp.as_deref(), ("smtp.gmail.com", 465)),
            extra_root,
        };
        Settings { oauth, mail }
    }
}

/// The HTTP client for Google: no redirects (a token never follows one), bounded time.
pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .user_agent(concat!("Familiar/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}

/// Random URL-safe text from 32 bytes (OAuth `state`, PKCE verifier).
pub fn random_token() -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(familiar_crypto::SecretBox::random_32())
}

// ---- accounts in the database ------------------------------------------------------------------------------------

/// A connected account, with its sealed secret (the refresh token or the app password).
#[derive(Clone, sqlx::FromRow)]
pub struct Account {
    pub id: Uuid,
    pub kind: String,
    pub email: String,
    pub scopes: Vec<String>,
    pub secret_enc: String,
    pub status: String,
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account").field("id", &self.id).field("kind", &self.kind).field("status", &self.status).finish_non_exhaustive()
    }
}

impl Account {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.kind == KIND_APP_PASSWORD || self.scopes.iter().any(|s| s == scope || (scope == SCOPE_SEND && s == SCOPE_COMPOSE))
    }
}

/// The accounts this teammate may use, OAuth first.
pub async fn linked_accounts(db: &Db, bot: Uuid) -> Result<Vec<Account>> {
    Ok(sqlx::query_as(
        "select a.id, a.kind, a.email, a.scopes, a.secret_enc, a.status from accounts a
         join bot_accounts ba on ba.account_id = a.id
         where ba.bot_id = $1 and a.owner_id = $2
         order by case a.kind when 'google' then 0 else 1 end",
    )
    .bind(bot)
    .bind(db.owner)
    .fetch_all(&db.pool)
    .await?)
}

/// What [`save_account`] did.
pub struct Saved {
    pub id: Uuid,
    /// The sealed secret of a different account it replaced (to revoke), if any.
    pub replaced: Option<(String, String)>,
}

/// Store a connected account (one per kind): the same address again keeps who may use it; another address replaces
/// the old one and its teammate links (a teammate the owner let read sam@ must not silently get kim@'s mail).
pub async fn save_account(pool: &PgPool, owner: Uuid, kind: &str, email: &str, scopes: &[String], sealed: &str) -> Result<Saved> {
    let mut tx = pool.begin().await?;
    let old: Option<(Uuid, String, String)> =
        sqlx::query_as("select id, email, secret_enc from accounts where owner_id = $1 and kind = $2 for update")
            .bind(owner)
            .bind(kind)
            .fetch_optional(&mut *tx)
            .await?;
    let saved = match old {
        Some((id, old_email, _)) if old_email.eq_ignore_ascii_case(email) => {
            sqlx::query(
                "update accounts set email = $2, scopes = $3, secret_enc = $4, status = 'ok', updated_at = now() where id = $1",
            )
            .bind(id)
            .bind(email)
            .bind(scopes)
            .bind(sealed)
            .execute(&mut *tx)
            .await?;
            Saved { id, replaced: None }
        }
        other => {
            let replaced = other.map(|(id, _, enc)| (id, enc));
            if let Some((id, _)) = &replaced {
                sqlx::query("delete from accounts where id = $1").bind(id).execute(&mut *tx).await?;
            }
            let id: Uuid = sqlx::query_scalar(
                "insert into accounts (owner_id, kind, email, scopes, secret_enc) values ($1, $2, $3, $4, $5) returning id",
            )
            .bind(owner)
            .bind(kind)
            .bind(email)
            .bind(scopes)
            .bind(sealed)
            .fetch_one(&mut *tx)
            .await?;
            Saved { id, replaced: replaced.map(|(_, enc)| (kind.to_owned(), enc)) }
        }
    };
    tx.commit().await?;
    Ok(saved)
}

async fn mark_reconnect(db: &Db, id: Uuid) {
    let _ = sqlx::query("update accounts set status = 'reconnect', updated_at = now() where id = $1 and owner_id = $2 and status <> 'reconnect'")
        .bind(id)
        .bind(db.owner)
        .execute(&db.pool)
        .await;
}

// ---- access tokens (memory only) --------------------------------------------------------------------------------

/// What a token refresh can end in.
#[derive(Debug)]
pub enum TokenError {
    /// Google refused the refresh token (revoked, expired, password changed): the owner reconnects.
    Reconnect,
    /// Try again later (network, Google's 5xx, a backoff still running), with a plain reason.
    Later(String),
}

impl TokenError {
    pub fn message(&self) -> String {
        match self {
            TokenError::Reconnect => "Your owner's Google connection stopped working (access was removed or it expired). \
                They need to reconnect it in Familiar: Integrations → Accounts → Connect Google."
                .into(),
            TokenError::Later(why) => format!("Google can't be reached right now ({why}); try again later."),
        }
    }
}

#[derive(Default)]
struct Slot {
    /// Which sealed refresh token this access token came from (a reconnect changes it).
    from: [u8; 32],
    access: Option<(zeroize::Zeroizing<String>, Instant)>,
    failures: u32,
    retry_at: Option<Instant>,
}

/// Access tokens per account, in memory, refreshed when they are about to expire. Never logged.
#[derive(Default)]
pub struct Tokens {
    slots: Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<Slot>>>>,
}

impl Tokens {
    fn slot(&self, id: Uuid) -> Arc<tokio::sync::Mutex<Slot>> {
        self.slots.lock().unwrap().entry(id).or_default().clone()
    }

    /// Forget an account's access token (Google said it no longer works).
    pub async fn forget(&self, id: Uuid) {
        let slot = self.slot(id);
        slot.lock().await.access = None;
    }

    /// A valid access token for this OAuth account: the cached one, else a refresh (one at a time per account, with
    /// exponential backoff after failures: 2 s, 4 s, … up to 5 min). A refused refresh token marks the account
    /// "reconnect".
    pub async fn access(
        &self,
        http: &reqwest::Client,
        client: &OAuthClient,
        secrets: &familiar_crypto::SecretBox,
        db: &Db,
        account: &Account,
    ) -> Result<String, TokenError> {
        if account.status == "reconnect" {
            return Err(TokenError::Reconnect);
        }
        let from: [u8; 32] = Sha256::digest(account.secret_enc.as_bytes()).into();
        let slot = self.slot(account.id);
        let mut s = slot.lock().await;
        if s.from != from {
            *s = Slot { from, ..Slot::default() };
        }
        if let Some((token, until)) = &s.access
            && Instant::now() + Duration::from_secs(60) < *until
        {
            return Ok(token.to_string());
        }
        if let Some(at) = s.retry_at
            && Instant::now() < at
        {
            return Err(TokenError::Later(format!("retrying in {} s", (at - Instant::now()).as_secs().max(1))));
        }
        let refresh = secrets.decrypt(&account.secret_enc).map(zeroize::Zeroizing::new).map_err(|_| TokenError::Later("its stored secret can't be opened with this key".into()))?;
        match oauth::refresh(http, client, &refresh).await {
            Ok((token, expires_in)) => {
                s.access = Some((zeroize::Zeroizing::new(token.clone()), Instant::now() + Duration::from_secs(expires_in.clamp(60, 3600))));
                s.failures = 0;
                s.retry_at = None;
                Ok(token)
            }
            Err(oauth::RefreshError::Revoked) => {
                s.access = None;
                mark_reconnect(db, account.id).await;
                Err(TokenError::Reconnect)
            }
            Err(oauth::RefreshError::Other(why)) => {
                s.failures = (s.failures + 1).min(16);
                s.retry_at = Some(Instant::now() + backoff(s.failures));
                Err(TokenError::Later(why))
            }
        }
    }
}

/// 2 s, 4 s, 8 s … at most 5 minutes.
pub fn backoff(failures: u32) -> Duration {
    Duration::from_secs((1u64 << failures.min(9)).min(300))
}

/// The daemon's Google state: settings, access tokens and per-run limits.
#[derive(Default)]
pub struct State {
    pub settings: Settings,
    pub http: reqwest::Client,
    pub tokens: Tokens,
    calls: Mutex<HashMap<Uuid, (u32, u32)>>,
}

/// Per run: Google reads, and actions (drafts, sends, events).
pub const MAX_READS_PER_RUN: u32 = 300;
pub const MAX_ACTIONS_PER_RUN: u32 = 30;

impl State {
    pub fn new(settings: Settings) -> Self {
        State { settings, http: http(), tokens: Tokens::default(), calls: Mutex::default() }
    }

    /// The run ended: forget its counts.
    pub fn finished(&self, run: Uuid) {
        self.calls.lock().unwrap().remove(&run);
    }

    /// Count one call of a run; false once it is over the limit.
    pub fn count(&self, run: Uuid, action: bool) -> bool {
        let mut calls = self.calls.lock().unwrap();
        if calls.len() > 1000 {
            calls.clear();
        }
        let c = calls.entry(run).or_default();
        if action {
            c.1 += 1;
            c.1 <= MAX_ACTIONS_PER_RUN
        } else {
            c.0 += 1;
            c.0 <= MAX_READS_PER_RUN
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_must_be_https_or_this_computer() {
        let d = "https://oauth2.googleapis.com/token";
        assert_eq!(endpoint(None, d), d);
        assert_eq!(endpoint(Some("https://example.test/token/"), d), "https://example.test/token");
        assert_eq!(endpoint(Some("http://127.0.0.1:5000/token"), d), "http://127.0.0.1:5000/token");
        for bad in ["http://evil.example/token", "ftp://x", "nonsense", "http://192.168.1.2/t", ""] {
            assert_eq!(endpoint(Some(bad), d), d, "{bad}");
        }
        assert_eq!(host_port(Some("127.0.0.1:1993"), ("imap.gmail.com", 993)), ("127.0.0.1".into(), 1993));
        assert_eq!(host_port(Some("nope"), ("imap.gmail.com", 993)), ("imap.gmail.com".into(), 993));
    }

    #[test]
    fn backoff_grows_and_stops() {
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(3), Duration::from_secs(8));
        assert_eq!(backoff(30), Duration::from_secs(300));
    }

    #[test]
    fn secrets_never_show_in_debug() {
        let c = OAuthClient {
            client_id: "id".into(),
            client_secret: "SECRET".into(),
            auth_url: String::new(),
            token_url: String::new(),
            revoke_url: String::new(),
            gmail_url: String::new(),
            calendar_url: String::new(),
        };
        assert!(!format!("{c:?}").contains("SECRET"));
        let a = Account { id: Uuid::nil(), kind: "google".into(), email: "a@b.io".into(), scopes: vec![], secret_enc: "v1:SEALED".into(), status: "ok".into() };
        assert!(!format!("{a:?}").contains("SEALED"));
    }

    #[test]
    fn random_tokens_are_long_and_fresh() {
        let (a, b) = (random_token(), random_token());
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn limits_per_run() {
        let s = State::new(Settings::default());
        let run = Uuid::new_v4();
        for _ in 0..MAX_ACTIONS_PER_RUN {
            assert!(s.count(run, true));
        }
        assert!(!s.count(run, true));
        assert!(s.count(run, false));
        assert!(s.count(Uuid::new_v4(), true));
    }
}
