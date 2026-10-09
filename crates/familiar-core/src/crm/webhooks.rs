//! Outgoing CRM webhooks: other CRMs (HubSpot through Zapier, Attio, a home-made one) get every change as a signed POST.
//!
//! - [`enqueue`] writes one delivery per matching webhook in the transaction of the change itself.
//! - The daemon ([`run`]) delivers them: POST, [`TIMEOUT`], no redirects, at most [`MAX_BODY`], headers
//!   `Familiar-Event`, `Familiar-Delivery` (the delivery id; the same on every retry) and
//!   `Familiar-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256 of "<t>.<body>" keyed with the secret>` ([`signature`]).
//!   No answer, a 5xx, a redirect, 408 or 429 is retried after [`RETRIES`] (1 m, 5 m, 30 m, 2 h, 6 h), then the delivery
//!   is `failed`; another 4xx, a payload over the limit or a secret that can't be read fails at once ([`Failure`]).
//! - Where a webhook may point ([`refused`]), checked when it is saved and again after DNS at every delivery, with the
//!   connection pinned to the addresses that were checked (no DNS rebinding in between): `https` to public addresses;
//!   this computer (loopback) over `http` or `https` in the desktop app only ([`set_allow_loopback`]; a shared server
//!   refuses it unless told otherwise); never private LAN, link-local (cloud metadata), multicast,
//!   unspecified, documentation or other reserved addresses, whatever the scheme. Private LAN addresses are refused even
//!   with https: the payload is customer data, LAN devices are the usual target of a forged request, and a receiver on
//!   the LAN can be reached through a public https endpoint or a tunnel on this computer instead.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures_util::StreamExt;
use hmac::{KeyInit, Mac};
use serde_json::{Value, json};
use sqlx::{PgConnection, types::Json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use super::{CrmError, Result};
use crate::db::Db;

/// What a webhook can listen to.
pub const EVENTS: [&str; 9] = [
    "company.created", "company.updated", "contact.created", "contact.updated", "contact.do_not_contact",
    "deal.created", "deal.updated", "deal.stage_changed", "activity.created",
];

/// The largest body sent; a bigger payload fails at once.
pub const MAX_BODY: usize = 256 * 1024;
/// How long one delivery may take, connection included.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// The waits after the 1st..5th failed attempt; after the 6th, the delivery is `failed`.
pub const RETRIES: [Duration; 5] = [
    Duration::from_secs(60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(30 * 60),
    Duration::from_secs(2 * 60 * 60),
    Duration::from_secs(6 * 60 * 60),
];
/// Webhooks per owner.
pub const MAX_WEBHOOKS: i64 = 20;

/// How long to wait after `attempts` failed attempts before the next one; None = give up.
pub fn backoff(attempts: i32) -> Option<Duration> {
    usize::try_from(attempts).ok().filter(|a| *a >= 1).and_then(|a| RETRIES.get(a - 1).copied())
}

// ---- secrets and signatures -------------------------------------------------------

/// A new signing secret: `whsec_` + 64 hex characters. The whole string (prefix included) is the HMAC key.
pub fn new_secret() -> String {
    let a = Uuid::new_v4().simple().to_string();
    let b = Uuid::new_v4().simple().to_string();
    format!("whsec_{a}{b}")
}

/// Hex HMAC-SHA256 of `"<t>.<body>"` keyed with `secret`.
pub fn sign(secret: &str, t: i64, body: &[u8]) -> String {
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key length");
    mac.update(format!("{t}.").as_bytes());
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// The `Familiar-Signature` header: `t=<t>,v1=<sign(..)>`.
pub fn signature(secret: &str, t: i64, body: &[u8]) -> String {
    format!("t={t},v1={}", sign(secret, t, body))
}

// ---- where a webhook may point --------------------------------------------------------

fn loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v| v.is_loopback()),
    }
}

static ALLOW_LOOPBACK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Let webhooks go to this computer (loopback). On in the desktop app, where the computer is the owner's own and a local
/// CRM is a normal receiver; off by default, because on a shared self-hosted server "this computer" is the server, and a
/// user could aim webhooks (and the Test button's answers) at its internal services. A single-owner headless install
/// opts in with `FAMILIAR_WEBHOOKS_ALLOW_LOOPBACK=1`.
pub fn set_allow_loopback(on: bool) {
    ALLOW_LOOPBACK.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn loopback_allowed() -> bool {
    ALLOW_LOOPBACK.load(std::sync::atomic::Ordering::Relaxed)
        || std::env::var("FAMILIAR_WEBHOOKS_ALLOW_LOOPBACK").is_ok_and(|v| v.trim() == "1")
}

/// Why `ip` may not receive a webhook over `https` (true) or `http` (false); None = it may.
pub fn refused(ip: IpAddr, https: bool) -> Option<&'static str> {
    refused_with(ip, https, loopback_allowed())
}

