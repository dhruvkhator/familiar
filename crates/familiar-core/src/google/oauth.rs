//! "Connect Google": OAuth 2.0 for installed apps (RFC 8252) with PKCE (RFC 7636, S256) and a loopback redirect.
//!
//! The sign-in page opens in the owner's own browser (the app calls the system's URL opener; never a teammate's
//! browser, never an embedded view). Google sends the browser back to `http://127.0.0.1:<random port>/` with `state`
//! and `code`. The listener ([`Loopback`]) answers exactly one such redirect and closes; requests without a `state`
//! (a favicon, a stray local probe) are refused, at most [`MAX_STRAY`] of them; a wrong `state` ends the sign-in (a
//! forged redirect); the whole wait is bounded. The page it shows never echoes anything from the request.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::{OAuthClient, SCOPES};

/// How long the owner has to finish signing in.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Requests without a `state` the listener refuses before it gives up.
pub const MAX_STRAY: usize = 16;
const MAX_REQUEST: usize = 8 * 1024;

/// A PKCE pair: the verifier stays here, the challenge goes in the sign-in URL.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pkce(…)")
    }
}

/// S256: base64url(SHA-256(verifier)), no padding.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A fresh verifier (43 characters from 32 random bytes, RFC 7636's minimum length and entropy) and its challenge.
pub fn pkce() -> Pkce {
    let verifier = super::random_token();
    Pkce { challenge: challenge(&verifier), verifier }
}

/// Google's sign-in page for this client: offline access (a refresh token), the consent screen every time (so a
/// refresh token always comes back), PKCE and `state`.
pub fn auth_url(client: &OAuthClient, redirect_uri: &str, state: &str, challenge: &str) -> String {
    let mut u = url::Url::parse(&client.auth_url).expect("checked endpoint");
    u.query_pairs_mut()
        .append_pair("client_id", &client.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", &SCOPES.join(" "))
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");
    u.into()
}

/// Why a sign-in ended without a code.
#[derive(Debug, PartialEq, Eq)]
pub enum FlowError {
    Timeout,
    Cancelled,
    /// The owner said no on Google's page (or Google refused).
    Denied,
    /// A redirect with another `state`: not the sign-in Familiar started.
    StateMismatch,
    /// Too many requests that weren't the redirect.
    TooManyStray,
    Io(String),
}

impl FlowError {
    pub fn message(&self) -> String {
        match self {
            FlowError::Timeout => "The sign-in took longer than 5 minutes. Try again.".into(),
            FlowError::Cancelled => "Cancelled.".into(),
            FlowError::Denied => "Google didn't give access (the sign-in was cancelled or not allowed).".into(),
            FlowError::StateMismatch => {
                "The answer that came back wasn't for this sign-in, so Familiar ignored it. Try again.".into()
            }
            FlowError::TooManyStray => "Something else kept calling the sign-in address. Try again.".into(),
            FlowError::Io(e) => format!("The sign-in couldn't finish ({e})."),
        }
    }
}

/// The one-shot redirect listener on `127.0.0.1:<random port>`.
pub struct Loopback {
    listener: TcpListener,
    pub redirect_uri: String,
}

impl Loopback {
    pub async fn bind() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let redirect_uri = format!("http://127.0.0.1:{}/", listener.local_addr()?.port());
        Ok(Loopback { listener, redirect_uri })
    }

    /// Wait for Google's redirect with this `state`; returns the code. The listener closes when this returns.
    pub async fn wait(self, state: &str, timeout: Duration, cancel: impl std::future::Future<Output = ()>) -> Result<String, FlowError> {
        let deadline = tokio::time::Instant::now() + timeout;
        tokio::pin!(cancel);
        let mut stray = 0;
        loop {
            let (mut conn, _) = tokio::select! {
                a = self.listener.accept() => a.map_err(|e| FlowError::Io(e.kind().to_string()))?,
                _ = tokio::time::sleep_until(deadline) => return Err(FlowError::Timeout),
                _ = &mut cancel => return Err(FlowError::Cancelled),
            };
            let target = match tokio::time::timeout(Duration::from_secs(10), read_target(&mut conn)).await {
                Ok(Some(t)) => t,
                _ => {
                    let _ = respond(&mut conn, 400, PAGE_WAITING).await;
                    stray += 1;
                    if stray >= MAX_STRAY {
                        return Err(FlowError::TooManyStray);
                    }
                    continue;
                }
            };
            match redirect(&target, state) {
                Redirect::Stray => {
                    let _ = respond(&mut conn, 404, PAGE_WAITING).await;
                    stray += 1;
                    if stray >= MAX_STRAY {
                        return Err(FlowError::TooManyStray);
                    }
                }
                Redirect::Code(code) => {
                    let _ = respond(&mut conn, 200, PAGE_DONE).await;
                    return Ok(code);
                }
                Redirect::Failed(e) => {
                    let _ = respond(&mut conn, 400, PAGE_FAILED).await;
                    return Err(e);
                }
            }
        }
    }
}

