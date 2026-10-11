//! Gmail and Google Calendar over their REST APIs (OAuth accounts). Ids that go into a URL path are checked first
//! (Gmail ids are hex), queries go in as encoded parameters, and every answer is bounded before it is parsed.

use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use serde_json::{Value, json};

use super::{OAuthClient, mime};

/// The largest answer Familiar reads from Google.
const MAX_ANSWER: usize = 8 * 1024 * 1024;

/// Why a call failed.
#[derive(Debug)]
pub enum ApiError {
    /// 401: the access token stopped working (refresh and try once more).
    Unauthorized,
    /// 403 for a missing scope, or the owner turned something off.
    Forbidden,
    NotFound,
    RateLimited,
    Other(String),
}

impl ApiError {
    pub fn message(&self) -> String {
        match self {
            ApiError::Unauthorized => "Google didn't accept Familiar's access; try again.".into(),
            ApiError::Forbidden => "Google refused: your owner didn't give Familiar this access when connecting. They can reconnect and tick it.".into(),
            ApiError::NotFound => "Not found (check the id: it comes from gmail_search or calendar_list).".into(),
            ApiError::RateLimited => "Google is limiting how fast Familiar can go; wait a minute and try again.".into(),
            ApiError::Other(e) => e.clone(),
        }
    }
}

type R<T> = Result<T, ApiError>;

/// A Gmail id (message, thread, draft): hex or Google's other URL-safe ids, short.
pub fn id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

async fn call(http: &reqwest::Client, req: reqwest::RequestBuilder, token: &str) -> R<Value> {
    let _ = http;
    let r = req.bearer_auth(token).header(reqwest::header::ACCEPT, "application/json").send().await.map_err(|e| {
        ApiError::Other(if e.is_timeout() { "Google didn't answer in time".into() } else { "could not reach Google".into() })
    })?;
    let status = r.status().as_u16();
    if r.content_length().is_some_and(|n| n as usize > MAX_ANSWER) {
        return Err(ApiError::Other("Google's answer was too big".into()));
    }
    let bytes = r.bytes().await.map_err(|_| ApiError::Other("Google's answer was cut off".into()))?;
    if bytes.len() > MAX_ANSWER {
        return Err(ApiError::Other("Google's answer was too big".into()));
    }
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    match status {
        200..=299 => Ok(v),
        401 => Err(ApiError::Unauthorized),
        403 if v["error"]["errors"][0]["reason"].as_str().is_some_and(|r| r.contains("ateLimit")) => Err(ApiError::RateLimited),
        403 => Err(ApiError::Forbidden),
        404 => Err(ApiError::NotFound),
        429 => Err(ApiError::RateLimited),
        s => Err(ApiError::Other(format!("Google answered HTTP {s}"))),
    }
}

fn url(base: &str, path: &str, query: &[(&str, &str)]) -> String {
    let mut u = url::Url::parse(&format!("{base}{path}")).expect("checked endpoint");
    if !query.is_empty() {
        let mut q = u.query_pairs_mut();
        for (k, v) in query {
            q.append_pair(k, v);
        }
    }
    u.into()
}

/// The connected address (`users.getProfile`).
pub async fn profile_email(http: &reqwest::Client, c: &OAuthClient, token: &str) -> R<String> {
    let v = call(http, http.get(url(&c.gmail_url, "/gmail/v1/users/me/profile", &[])), token).await?;
    v["emailAddress"].as_str().map(str::to_owned).ok_or_else(|| ApiError::Other("Google didn't say which account this is".into()))
}

/// Without Gmail access, the primary calendar's id is the address.
pub async fn calendar_email(http: &reqwest::Client, c: &OAuthClient, token: &str) -> R<String> {
    let v = call(http, http.get(url(&c.calendar_url, "/calendars/primary/events", &[("maxResults", "1")])), token).await?;
    v["summary"].as_str().map(str::to_owned).ok_or_else(|| ApiError::Other("Google didn't say which account this is".into()))
}

/// A found message: id, thread id, Gmail's snippet and its headers.
#[derive(Debug, Clone)]
pub struct Summary {
    pub id: String,
    pub thread_id: String,
    pub snippet: String,
    pub mail: mime::Mail,
}

