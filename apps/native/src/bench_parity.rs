//! The bench's synthetic integrations (`--bench-shot integrations…`): the real connector catalog (the API's own
//! `presets.json`), four installed connectors (two from the catalog, two of the owner's own, one of them off) and a
//! Telegram channel; a teammate's webhooks. The fake API answers their reads (and making a webhook); nothing here is
//! ever written to a real Familiar.

use chrono::{DateTime, Duration, Utc};
use familiar_client::{Channel, Connector, ConnectorPreset, Memory, SecretNames, Skill, Trigger};
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
    /// A teammate's webhooks (the same for each).
    pub triggers: Vec<Trigger>,
    /// A teammate's skills and memories (the Learned tab).
    pub skills: Vec<Skill>,
    pub memories: Vec<Memory>,
    now: DateTime<Utc>,
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
        let trigger = |n: usize, name: &str, prompt: &str, kind: &str, enabled: bool, fired: Option<i64>| Trigger {
            id: pid(3, n),
            bot_id: answers,
            name: name.into(),
            prompt: prompt.into(),
            kind: kind.into(),
            enabled,
            last_fired_at: fired.map(|m| now - Duration::minutes(m)),
            created_at: Some(now - Duration::days(5)),
            url: None,
        };
        let triggers = vec![
            trigger(1, "Build failed", "A build failed. Read the details below, find the cause and tell me what to fix.", "scheduled", true, Some(130)),
            trigger(2, "New sign-up form", "Someone filled in the sign-up form. Look up their company and add it to the CRM with a fit score.", "proactive", true, None),
            trigger(3, "Uptime alert", "The site is down. Check the status page and the last deploy, then tell me what you found.", "scheduled", false, Some(60 * 24 * 9)),
        ];
        let skill = |n: usize, name: &str, description: &str, body: &str, days: i64| Skill {
            id: pid(4, n),
            bot_id: answers,
            name: name.into(),
            description: Some(description.into()),
            body: body.into(),
            updated_at: now - Duration::days(days),
        };
        let skills = vec![
            skill(
                1,
                "launch-checklist",
                "Before any launch: walk the release checklist and report what is still open.",
                "---\nname: launch-checklist\ndescription: Before any launch: walk the release checklist and report what is still open.\n---\n\n# Launch checklist\n\n1. Open `docs/launch.md` and read the checklist for this release.\n2. For each item, check the linked issue or pull request:\n   - merged and deployed: tick it\n   - open: note who owns it and the last update\n3. Run `npm run test:e2e -- --grep checkout` and note any failure.\n4. Post a short summary in the chat:\n   - what is done\n   - what is open, with owners\n   - anything that blocks the launch\n\nNever change the checklist file itself; ask first.\n",
                2,
            ),
            skill(
                2,
                "weekly-metrics",
                "Every Monday, or when asked for the numbers: pull active teams, sign-ups and churn and compare with last week.",
                "---\nname: weekly-metrics\ndescription: Every Monday, or when asked for the numbers.\n---\n\n# Weekly metrics\n\nQuery the read replica with the saved `weekly_active_teams.sql`, then compare with last week's report.\n",
                9,
            ),
            skill(3, "expense-report", "Filing an expense in the finance portal, step by step.", "# Expense report\n\n1. Sign in to the portal.\n2. New claim, attach the receipt.\n", 21),
        ];
        let memory = |n: usize, content: &str, source: &str, status: &str, days: i64| Memory {
            id: pid(5, n),
            bot_id: answers,
            content: content.into(),
            source: source.into(),
            status: Some(status.into()),
            created_at: now - Duration::days(days),
        };
        let memories = vec![
            memory(1, "Launches happen on Thursdays; the checklist lives in docs/launch.md.", "bot", "active", 6),
            memory(2, "Sam prefers short answers with the open items first.", "user", "active", 12),
        ];
        Self { presets, connectors, channels, triggers, skills, memories, now }
    }

    /// The reads (`segs` is the path after `/api`).
    pub fn get(&self, segs: &[&str]) -> Option<Value> {
        Some(match segs {
            ["connectors"] => json!(self.connectors),
            ["connectors", "presets"] => json!(self.presets),
            ["channels"] => json!(self.channels),
            // Every teammate may use GitHub and the docs server.
            ["bots", _, "connectors"] => json!(self.connectors.iter().filter(|c| c.name == "github" || c.name == "acme-docs").collect::<Vec<_>>()),
            ["bots", _, "triggers"] => json!(self.triggers),
            ["bots", _, "skills"] => json!(self.skills),
            ["bots", _, "memories"] => json!(self.memories),
            _ => return None,
        })
    }

    /// The writes the screenshots use: making a webhook answers with its address (shown once).
    pub fn write(&mut self, method: &str, segs: &[&str], body: &str) -> Option<Value> {
        Some(match (method, segs) {
            ("POST", ["bots", b, "triggers"]) => {
                let v: Value = serde_json::from_str(body).ok()?;
                let t = Trigger {
                    id: pid(3, 10 + self.triggers.len()),
                    bot_id: b.parse().ok()?,
                    name: v["name"].as_str().unwrap_or_default().into(),
                    prompt: v["prompt"].as_str().unwrap_or_default().into(),
                    kind: v["kind"].as_str().unwrap_or("scheduled").into(),
                    enabled: true,
                    created_at: Some(self.now),
                    ..Default::default()
                };
                self.triggers.push(t.clone());
                json!(Trigger { url: Some("http://localhost:7766/hooks/q3Vh9wXn2LkT8rJ5mZcY1pFbA6sDeG4u".into()), ..t })
            }
            _ => return None,
        })
    }
}
