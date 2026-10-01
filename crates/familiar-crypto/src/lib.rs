//! Secrets at rest: AES-256-GCM with a key shared by familiar-server and the daemon (`FAMILIAR_SECRET_KEY`,
//! 32 bytes, base64). Ciphertext format: `v1:` + base64(nonce ‖ ciphertext+tag).

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

pub struct SecretBox(Aes256Gcm);

impl SecretBox {
    /// From the base64 key in `FAMILIAR_SECRET_KEY` (or config). Generate one with [`SecretBox::generate_key`].
    pub fn from_base64(key: &str) -> Result<Self> {
        let bytes = STANDARD.decode(key.trim()).context("secret key is not base64")?;
        if bytes.len() != 32 {
            bail!("secret key must be 32 bytes, got {}", bytes.len());
        }
        Ok(Self(Aes256Gcm::new_from_slice(&bytes).map_err(|e| anyhow!("{e}"))?))
    }

    pub fn generate_key() -> String {
        STANDARD.encode(rand::random::<[u8; 32]>())
    }

    pub fn encrypt(&self, plain: &str) -> Result<String> {
        let nonce: [u8; 12] = rand::random();
        let ct = self.0.encrypt(&Nonce::from(nonce), plain.as_bytes()).map_err(|e| anyhow!("encrypt: {e}"))?;
        let mut out = nonce.to_vec();
        out.extend(ct);
        Ok(format!("v1:{}", STANDARD.encode(out)))
    }

    pub fn decrypt(&self, sealed: &str) -> Result<String> {
        let raw = STANDARD
            .decode(sealed.strip_prefix("v1:").context("unknown secret format")?)
            .context("secret is not base64")?;
        if raw.len() < 12 {
            bail!("secret too short");
        }
        let (nonce, ct) = raw.split_at(12);
        let plain = self.0.decrypt(&Nonce::try_from(nonce).map_err(|_| anyhow!("bad nonce"))?, ct).map_err(|_| anyhow!("wrong key or corrupted secret"))?;
        Ok(String::from_utf8(plain)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let b = SecretBox::from_base64(&SecretBox::generate_key()).unwrap();
        let sealed = b.encrypt(r#"{"env":{"GITHUB_TOKEN":"x"}}"#).unwrap();
        assert_eq!(b.decrypt(&sealed).unwrap(), r#"{"env":{"GITHUB_TOKEN":"x"}}"#);
        let other = SecretBox::from_base64(&SecretBox::generate_key()).unwrap();
        assert!(other.decrypt(&sealed).is_err());
    }
}