/// What one request to the listener is.
#[derive(Debug, PartialEq, Eq)]
enum Redirect {
    /// Not the redirect (no `state`, another path): refused, keep waiting.
    Stray,
    Code(String),
    Failed(FlowError),
}

fn redirect(target: &str, state: &str) -> Redirect {
    let Ok(u) = url::Url::parse(&format!("http://127.0.0.1{target}")) else { return Redirect::Stray };
    if u.path() != "/" {
        return Redirect::Stray;
    }
    let get = |k: &str| u.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned());
    let Some(got) = get("state") else { return Redirect::Stray };
    if !same(got.as_bytes(), state.as_bytes()) {
        return Redirect::Failed(FlowError::StateMismatch);
    }
    if get("error").is_some() {
        return Redirect::Failed(FlowError::Denied);
    }
    match get("code").filter(|c| !c.is_empty() && c.len() <= 2048) {
        Some(code) => Redirect::Code(code),
        None => Redirect::Failed(FlowError::Denied),
    }
}

/// Equal, in time that doesn't depend on where they differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The request target of a `GET` (headers read up to [`MAX_REQUEST`] bytes).
async fn read_target(conn: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = conn.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_REQUEST {
            return None;
        }
    }
    let line = std::str::from_utf8(&buf).ok()?.lines().next()?.to_owned();
    let mut parts = line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    (method == "GET" && target.starts_with('/') && version.starts_with("HTTP/1.")).then(|| target.to_owned())
}

const PAGE_DONE: &str = "Familiar is connected to your Google account. You can close this tab and go back to Familiar.";
const PAGE_FAILED: &str = "Familiar couldn't connect. Go back to Familiar to see why and try again.";
const PAGE_WAITING: &str = "Familiar is waiting for Google's sign-in.";

async fn respond(conn: &mut TcpStream, status: u16, text: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    };
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Familiar</title><body style=\"font:16px system-ui;margin:48px\"><p>{text}</p></body>"
    );
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         Referrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\n\
         X-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    conn.write_all(head.as_bytes()).await?;
    conn.write_all(body.as_bytes()).await?;
    conn.shutdown().await
}

// ---- tokens ------------------------------------------------------------------------------------------------------

/// What the code exchange returns. Debug never shows the tokens.
pub struct Grant {
    pub refresh_token: String,
    pub access_token: String,
    pub scopes: Vec<String>,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant").field("scopes", &self.scopes).finish_non_exhaustive()
    }
}

fn form(pairs: &[(&str, &str)]) -> String {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        s.append_pair(k, v);
    }
    s.finish()
}

