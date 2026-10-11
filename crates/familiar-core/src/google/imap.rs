//! Gmail over IMAP with an app password, on the maintained `async-imap` crate (MIT OR Apache-2.0; its `imap-proto`
//! parser handles the protocol). Implicit TLS on 993 only ([`super::tls`]: rustls with the platform verifier).
//!
//! Gmail's own extensions: the special-use mailboxes (`\All`, `\Drafts`, whatever the account's language calls
//! them), `X-GM-RAW` (the same query syntax as Gmail's search box) and `X-GM-MSGID` (the decimal form of the Gmail
//! API's hex message id, so a teammate sees the same ids however Familiar is connected). Reading is `EXAMINE` +
//! `BODY.PEEK` (nothing gets marked as read); a draft is an `APPEND` to Drafts with the `\Draft` flag. A message over
//! [`MAX_MESSAGE`] is read by its headers only. `async-imap` quotes mailbox names and the login; the search query
//! Familiar quotes itself after refusing control characters, so nothing a teammate passes can start a command.

use std::time::Duration;

use async_imap::types::NameAttribute;
use futures_util::TryStreamExt;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

use super::{MailServers, mime, tls};

/// The largest message read whole (bigger ones: headers only).
pub const MAX_MESSAGE: u32 = 5 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq)]
pub enum MailError {
    /// The server refused the address and app password.
    Auth,
    Other(String),
}

impl MailError {
    pub fn message(&self) -> String {
        match self {
            MailError::Auth => "Gmail refused the app password (wrong, revoked, or app passwords are turned off for this account).".into(),
            MailError::Other(e) => e.clone(),
        }
    }
}

type R<T> = Result<T, MailError>;
type Session = async_imap::Session<TlsStream<TcpStream>>;

fn other(s: impl Into<String>) -> MailError {
    MailError::Other(s.into())
}

/// The server's own words stay out of the message (they are outside text).
fn failed(_: async_imap::error::Error) -> MailError {
    other("the mail server refused or stopped answering")
}

