//! The bench's synthetic integrations (`--bench-shot integrations…`): the real connector catalog (the API's own
//! `presets.json`), four installed connectors (two from the catalog, two of the owner's own, one of them off) and a
//! Telegram channel. The fake API answers their reads; nothing here is ever written to a real Familiar.

use chrono::{DateTime, Duration, Utc};
use familiar_client::{Channel, Connector, ConnectorPreset, SecretNames};
use serde_json::{Value, json};
use uuid::Uuid;

/// The catalog the API serves.
const PRESETS: &str = include_str!("../../../crates/familiar-server/src/presets.json");

pub fn pid(kind: u128, n: usize) -> Uuid {
    Uuid::from_u128(0xba11_0000_0000_4000_8000_0000_0000_0000 | (kind << 32) | n as u128)
}

pub struct Parity {
    pub presets: Vec<ConnectorPreset>,
    pub connectors: Vec<Connector>,
    /// Telegram: paired (most shots), waiting to pair (`integrations-telegram-pair`) or not set up
    /// (`integrations-telegram`).
    pub channels: Vec<Channel>,
}

impl Parity {
    pub fn new(now: DateTime<Utc>, page: &str, answers: Uuid) -> Self {
        let presets: Vec<ConnectorPreset> = serde_json::from_str(PRESETS).unwrap_or_default();
        let secrets = |env: &[&str], headers: &[&str]| SecretNames {
            env: env.iter().map(|s| (*s).to_owned()).collect(),
            headers: headers.iter().map(|s| (*s).to_owned()).collect(),
        };
        let connectors = vec![
            Connector {
                id: pid(1, 1),
                name: "acme-docs".into(),
                preset: Some("custom".into()),
                transport: "http".into(),
                url: Some("https://mcp.acme-internal.example.com/docs".into()),
                secret_names: secrets(&[], &["Authorization"]),
                has_secrets: true,
                enabled: true,
                created_at: Some(now - Duration::days(6)),
                ..Default::default()
            },
            Connector {
                id: pid(1, 2),
                name: "github".into(),
                preset: Some("github".into()),
                transport: "stdio".into(),
                command: Some("npx".into()),
                args: Some(vec!["-y".into(), "@modelcontextprotocol/server-github".into()]),
                secret_names: secrets(&["GITHUB_PERSONAL_ACCESS_TOKEN"], &[]),
                has_secrets: true,
                enabled: true,
                created_at: Some(now - Duration::days(12)),
                ..Default::default()
            },
            Connector {
                id: pid(1, 3),
                name: "notes".into(),
                preset: Some("custom".into()),
                transport: "stdio".into(),
                command: Some("uvx".into()),
                args: Some(vec!["notes-mcp".into(), "--folder".into(), r"C:\Users\sam\Documents\Team notes".into()]),
                secret_names: secrets(&["NOTES_API_KEY"], &[]),
                has_secrets: true,
                enabled: true,
                created_at: Some(now - Duration::days(3)),
                ..Default::default()
            },
            Connector {
                id: pid(1, 4),
                name: "notion".into(),
                preset: Some("notion".into()),
                transport: "stdio".into(),
                command: Some("npx".into()),
                args: Some(vec!["-y".into(), "@notionhq/notion-mcp-server".into()]),
                secret_names: secrets(&["NOTION_TOKEN"], &[]),
                has_secrets: true,
                enabled: false,
                created_at: Some(now - Duration::days(20)),
                ..Default::default()
            },
        ];
        let channel = |bound: bool| Channel {
            id: pid(2, 1),
            kind: "telegram".into(),
            bound,
            pair_code: (!bound).then(|| "K7QM2XWD".into()),
            default_bot_id: Some(answers),
            enabled: true,
        };
        let channels = match page {
            "integrations-telegram" => Vec::new(),
            "integrations-telegram-pair" => vec![channel(false)],
            _ => vec![channel(true)],
        };
        Self { presets, connectors, channels }
    }

    /// The reads (`segs` is the path after `/api`).
    pub fn get(&self, segs: &[&str]) -> Option<Value> {
        Some(match segs {
            ["connectors"] => json!(self.connectors),
            ["connectors", "presets"] => json!(self.presets),
            ["channels"] => json!(self.channels),
            _ => return None,
        })
    }
}
