//! Sending one email through Gmail's SMTP server with an app password, on the maintained `lettre` crate (MIT):
//! implicit TLS on 465 only (`Tls::Wrapper`: no plaintext, so no STARTTLS to strip), rustls with the platform
//! verifier, `AUTH PLAIN`, the account itself as the envelope sender, To, Cc and Bcc as envelope recipients (Bcc is in
//! no header). The message was composed by [`super::mime::compose`]; `lettre` dot-stuffs it. Gmail files what it sends
//! under Sent by itself.

use std::time::Duration;

use lettre::address::Envelope;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Certificate, Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

use super::imap::MailError;
use super::{MailServers, mime};

fn other(s: impl Into<String>) -> MailError {
    MailError::Other(s.into())
}

/// Send `raw` from `from` to every envelope recipient.
pub async fn send(servers: &MailServers, from: &str, password: &str, recipients: &[String], raw: &[u8]) -> Result<(), MailError> {
    let from: lettre::Address = mime::plain_address(from)
        .and_then(|a| a.parse().ok())
        .ok_or_else(|| other("the account's address isn't a plain email address"))?;
    let to: Vec<lettre::Address> = recipients
        .iter()
        .map(|r| mime::plain_address(r).and_then(|a| a.parse().ok()).ok_or_else(|| other("a recipient isn't a plain email address")))
        .collect::<Result<_, _>>()?;
    let envelope = Envelope::new(Some(from.clone()), to).map_err(|_| other("nobody to send it to"))?;
    let (host, port) = &servers.smtp;
    let mut tls = TlsParameters::builder(host.clone());
    if let Some(der) = &servers.extra_root {
        tls = tls.add_root_certificate(Certificate::from_der(der.clone()).map_err(|_| other("the extra TLS root isn't a certificate"))?);
    }
    let tls = tls.build_rustls().map_err(|_| other("TLS setup failed"))?;
    let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host.clone())
        .port(*port)
        .tls(Tls::Wrapper(tls))
        .credentials(Credentials::new(from.to_string(), password.to_owned()))
        .authentication(vec![Mechanism::Plain])
        .timeout(Some(Duration::from_secs(60)))
        .build();
    match transport.send_raw(&envelope, raw).await {
        Ok(_) => Ok(()),
        // 535: the server refused the app password.
        Err(e) if e.status().is_some_and(|c| c.to_string() == "535") => Err(MailError::Auth),
        Err(e) if e.is_permanent() || e.is_transient() => Err(other(format!(
            "the mail server refused it ({})",
            e.status().map(|c| c.to_string()).unwrap_or_else(|| "no code".into())
        ))),
        Err(_) => Err(other("could not reach the mail server securely")),
    }
}