async fn post_form(http: &reqwest::Client, url: &str, body: String) -> Result<(u16, Value), String> {
    let r = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(reqwest::header::ACCEPT, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| if e.is_timeout() { "timed out".to_owned() } else { "could not connect".to_owned() })?;
    let status = r.status().as_u16();
    let text = r.text().await.map_err(|_| "the answer was cut off".to_owned())?;
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

/// Trade the code for tokens (with the PKCE verifier). Refuses an answer without a refresh token.
pub async fn exchange(http: &reqwest::Client, client: &OAuthClient, code: &str, verifier: &str, redirect_uri: &str) -> Result<Grant, String> {
    let body = form(&[
        ("code", code),
        ("client_id", &client.client_id),
        ("client_secret", &client.client_secret),
        ("redirect_uri", redirect_uri),
        ("grant_type", "authorization_code"),
        ("code_verifier", verifier),
    ]);
    let (status, v) = post_form(http, &client.token_url, body).await?;
    if status != 200 {
        // Google's error code only (`invalid_grant`, `invalid_client`), never the request.
        let code = v["error"].as_str().filter(|e| e.len() < 64 && e.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).unwrap_or("error");
        return Err(format!("Google refused the sign-in ({code}, HTTP {status})"));
    }
    let access_token = v["access_token"].as_str().filter(|t| !t.is_empty()).ok_or("Google's answer had no access token")?.to_owned();
    let refresh_token = v["refresh_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or("Google didn't send a refresh token. Remove Familiar under Third-party access in your Google account, then connect again.")?
        .to_owned();
    let scopes = v["scope"].as_str().unwrap_or_default().split_whitespace().map(str::to_owned).collect();
    Ok(Grant { refresh_token, access_token, scopes })
}

/// After the redirect: trade the code, and find out which account it is (Gmail's profile, else the primary calendar's
/// id when the owner didn't grant Gmail). The access token is dropped here; the daemon gets its own.
pub async fn finish(http: &reqwest::Client, client: &OAuthClient, code: &str, verifier: &str, redirect_uri: &str) -> Result<(String, Grant), String> {
    let grant = exchange(http, client, code, verifier, redirect_uri).await?;
    let email = match super::api::profile_email(http, client, &grant.access_token).await {
        Ok(e) => e,
        Err(_) => super::api::calendar_email(http, client, &grant.access_token).await.map_err(|e| e.message())?,
    };
    let email = super::mime::plain_address(&email).ok_or("Google named an account Familiar can't use")?;
    Ok((email, grant))
}

#[derive(Debug)]
pub enum RefreshError {
    /// `invalid_grant`: the refresh token no longer works.
    Revoked,
    Other(String),
}

/// A new access token and its lifetime in seconds.
pub async fn refresh(http: &reqwest::Client, client: &OAuthClient, refresh_token: &str) -> Result<(String, u64), RefreshError> {
    let body = form(&[
        ("client_id", &client.client_id),
        ("client_secret", &client.client_secret),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ]);
    let (status, v) = post_form(http, &client.token_url, body).await.map_err(RefreshError::Other)?;
    if status == 400 && v["error"] == "invalid_grant" {
        return Err(RefreshError::Revoked);
    }
    if status != 200 {
        return Err(RefreshError::Other(format!("HTTP {status}")));
    }
    let token = v["access_token"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| RefreshError::Other("no access token".into()))?;
    Ok((token.to_owned(), v["expires_in"].as_u64().unwrap_or(3600)))
}

/// Revoke a token at Google (a refresh token ends the whole grant). True when Google confirmed it, or the token was
/// already invalid.
pub async fn revoke(http: &reqwest::Client, client: &OAuthClient, token: &str) -> bool {
    match post_form(http, &client.revoke_url, form(&[("token", token)])).await {
        Ok((200, _)) => true,
        Ok((400, v)) => v["error"] == "invalid_token",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> OAuthClient {
        OAuthClient {
            client_id: "123.apps.googleusercontent.com".into(),
            client_secret: "s".into(),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: String::new(),
            revoke_url: String::new(),
            gmail_url: String::new(),
            calendar_url: String::new(),
        }
    }

    #[test]
    fn pkce_s256_matches_rfc_7636() {
        // RFC 7636, appendix B.
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let p = pkce();
        assert_eq!(p.verifier.len(), 43);
        assert_eq!(p.challenge, challenge(&p.verifier));
        assert_ne!(pkce().verifier, p.verifier);
        assert!(!format!("{p:?}").contains(&p.verifier));
    }

    #[test]
    fn the_sign_in_url_asks_for_exactly_what_is_needed() {
        let u = url::Url::parse(&auth_url(&client(), "http://127.0.0.1:5555/", "st", "ch")).unwrap();
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(u.host_str(), Some("accounts.google.com"));
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:5555/");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], "ch");
        assert_eq!(q["state"], "st");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["access_type"], "offline");
        assert_eq!(q["scope"].split(' ').collect::<Vec<_>>(), SCOPES);
        assert!(!q.contains_key("client_secret"));
    }

    #[test]
    fn redirects_are_checked() {
        assert_eq!(redirect("/?state=abc&code=4%2F0Ab", "abc"), Redirect::Code("4/0Ab".into()));
        assert_eq!(redirect("/?state=abd&code=x", "abc"), Redirect::Failed(FlowError::StateMismatch));
        assert_eq!(redirect("/?state=ab&code=x", "abc"), Redirect::Failed(FlowError::StateMismatch));
        assert_eq!(redirect("/?state=abc&error=access_denied", "abc"), Redirect::Failed(FlowError::Denied));
        assert_eq!(redirect("/?state=abc", "abc"), Redirect::Failed(FlowError::Denied));
        for stray in ["/favicon.ico", "/?code=x", "/", "/other?state=abc&code=x", "garbage"] {
            assert_eq!(redirect(stray, "abc"), Redirect::Stray, "{stray}");
        }
    }

    async fn get(port: u16, target: &str) -> String {
        let mut c = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        c.write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes()).await.unwrap();
        let mut out = String::new();
        let _ = c.read_to_string(&mut out).await;
        out
    }

    fn port(l: &Loopback) -> u16 {
        l.redirect_uri.trim_start_matches("http://127.0.0.1:").trim_end_matches('/').parse().unwrap()
    }

    #[tokio::test]
    async fn the_listener_takes_one_redirect_then_closes() {
        let l = Loopback::bind().await.unwrap();
        assert!(l.redirect_uri.starts_with("http://127.0.0.1:") && l.redirect_uri.ends_with('/'));
        let p = port(&l);
        let wait = tokio::spawn(l.wait("s1", Duration::from_secs(10), std::future::pending()));
        // a favicon request and a probe without state are refused; the sign-in goes on
        assert!(get(p, "/favicon.ico").await.starts_with("HTTP/1.1 404"));
        assert!(get(p, "/?code=stolen").await.starts_with("HTTP/1.1 404"));
        let page = get(p, "/?state=s1&code=the-code&scope=x").await;
        assert!(page.starts_with("HTTP/1.1 200") && page.contains("no-store") && !page.contains("the-code"), "{page}");
        assert_eq!(wait.await.unwrap(), Ok("the-code".into()));
        // closed: nobody answers any more
        assert!(TcpStream::connect(("127.0.0.1", p)).await.is_err());
    }

    #[tokio::test]
    async fn a_forged_redirect_ends_the_sign_in() {
        let l = Loopback::bind().await.unwrap();
        let p = port(&l);
        let wait = tokio::spawn(l.wait("right", Duration::from_secs(10), std::future::pending()));
        let page = get(p, "/?state=wrong&code=c").await;
        assert!(page.starts_with("HTTP/1.1 400") && !page.contains("wrong"), "{page}");
        assert_eq!(wait.await.unwrap(), Err(FlowError::StateMismatch));
    }

    #[tokio::test]
    async fn the_sign_in_times_out_or_is_cancelled() {
        let l = Loopback::bind().await.unwrap();
        assert_eq!(l.wait("s", Duration::from_millis(50), std::future::pending()).await, Err(FlowError::Timeout));
        let l = Loopback::bind().await.unwrap();
        assert_eq!(l.wait("s", Duration::from_secs(10), async {}).await, Err(FlowError::Cancelled));
    }

    #[tokio::test]
    async fn too_many_stray_requests_end_it() {
        let l = Loopback::bind().await.unwrap();
        let p = port(&l);
        let wait = tokio::spawn(l.wait("s", Duration::from_secs(20), std::future::pending()));
        for _ in 0..MAX_STRAY {
            let _ = get(p, "/x").await;
        }
        assert_eq!(wait.await.unwrap(), Err(FlowError::TooManyStray));
    }

    #[tokio::test]
    async fn only_get_requests_count() {
        let l = Loopback::bind().await.unwrap();
        let p = port(&l);
        let wait = tokio::spawn(l.wait("s", Duration::from_secs(10), std::future::pending()));
        let mut c = TcpStream::connect(("127.0.0.1", p)).await.unwrap();
        c.write_all(b"POST /?state=s&code=c HTTP/1.1\r\n\r\n").await.unwrap();
        let mut out = String::new();
        let _ = c.read_to_string(&mut out).await;
        assert!(out.starts_with("HTTP/1.1 400"));
        assert!(get(p, "/?state=s&code=ok").await.starts_with("HTTP/1.1 200"));
        assert_eq!(wait.await.unwrap(), Ok("ok".into()));
    }
}