fn refused_with(ip: IpAddr, https: bool, allow_loopback: bool) -> Option<&'static str> {
    if loopback(ip) {
        return (!allow_loopback).then_some(
            "this computer, which only the desktop app may send webhooks to (single-owner servers: set \
             FAMILIAR_WEBHOOKS_ALLOW_LOOPBACK=1)",
        );
    }
    if !crate::permissions::is_public(ip) {
        return Some(
            "a private, link-local or reserved address: webhooks go to public https addresses or to this computer",
        );
    }
    (!https).then_some("plain http is only allowed to this computer (127.0.0.1 or localhost); use https")
}

/// The URL when it may be a webhook's: http(s), a host, no user name or password in it, at most 1000 characters, and
/// (for an address written as numbers, in any notation the URL standard accepts) an address [`refused`] allows. A host
/// name is checked after DNS ([`resolve`]).
pub fn check_url(s: &str) -> std::result::Result<url::Url, String> {
    let s = s.trim();
    if s.is_empty() || s.chars().count() > 1000 {
        return Err("url must be 1-1000 characters".into());
    }
    let u = url::Url::parse(s).map_err(|_| "url must be an http or https link".to_string())?;
    let https = match u.scheme() {
        "https" => true,
        "http" => false,
        _ => return Err("url must be an http or https link".into()),
    };
    if !u.username().is_empty() || u.password().is_some() {
        return Err("url must not contain a user name or password; put a token in the path or use the signature".into());
    }
    let ip = match u.host() {
        None => return Err("url needs a host".into()),
        Some(url::Host::Domain(_)) => return Ok(u),
        Some(url::Host::Ipv4(v4)) => IpAddr::V4(v4),
        Some(url::Host::Ipv6(v6)) => IpAddr::V6(v6),
    };
    match refused(ip, https) {
        Some(why) => Err(format!("url points at {why}")),
        None => Ok(u),
    }
}

/// The addresses to connect to for `u` (already through [`check_url`]): every address its host resolves to must pass
/// [`refused`].
pub async fn resolve(u: &url::Url) -> std::result::Result<Vec<SocketAddr>, String> {
    let https = u.scheme() == "https";
    let port = u.port_or_known_default().ok_or("url needs a port")?;
    let addrs: Vec<SocketAddr> = match u.host() {
        Some(url::Host::Ipv4(v4)) => vec![SocketAddr::new(IpAddr::V4(v4), port)],
        Some(url::Host::Ipv6(v6)) => vec![SocketAddr::new(IpAddr::V6(v6), port)],
        Some(url::Host::Domain(d)) => {
            let lookup = tokio::net::lookup_host((d, port));
            match tokio::time::timeout(Duration::from_secs(5), lookup).await {
                Ok(Ok(a)) => a.collect(),
                Ok(Err(_)) | Err(_) => return Err(format!("could not resolve {d}")),
            }
        }
        None => return Err("url needs a host".into()),
    };
    if addrs.is_empty() {
        return Err("the host has no address".into());
    }
    for a in &addrs {
        if let Some(why) = refused(a.ip(), https) {
            return Err(format!("the host resolves to {}, {why}", a.ip()));
        }
    }
    Ok(addrs)
}

/// The events of a webhook: known ones, at least one, no duplicates.
pub fn events(v: &[String]) -> std::result::Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for e in v.iter().map(|e| e.trim()) {
        if !EVENTS.contains(&e) {
            return Err(format!("unknown event {e:?}; events are: {}", EVENTS.join(", ")));
        }
        if !out.iter().any(|o| o == e) {
            out.push(e.to_string());
        }
    }
    if out.is_empty() {
        return Err("choose at least one event".into());
    }
    Ok(out)
}

// ---- queueing ------------------------------------------------------------------------