fn header_list(payload: &Value) -> Vec<(String, String)> {
    payload["headers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| Some((h["name"].as_str()?.to_ascii_lowercase(), h["value"].as_str()?.to_owned())))
        .take(500)
        .collect()
}

fn mail_headers(payload: &Value) -> mime::Mail {
    let hs = header_list(payload);
    let get = |k: &str| hs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    mime::Mail {
        from: get("from"),
        to: get("to"),
        cc: get("cc"),
        subject: get("subject"),
        date: get("date"),
        message_id: get("message-id"),
        references: get("references"),
        ..Default::default()
    }
}

/// Search with Gmail's query syntax; up to `max` (≤ 25) messages, newest first.
pub async fn search(http: &reqwest::Client, c: &OAuthClient, token: &str, query: &str, max: usize) -> R<Vec<Summary>> {
    let max_s = max.to_string();
    let q = if query.trim().is_empty() { "in:inbox" } else { query };
    let list = call(http, http.get(url(&c.gmail_url, "/gmail/v1/users/me/messages", &[("q", q), ("maxResults", &max_s)])), token).await?;
    let mut out = Vec::new();
    for m in list["messages"].as_array().into_iter().flatten().take(max) {
        let Some(id) = m["id"].as_str().filter(|i| id_ok(i)) else { continue };
        let path = format!("/gmail/v1/users/me/messages/{id}");
        let query: Vec<(&str, &str)> = vec![
            ("format", "metadata"),
            ("metadataHeaders", "From"),
            ("metadataHeaders", "To"),
            ("metadataHeaders", "Cc"),
            ("metadataHeaders", "Subject"),
            ("metadataHeaders", "Date"),
        ];
        let v = call(http, http.get(url(&c.gmail_url, &path, &query)), token).await?;
        out.push(Summary {
            id: id.to_owned(),
            thread_id: v["threadId"].as_str().unwrap_or_default().to_owned(),
            snippet: v["snippet"].as_str().unwrap_or_default().to_owned(),
            mail: mail_headers(&v["payload"]),
        });
    }
    Ok(out)
}

fn b64(data: &str) -> Vec<u8> {
    let d = data.trim_end_matches('=');
    URL_SAFE_NO_PAD.decode(d).or_else(|_| URL_SAFE.decode(data)).unwrap_or_default()
}

/// A full-format message: its headers and text (plain preferred, else HTML as text), attachments by name.
pub fn full_message(v: &Value) -> mime::Mail {
    let mut mail = mail_headers(&v["payload"]);
    let mut plain = None;
    let mut html = None;
    let mut attachments = Vec::new();
    let mut stack = vec![(&v["payload"], 0usize)];
    let mut seen = 0;
    while let Some((p, depth)) = stack.pop() {
        seen += 1;
        if seen > 64 || depth > 8 {
            break;
        }
        let mime_type = p["mimeType"].as_str().unwrap_or_default().to_ascii_lowercase();
        let filename = p["filename"].as_str().unwrap_or_default();
        if !filename.is_empty() {
            if attachments.len() < 50 {
                attachments.push(filename.chars().filter(|c| !c.is_control()).take(120).collect());
            }
            continue;
        }
        if let Some(parts) = p["parts"].as_array() {
            for part in parts.iter().rev() {
                stack.push((part, depth + 1));
            }
            continue;
        }
        let Some(data) = p["body"]["data"].as_str() else { continue };
        let text = String::from_utf8_lossy(&b64(data)).into_owned();
        match mime_type.as_str() {
            "text/plain" if plain.is_none() => plain = Some(text),
            "text/html" if html.is_none() => html = Some(text),
            _ => {}
        }
    }
    let body = match (plain, html) {
        (Some(p), _) if !p.trim().is_empty() => p,
        (_, Some(h)) => mime::html_to_text(&h),
        (p, None) => p.unwrap_or_default(),
    };
    mail.body = mime::clip(&body.replace("\r\n", "\n"), mime::MAX_BODY);
    mail.attachments = attachments;
    mail
}

/// One message, or a whole thread (its newest `max` messages, oldest first): (id, thread id, mail).
pub async fn read(http: &reqwest::Client, c: &OAuthClient, token: &str, id: &str, thread: bool, max: usize) -> R<Vec<(String, String, mime::Mail)>> {
    if !id_ok(id) {
        return Err(ApiError::NotFound);
    }
    if thread {
        let v = call(http, http.get(url(&c.gmail_url, &format!("/gmail/v1/users/me/threads/{id}"), &[("format", "full")])), token).await?;
        let msgs = v["messages"].as_array().cloned().unwrap_or_default();
        let skip = msgs.len().saturating_sub(max);
        return Ok(msgs
            .iter()
            .skip(skip)
            .map(|m| (m["id"].as_str().unwrap_or_default().to_owned(), m["threadId"].as_str().unwrap_or_default().to_owned(), full_message(m)))
            .collect());
    }
    let v = call(http, http.get(url(&c.gmail_url, &format!("/gmail/v1/users/me/messages/{id}"), &[("format", "full")])), token).await?;
    Ok(vec![(id.to_owned(), v["threadId"].as_str().unwrap_or_default().to_owned(), full_message(&v))])
}

fn raw_b64(raw: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(raw)
}

/// Send a composed message (in a thread, when replying). Returns the sent message's id.
pub async fn send(http: &reqwest::Client, c: &OAuthClient, token: &str, raw: &[u8], thread_id: Option<&str>) -> R<String> {
    let mut body = json!({ "raw": raw_b64(raw) });
    if let Some(t) = thread_id.filter(|t| id_ok(t)) {
        body["threadId"] = json!(t);
    }
    let v = call(http, http.post(url(&c.gmail_url, "/gmail/v1/users/me/messages/send", &[])).json(&body), token).await?;
    Ok(v["id"].as_str().unwrap_or_default().to_owned())
}

/// Save a draft in Gmail. Returns the draft's id.
pub async fn draft(http: &reqwest::Client, c: &OAuthClient, token: &str, raw: &[u8], thread_id: Option<&str>) -> R<String> {
    let mut message = json!({ "raw": raw_b64(raw) });
    if let Some(t) = thread_id.filter(|t| id_ok(t)) {
        message["threadId"] = json!(t);
    }
    let v = call(http, http.post(url(&c.gmail_url, "/gmail/v1/users/me/drafts", &[])).json(&json!({ "message": message })), token).await?;
    Ok(v["id"].as_str().unwrap_or_default().to_owned())
}

/// Events of the primary calendar between two RFC 3339 times (single events, by start time).
pub async fn events(http: &reqwest::Client, c: &OAuthClient, token: &str, from: &str, to: &str, query: Option<&str>, max: usize) -> R<Vec<Value>> {
    let max_s = max.to_string();
    let mut q = vec![("timeMin", from), ("timeMax", to), ("singleEvents", "true"), ("orderBy", "startTime"), ("maxResults", max_s.as_str())];
    if let Some(text) = query.filter(|t| !t.trim().is_empty()) {
        q.push(("q", text));
    }
    let v = call(http, http.get(url(&c.calendar_url, "/calendars/primary/events", &q)), token).await?;
    Ok(v["items"].as_array().cloned().unwrap_or_default().into_iter().take(max).collect())
}

/// Create an event on the primary calendar; invitations go out only when it has attendees.
pub async fn create_event(http: &reqwest::Client, c: &OAuthClient, token: &str, event: &Value, invite: bool) -> R<Value> {
    let updates = if invite { "all" } else { "none" };
    call(http, http.post(url(&c.calendar_url, "/calendars/primary/events", &[("sendUpdates", updates)])).json(event), token).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_that_go_into_paths_are_checked() {
        assert!(id_ok("18c2f0a1b2c3d4e5"));
        assert!(id_ok("r-123_abc"));
        for bad in ["", "../profile", "a/b", "a?b", "a b", "%2e%2e", &"a".repeat(65)] {
            assert!(!id_ok(bad), "{bad}");
        }
    }

    #[test]
    fn full_messages_prefer_plain_text_and_name_attachments() {
        let enc = |s: &str| URL_SAFE_NO_PAD.encode(s);
        let v = json!({ "id": "1", "threadId": "t", "payload": {
            "mimeType": "multipart/mixed",
            "headers": [{ "name": "From", "value": "Sam <sam@acme.com>" }, { "name": "Subject", "value": "Hi" }],
            "parts": [
                { "mimeType": "multipart/alternative", "parts": [
                    { "mimeType": "text/html", "body": { "data": enc("<p>html <b>version</b></p>") } },
                    { "mimeType": "text/plain", "body": { "data": enc("plain version\r\nline 2") } } ] },
                { "mimeType": "application/pdf", "filename": "deck.pdf", "body": { "attachmentId": "a1" } } ] } });
        let m = full_message(&v);
        assert_eq!(m.from, "Sam <sam@acme.com>");
        assert_eq!(m.body, "plain version\nline 2");
        assert_eq!(m.attachments, vec!["deck.pdf".to_owned()]);
        let html_only = json!({ "payload": { "mimeType": "text/html", "body": { "data": enc("<p>Only <i>html</i></p><script>x()</script>") } } });
        let body = full_message(&html_only).body;
        assert!(body.contains("Only") && body.contains("html") && !body.contains("x()") && !body.contains('<'), "{body:?}");
    }
}