/// `"…"` for a search query: printable text only (UTF-8 allowed), quotes and backslashes escaped.
pub fn quote(s: &str) -> R<String> {
    if s.chars().any(char::is_control) {
        return Err(other("text sent to the mail server may not contain line breaks or other control characters"));
    }
    Ok(format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// Connect (TLS), read the greeting and log in, within a minute.
pub async fn login(servers: &MailServers, email: &str, password: &str) -> R<Session> {
    let (host, port) = &servers.imap;
    let go = async {
        let stream = tls::connect(servers, host, *port).await.map_err(MailError::Other)?;
        let mut client = async_imap::Client::new(stream);
        match client.read_response().await {
            Ok(Some(_)) => {}
            _ => return Err(other("the mail server didn't greet Familiar")),
        }
        client.login(email, password).await.map_err(|(e, _)| match e {
            async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_) | async_imap::error::Error::Validate(_) => MailError::Auth,
            e => failed(e),
        })
    };
    tokio::time::timeout(TIMEOUT, go).await.map_err(|_| other("the mail server didn't answer in time"))?
}

async fn logout(mut s: Session) {
    let _ = tokio::time::timeout(Duration::from_secs(5), s.logout()).await;
}

/// The `\All` and `\Drafts` mailboxes (their names depend on the account's language).
async fn special(s: &mut Session) -> R<(String, String)> {
    let names: Vec<_> = s.list(Some(""), Some("*")).await.map_err(failed)?.try_collect().await.map_err(failed)?;
    let find = |want: &NameAttribute| names.iter().find(|n| n.attributes().contains(want)).map(|n| n.name().to_owned());
    Ok((
        find(&NameAttribute::All).unwrap_or_else(|| "[Gmail]/All Mail".into()),
        find(&NameAttribute::Drafts).unwrap_or_else(|| "[Gmail]/Drafts".into()),
    ))
}

/// A Gmail id as the API writes it (lowercase hex) → the number IMAP uses.
pub fn id_number(id: &str) -> Option<u64> {
    (!id.is_empty() && id.len() <= 16).then(|| u64::from_str_radix(id, 16).ok()).flatten()
}

pub fn id_text(n: u64) -> String {
    format!("{n:x}")
}

/// One message as found: its Gmail id and what it says (headers only for a search). IMAP gives no thread ids
/// through `async-imap`, so `thread_id` is empty here (Connect Google reads whole conversations).
#[derive(Debug, Clone)]
pub struct Found {
    pub id: String,
    pub thread_id: String,
    pub mail: mime::Mail,
}

/// Check the address and app password (log in and out).
pub async fn verify(servers: &MailServers, email: &str, password: &str) -> R<()> {
    logout(login(servers, email, password).await?).await;
    Ok(())
}

async fn headers(s: &mut Session, uids: &[u32]) -> R<Vec<Found>> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let set = uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let fetched: Vec<_> = s.uid_fetch(&set, "(UID X-GM-MSGID BODY.PEEK[HEADER])").await.map_err(failed)?.try_collect().await.map_err(failed)?;
    let mut out: Vec<(u32, Found)> = fetched
        .iter()
        .filter_map(|f| {
            let id = *f.gmail_msg_id()?;
            Some((f.uid?, Found { id: id_text(id), thread_id: String::new(), mail: mime::parse(f.header()?) }))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(out.into_iter().map(|(_, f)| f).collect())
}

/// Search all mail with Gmail's query syntax; the newest `max`, newest first (headers only).
pub async fn search(servers: &MailServers, email: &str, password: &str, query: &str, max: usize) -> R<Vec<Found>> {
    let q = quote(if query.trim().is_empty() { "in:inbox" } else { query })?;
    let mut s = login(servers, email, password).await?;
    let go = async {
        let (all, _) = special(&mut s).await?;
        s.examine(&all).await.map_err(failed)?;
        let mut uids: Vec<u32> = s.uid_search(format!("X-GM-RAW {q}")).await.map_err(failed)?.into_iter().collect();
        uids.sort_unstable_by(|a, b| b.cmp(a));
        uids.truncate(max);
        headers(&mut s, &uids).await
    };
    let r = tokio::time::timeout(TIMEOUT, go).await.map_err(|_| other("the mail server didn't answer in time"))?;
    logout(s).await;
    r
}

/// One message by its Gmail id (`None` when there is no such message).
pub async fn read(servers: &MailServers, email: &str, password: &str, id: &str) -> R<Option<Found>> {
    let n = id_number(id).ok_or_else(|| other("that isn't a message id from gmail_search"))?;
    let mut s = login(servers, email, password).await?;
    let go = async {
        let (all, _) = special(&mut s).await?;
        s.examine(&all).await.map_err(failed)?;
        let Some(uid) = s.uid_search(format!("X-GM-MSGID {n}")).await.map_err(failed)?.into_iter().next() else { return Ok(None) };
        let sized: Vec<_> = s.uid_fetch(uid.to_string(), "(UID RFC822.SIZE)").await.map_err(failed)?.try_collect().await.map_err(failed)?;
        let size = sized.first().and_then(|f| f.size).unwrap_or(0);
        if size > MAX_MESSAGE {
            let mut found = headers(&mut s, &[uid]).await?;
            if let Some(f) = found.first_mut() {
                f.mail.body = format!("(This email is {} MB, too big to read here; open it in Gmail.)", size / (1024 * 1024));
            }
            return Ok(found.into_iter().next());
        }
        let full: Vec<_> = s.uid_fetch(uid.to_string(), "(UID X-GM-MSGID BODY.PEEK[])").await.map_err(failed)?.try_collect().await.map_err(failed)?;
        Ok(full.first().and_then(|f| Some(Found { id: id_text(*f.gmail_msg_id()?), thread_id: String::new(), mail: mime::parse(f.body()?) })))
    };
    let r = tokio::time::timeout(TIMEOUT, go).await.map_err(|_| other("the mail server didn't answer in time"))?;
    logout(s).await;
    r
}

/// Save a draft into Gmail's Drafts.
pub async fn save_draft(servers: &MailServers, email: &str, password: &str, raw: &[u8]) -> R<()> {
    let mut s = login(servers, email, password).await?;
    let go = async {
        let (_, drafts) = special(&mut s).await?;
        s.append(&drafts, Some("(\\Draft \\Seen)"), None, raw).await.map_err(failed)
    };
    let r = tokio::time::timeout(TIMEOUT, go).await.map_err(|_| other("the mail server didn't answer in time"))?;
    logout(s).await;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_sent_can_start_a_command() {
        assert_eq!(quote("from:sam \"quoted\" \\x").unwrap(), "\"from:sam \\\"quoted\\\" \\\\x\"");
        assert!(quote("x\r\nf9 DELETE INBOX").is_err());
        assert!(quote("a\0b").is_err());
        assert_eq!(quote("Jürgen").unwrap(), "\"Jürgen\"");
    }

    #[test]
    fn ids_round_trip() {
        assert_eq!(id_number("11bdfc5cae0c8191"), Some(1278455344230334865));
        assert_eq!(id_text(1278455344230334865), "11bdfc5cae0c8191");
        assert_eq!(id_number("../etc"), None);
        assert_eq!(id_number("12345678901234567"), None);
    }
}