/// One delivery per enabled webhook of `owner` that listens to `event`, in the transaction of the change. The payload
/// gets its delivery `id`, `at` (now) and the teammate's `actor.bot_slug`; a missing `previous` is left out.
pub async fn enqueue(tx: &mut PgConnection, owner: Uuid, event: &str, payload: &Value) -> Result<()> {
    let hooks: Vec<Uuid> =
        sqlx::query_scalar("select id from crm_webhooks where owner_id = $1 and enabled and $2 = any(events) order by created_at")
            .bind(owner)
            .bind(event)
            .fetch_all(&mut *tx)
            .await?;
    if hooks.is_empty() {
        return Ok(());
    }
    let mut p = payload.clone();
    p["event"] = json!(event);
    p["at"] = json!(Utc::now().to_rfc3339());
    if p["previous"].is_null()
        && let Some(o) = p.as_object_mut()
    {
        o.remove("previous");
    }
    if let Some(bot) = p["actor"]["bot_id"].as_str().and_then(|b| b.parse::<Uuid>().ok()) {
        let slug: Option<String> = sqlx::query_scalar("select slug from bots where id = $1 and owner_id = $2")
            .bind(bot)
            .bind(owner)
            .fetch_optional(&mut *tx)
            .await?;
        p["actor"]["bot_slug"] = json!(slug);
    }
    for hook in hooks {
        let id = Uuid::new_v4();
        p["id"] = json!(id);
        sqlx::query(
            "insert into crm_webhook_deliveries (id, owner_id, webhook_id, event, payload) values ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(owner)
        .bind(hook)
        .bind(event)
        .bind(Json(&p))
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

// ---- delivery ------------------------------------------------------------------------

/// Why a delivery attempt failed, and whether trying again later could help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub why: String,
    /// False for what another attempt can't change: a payload too large, a secret that can't be read, a 4xx answer
    /// other than 408 or 429.
    pub retry: bool,
}

impl Failure {
    fn again(why: impl Into<String>) -> Self {
        Failure { why: why.into(), retry: true }
    }
    fn last(why: impl Into<String>) -> Self {
        Failure { why: why.into(), retry: false }
    }
}

/// Whether an answer with this status is worth another attempt: server errors, redirects (the URL may be fixed), 408
/// and 429; not other client errors.
pub fn retryable(status: u16) -> bool {
    !(400..500).contains(&status) || status == 408 || status == 429
}

/// POST one delivery. Ok(HTTP status) for a 2xx answer. The address rules are checked again here, after DNS, and the
/// connection goes only to the addresses checked.
pub async fn send(url: &str, secret: &str, delivery: Uuid, event: &str, body: &[u8]) -> std::result::Result<u16, Failure> {
    if body.len() > MAX_BODY {
        return Err(Failure::last(format!("the payload is larger than {} KB", MAX_BODY / 1024)));
    }
    // The address may be fine again later (DNS changes): retried.
    let u = check_url(url).map_err(Failure::again)?;
    let addrs = resolve(&u).await.map_err(Failure::again)?;
    let mut b = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .connect_timeout(TIMEOUT)
        .no_proxy()
        .user_agent("Familiar-Webhooks/1");
    if let Some(url::Host::Domain(d)) = u.host() {
        b = b.resolve_to_addrs(d, &addrs);
    }
    let client = b.build().map_err(|e| Failure::again(format!("could not build the request: {e}")))?;
    let t = Utc::now().timestamp();
    let resp = client
        .post(u)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("Familiar-Event", event)
        .header("Familiar-Delivery", delivery.to_string())
        .header("Familiar-Signature", signature(secret, t, body))
        .body(body.to_vec())
        .send()
        .await
        .map_err(|e| {
            Failure::again(if e.is_timeout() {
                format!("no answer within {} s", TIMEOUT.as_secs())
            } else if e.is_connect() {
                "could not connect".to_string()
            } else {
                // Not the library's text: it can describe the receiver (TLS details, protocol errors) to whoever reads
                // the delivery log. The daemon's own log keeps the detail.
                tracing::debug!("webhook request failed: {}", e.without_url());
                "the request failed (TLS or protocol error)".to_string()
            })
        })?;
    let status = resp.status().as_u16();
    match status {
        200..=299 => Ok(status),
        300..=399 => Err(Failure::again(format!("HTTP {status} (redirects aren't followed)"))),
        _ if retryable(status) => Err(Failure::again(format!("HTTP {status}"))),
        _ => Err(Failure::last(format!("HTTP {status} (not retried: the receiver refused it)"))),
    }
}

/// The body and the secret of a delivery, then [`send`]. A secret this key can't open is final.
async fn send_sealed(
    secrets: &familiar_crypto::SecretBox,
    secret_enc: &str,
    url: &str,
    id: Uuid,
    event: &str,
    payload: &Value,
) -> std::result::Result<u16, Failure> {
    let Ok(secret) = secrets.decrypt(secret_enc) else {
        return Err(Failure::last("the webhook's secret can't be read with this secret key"));
    };
    let body = serde_json::to_vec(payload).map_err(|e| Failure::last(format!("could not encode the payload: {e}")))?;
    send(url, &secret, id, event, &body).await
}

type Claimed = (Uuid, Uuid, String, Json<Value>, i32, String, String);

/// Record what one attempt did: delivered, or failed (retried later per [`backoff`] when it may help, else `failed`).
async fn settle(db: &Db, id: Uuid, attempts: i32, result: std::result::Result<u16, Failure>) -> Result<()> {
    match result {
        Ok(_) => {
            sqlx::query(
                "update crm_webhook_deliveries set status = 'delivered', delivered_at = now(), last_error = null
                 where id = $1 and owner_id = $2",
            )
            .bind(id)
            .bind(db.owner)
            .execute(&db.pool)
            .await?;
        }
        Err(f) => {
            let why: String = f.why.chars().take(500).collect();
            let wait = backoff(attempts).filter(|_| f.retry).map(|d| d.as_secs_f64());
            sqlx::query(
                "update crm_webhook_deliveries set last_error = $3,
                        status = case when $4::float8 is null then 'failed' else 'pending' end,
                        next_attempt_at = case when $4::float8 is null then next_attempt_at
                                               else now() + make_interval(secs => $4) end
                 where id = $1 and owner_id = $2",
            )
            .bind(id)
            .bind(db.owner)
            .bind(why)
            .bind(wait)
            .execute(&db.pool)
            .await?;
        }
    }
    Ok(())
}

/// Deliver what is due (at most 20 per call, 4 at a time); returns how many were attempted. Each one is claimed first
/// (its attempt counted, and pushed 10 minutes out), so a second daemon or a crash mid-send never sends it twice at once.
pub async fn deliver_due(db: &Db, secrets: &familiar_crypto::SecretBox) -> Result<usize> {
    let due: Vec<Claimed> = sqlx::query_as(
        "update crm_webhook_deliveries d set attempts = d.attempts + 1, next_attempt_at = now() + interval '10 minutes'
         from crm_webhooks w
         where d.id in (select x.id from crm_webhook_deliveries x join crm_webhooks y on y.id = x.webhook_id and y.enabled
                        where x.owner_id = $1 and x.status = 'pending' and x.next_attempt_at <= now()
                        order by x.next_attempt_at limit 20 for update of x skip locked)
           and w.id = d.webhook_id
         returning d.id, d.webhook_id, d.event, d.payload, d.attempts, w.url, w.secret_enc",
    )
    .bind(db.owner)
    .fetch_all(&db.pool)
    .await?;
    let n = due.len();
    futures_util::stream::iter(due)
        .for_each_concurrent(4, |(id, webhook, event, payload, attempts, url, secret_enc)| async move {
            let result = send_sealed(secrets, &secret_enc, &url, id, &event, &payload.0).await;
            if let Err(f) = &result {
                info!(delivery = %id, %webhook, attempts, retry = f.retry, "webhook delivery failed: {}", f.why);
            }
            if let Err(e) = settle(db, id, attempts, result).await {
                warn!(delivery = %id, "recording a webhook delivery failed: {e}");
            }
        })
        .await;
    Ok(n)
}

/// Send a `ping` to one webhook right now (the owner's Test button), then record it as a settled delivery (delivered
/// or failed; never retried, never picked up by the daemon). Returns the delivery row.
pub async fn test(db: &Db, secrets: &familiar_crypto::SecretBox, webhook: Uuid) -> Result<Value> {
    let (url, secret_enc): (String, String) =
        sqlx::query_as("select url, secret_enc from crm_webhooks where id = $1 and owner_id = $2")
            .bind(webhook)
            .bind(db.owner)
            .fetch_optional(&db.pool)
            .await?
            .ok_or(CrmError::NotFound)?;
    let id = Uuid::new_v4();
    let payload = json!({
        "id": id, "event": "ping", "at": Utc::now().to_rfc3339(),
        "data": { "message": "A test delivery from Familiar. Check the Familiar-Signature header with your secret." },
        "actor": { "kind": "user" },
    });
    let result = send_sealed(secrets, &secret_enc, &url, id, "ping", &payload).await;
    let (status, error) = match result {
        Ok(_) => ("delivered", None),
        Err(f) => ("failed", Some(f.why.chars().take(500).collect::<String>())),
    };
    let row: Json<Value> = sqlx::query_scalar(
        "insert into crm_webhook_deliveries (id, owner_id, webhook_id, event, payload, status, attempts, last_error,
                                             delivered_at)
         values ($1, $2, $3, 'ping', $4, $5, 1, $6, case when $5 = 'delivered' then now() end)
         returning to_jsonb(crm_webhook_deliveries) - 'owner_id'",
    )
    .bind(id)
    .bind(db.owner)
    .bind(webhook)
    .bind(Json(&payload))
    .bind(status)
    .bind(error)
    .fetch_one(&db.pool)
    .await?;
    Ok(row.0)
}

/// A delivery as the API shows it.
pub const DELIVERY_VIEW: &str = "select to_jsonb(d) - 'owner_id' from crm_webhook_deliveries d";

/// Delivered and failed deliveries are kept 30 days.
pub async fn prune(db: &Db) -> Result<u64> {
    let r = sqlx::query(
        "delete from crm_webhook_deliveries where owner_id = $1 and status in ('delivered', 'failed')
           and created_at < now() - interval '30 days'",
    )
    .bind(db.owner)
    .execute(&db.pool)
    .await?;
    Ok(r.rows_affected())
}

/// The daemon's delivery loop: on `wake` (a delivery was queued) and every 30 s. Without a secret key nothing can be
/// signed, so nothing is sent.
pub async fn run(db: Db, secrets: Option<Arc<familiar_crypto::SecretBox>>, wake: Arc<Notify>, shutdown: CancellationToken) {
    let mut warned = false;
    for n in 0u64.. {
        match &secrets {
            Some(s) => loop {
                match deliver_due(&db, s).await {
                    // a full batch: there may be more due
                    Ok(20) => continue,
                    Ok(_) => break,
                    Err(e) => {
                        warn!("webhook deliveries failed: {e}");
                        break;
                    }
                }
            },
            None if !warned => {
                let pending: i64 = sqlx::query_scalar(
                    "select count(*) from crm_webhook_deliveries where owner_id = $1 and status = 'pending'",
                )
                .bind(db.owner)
                .fetch_one(&db.pool)
                .await
                .unwrap_or(0);
                if pending > 0 {
                    warn!("{pending} CRM webhook deliveries wait: set secret_key (FAMILIAR_SECRET_KEY) to send them");
                    warned = true;
                }
            }
            None => {}
        }
        if n % 2880 == 0
            && let Ok(k) = prune(&db).await
            && k > 0
        {
            info!("pruned {k} old webhook deliveries");
        }
        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            _ = shutdown.cancelled() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231() {
        // RFC 4231 test case 2: key "Jefe", data "what do ya want for nothing?"
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(b"Jefe").unwrap();
        mac.update(b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(mac.finalize().into_bytes()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn signature_covers_time_and_body() {
        let body = br#"{"event":"company.created"}"#;
        let s = signature("whsec_abc", 1_760_000_000, body);
        let v1 = s.strip_prefix("t=1760000000,v1=").expect(&s);
        assert_eq!(v1.len(), 64);
        // what a receiver computes: HMAC-SHA256(secret, "<t>.<body>")
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(b"whsec_abc").unwrap();
        mac.update(b"1760000000.");
        mac.update(body);
        assert_eq!(v1, hex::encode(mac.finalize().into_bytes()));
        assert_ne!(sign("whsec_abc", 1_760_000_001, body), v1, "the time is signed");
        assert_ne!(sign("whsec_abd", 1_760_000_000, body), v1, "the secret matters");
        assert_ne!(sign("whsec_abc", 1_760_000_000, b"{}"), v1, "the body is signed");
        let secret = new_secret();
        assert!(secret.starts_with("whsec_") && secret.len() == 70 && secret[6..].bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(new_secret(), secret);
    }

    #[test]
    fn which_answers_are_retried() {
        for s in [500, 502, 503, 301, 302, 408, 429] {
            assert!(retryable(s), "{s}");
        }
        for s in [400, 401, 403, 404, 410, 413, 422] {
            assert!(!retryable(s), "{s}");
        }
    }

    #[test]
    fn retries_then_gives_up() {
        let mins: Vec<u64> = (1..=5).map(|a| backoff(a).unwrap().as_secs() / 60).collect();
        assert_eq!(mins, vec![1, 5, 30, 120, 360]);
        assert_eq!(backoff(6), None);
        assert_eq!(backoff(0), None);
        assert_eq!(backoff(-1), None);
    }

    #[test]
    fn loopback_only_when_allowed() {
        for ip in ["127.0.0.1", "127.9.9.9", "::1", "::ffff:127.0.0.1"] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(refused_with(ip, false, false).unwrap().contains("only the desktop app"), "{ip}");
            assert!(refused_with(ip, true, false).is_some(), "{ip}");
            assert_eq!(refused_with(ip, false, true), None, "{ip}");
        }
        // The switch changes nothing else.
        let lan: IpAddr = "10.0.0.5".parse().unwrap();
        assert!(refused_with(lan, true, true).is_some());
        assert_eq!(refused_with("8.8.8.8".parse().unwrap(), true, false), None);
    }

    #[test]
    fn address_rules() {
        // As in the desktop app (the tests share one process; none of them turns it off).
        set_allow_loopback(true);
        let ok = |u: &str| check_url(u).is_ok();
        for u in [
            "https://hooks.zapier.com/hooks/catch/1/abc/", "https://8.8.8.8/hook", "https://[2606:4700:4700::1111]/x",
            "http://127.0.0.1:8080/hook", "https://127.0.0.1/", "http://[::1]:9000/", "http://127.1:3000/",
            "http://0x7f.0.0.1/", "http://2130706433/", "http://[::ffff:127.0.0.1]/", "http://localhost:3000/",
        ] {
            assert!(ok(u), "{u}");
        }
        for u in [
            // plain http to the internet
            "http://8.8.8.8/hook", "http://[2606:4700:4700::1111]/",
            // private LAN, even with https
            "https://10.0.0.5/", "https://192.168.1.10/", "https://172.16.0.1/", "https://100.64.0.1/", "https://[fd00::1]/",
            "https://0xa000005/", "https://10.1/", "https://[::ffff:10.0.0.1]/", "https://[::ffff:192.168.0.1]/",
            // link-local / cloud metadata, in every spelling
            "https://169.254.169.254/latest/meta-data", "http://169.254.169.254/", "https://0xa9fea9fe/",
            "https://2852039166/", "https://[fe80::1]/", "https://[::ffff:169.254.169.254]/", "https://[64:ff9b::a9fe:a9fe]/",
            "https://[2002:a9fe:a9fe::1]/", "https://[fd00:ec2::254]/",
            // unspecified, multicast, broadcast, documentation, reserved, Teredo, site-local
            "https://0.0.0.0/", "http://0.0.0.0:8080/", "https://[::]/", "https://224.0.0.1/", "https://[ff02::1]/",
            "https://255.255.255.255/", "https://192.0.2.1/", "https://[2001:db8::1]/", "https://240.0.0.1/",
            "https://[2001::1]/", "https://[fec0::1]/", "https://198.18.0.1/",
            // not a webhook URL at all
            "ftp://example.com/", "file:///etc/passwd", "javascript:alert(1)", "https://", "", "not a url",
            "https://user:pw@example.com/", "https://token@example.com/",
        ] {
            assert!(!ok(u), "{u}");
        }
        assert!(check_url(&format!("https://example.com/{}", "x".repeat(1000))).is_err());
        assert!(check_url("http://8.8.8.8/").unwrap_err().contains("use https"));
        assert!(check_url("https://10.0.0.1/").unwrap_err().contains("private"));
    }

    #[tokio::test]
    async fn names_are_checked_after_dns() {
        // localhost is this computer: allowed over http (in the desktop app)
        set_allow_loopback(true);
        let u = check_url("http://localhost:9/hook").unwrap();
        let addrs = resolve(&u).await.unwrap();
        assert!(addrs.iter().all(|a| a.ip().is_loopback() && a.port() == 9), "{addrs:?}");
        // a name that doesn't resolve is refused
        let u = check_url("https://no-such-host.invalid/").unwrap();
        assert!(resolve(&u).await.unwrap_err().contains("could not resolve"));
    }

    #[test]
    fn known_events_only() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(events(&v(&["deal.stage_changed", " deal.stage_changed ", "company.created"])).unwrap(), v(&["deal.stage_changed", "company.created"]));
        assert!(events(&v(&[])).is_err());
        assert!(events(&v(&["deal.deleted"])).is_err());
        assert_eq!(events(&v(&EVENTS)).unwrap().len(), 9);
    }
}
