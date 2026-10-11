//! TLS for IMAP, the same on Windows and macOS: rustls (ring provider) with `rustls-platform-verifier`, so the
//! certificate is checked by the operating system's own verifier (Windows' certificate store, macOS' Security
//! framework) as `lettre` does for SMTP; TLS 1.2 or newer; host name always checked. Plain connections are never made.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use super::MailServers;

/// The first certificate of a PEM file, as DER.
pub fn pem_to_der(pem: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(pem).ok()?;
    let start = text.find("-----BEGIN CERTIFICATE-----")? + "-----BEGIN CERTIFICATE-----".len();
    let end = text[start..].find("-----END CERTIFICATE-----")? + start;
    let b64: String = text[start..end].chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

fn connector(servers: &MailServers) -> Result<TlsConnector, String> {
    // Both ring and aws-lc-rs are compiled in (other crates ask for them), so name the provider explicitly.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let extra: Vec<CertificateDer<'static>> = servers.extra_root.iter().map(|d| CertificateDer::from(d.clone())).collect();
    let verifier = rustls_platform_verifier::Verifier::new_with_extra_roots(extra, provider.clone()).map_err(|_| "TLS setup failed".to_owned())?;
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| "TLS setup failed".to_owned())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// Connect to `host:port` and do the TLS handshake (implicit TLS), within 20 s.
pub async fn connect(servers: &MailServers, host: &str, port: u16) -> Result<TlsStream<TcpStream>, String> {
    let connector = connector(servers)?;
    let name = ServerName::try_from(host.to_owned()).map_err(|_| format!("{host} isn't a server name"))?;
    let go = async {
        let tcp = TcpStream::connect((host, port)).await.map_err(|_| format!("could not connect to {host}:{port}"))?;
        connector.connect(name, tcp).await.map_err(|_| format!("the secure connection to {host} failed (TLS)"))
    };
    tokio::time::timeout(Duration::from_secs(20), go).await.map_err(|_| format!("{host} didn't answer in time"))?
}
