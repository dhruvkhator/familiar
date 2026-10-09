//! The bench's synthetic integrations (`--bench-shot integrations…`): the real connector catalog (the API's own
//! `presets.json`) and four installed connectors (two from the catalog, two of the owner's own, one of them off). The
//! fake API answers their reads; nothing here is ever written to a real Familiar.

use chrono::{DateTime, Duration, Utc};
use familiar_client::{Connector, ConnectorPreset, SecretNames};
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
}

impl Parity {
    pub fn new(now: DateTime<Utc>) -> Self {
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
        Self { presets, connectors }
    }

    /// The reads (`segs` is the path after `/api`).
    pub fn get(&self, segs: &[&str]) -> Option<Value> {
        Some(match segs {
            ["connectors"] => json!(self.connectors),
            ["connectors", "presets"] => json!(self.presets),
            _ => return None,
        })
    }
}
