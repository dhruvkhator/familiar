//! The bench's synthetic integrations (`--bench-shot integrations…`): the real connector catalog (the API's own
//! `presets.json`), four installed connectors (two from the catalog, two of the owner's own, one of them off) and a
//! Telegram channel; a teammate's webhooks, skills, memories and files (stand-in screenshots drawn here). The fake API
//! answers their reads (and making a webhook); nothing here is ever written to a real Familiar.

use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;

use familiar_client::{Artifact, Channel, Connector, ConnectorPreset, Memory, Rule, RuleDecision, SecretNames, Skill, Trigger};
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
    /// A teammate's files, with their bytes (the Files tab).
    pub artifacts: Vec<(Artifact, Vec<u8>)>,
    /// Every rule (the Rules page).
    pub rules: Vec<Rule>,
    now: DateTime<Utc>,
}

impl Parity {
    /// `answers`: the teammate the channel and the per-teammate data belong to; `runs`: two of its runs (the files'
    /// sources: a short one and a long one).
    pub fn new(now: DateTime<Utc>, page: &str, answers: Uuid, runs: (Uuid, Uuid), others: (Uuid, Uuid)) -> Self {
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
        // Ada's files: three screenshots and three documents, from two of her runs.
        let (short_run, long_run) = runs;
        let file = |n: usize, run: Uuid, name: &str, mime: &str, bytes: Vec<u8>, minutes: i64| {
            (
                Artifact { id: pid(6, n), run_id: run, bot_id: answers, name: name.into(), mime: mime.into(), bytes: bytes.len() as u64, created_at: now - Duration::minutes(minutes) },
                bytes,
            )
        };
        let artifacts = vec![
            file(1, short_run, "checkout-annual-toggle.png", "image/png", screenshot(0x7285d5, 3), 31),
            file(2, short_run, "pricing-page-mobile.png", "image/png", screenshot(0x4fb98a, 5), 32),
            file(3, short_run, "launch-status.md", "text/markdown", b"# Launch status\n\n- Pricing page: fixed\n- Checkout test: failing on the annual toggle\n".to_vec(), 33),
            file(4, long_run, "weekly-active-teams.png", "image/png", screenshot(0xeda84b, 7), 121),
            file(5, long_run, "launch-plan.pdf", "application/pdf", b"%PDF-1.4\n% a stand-in for the bench\n".repeat(900), 122),
            file(6, long_run, "weekly_active_teams.sql", "application/sql", b"select week, count(distinct team_id) from events group by week;\n".to_vec(), 123),
        ];
        // Rules: three for every teammate, a few of the teammates' own (one that lets Pip run anything).
        let (milo, pip) = others;
        let rule = |n: usize, bot: Option<Uuid>, pattern: &str, decision: RuleDecision, note: Option<&str>, days: i64| Rule {
            id: pid(7, n),
            bot_id: bot,
            pattern: pattern.into(),
            decision,
            note: note.map(Into::into),
            created_at: now - Duration::days(days),
        };
        let rules = vec![
            rule(1, None, "Bash(git status*)", RuleDecision::Allow, Some("So it can check where things stand while it works"), 30),
            rule(2, None, "WebFetch", RuleDecision::Review, None, 28),
            rule(3, None, "mcp__github__create_pull_request", RuleDecision::Ask, Some("I want to see every pull request first"), 20),
            rule(4, Some(answers), "Edit(docs/*)", RuleDecision::Allow, None, 12),
            rule(5, Some(answers), "mcp__github__list_issues", RuleDecision::Allow, None, 6),
            rule(6, Some(milo), "Bash(npm run deploy*)", RuleDecision::Deny, Some("Deploys are mine"), 9),
            rule(7, Some(pip), "Bash", RuleDecision::Allow, None, 2),
        ];
        Self { presets, connectors, channels, triggers, skills, memories, artifacts, rules, now }
    }

    /// The reads (`segs` is the path after `/api`).
    pub fn get(&self, segs: &[&str], q: &HashMap<String, String>) -> Option<Value> {
        Some(match segs {
            ["connectors"] => json!(self.connectors),
            ["connectors", "presets"] => json!(self.presets),
            ["channels"] => json!(self.channels),
            // The API's filter: `all` everything, `bot_id` that teammate's own, else the ones for every teammate.
            ["rules"] => {
                let bot = q.get("bot_id").and_then(|b| b.parse::<Uuid>().ok());
                json!(self.rules.iter().filter(|r| q.contains_key("all") || r.bot_id == bot).collect::<Vec<_>>())
            }
            // Every teammate may use GitHub and the docs server.
            ["bots", _, "connectors"] => json!(self.connectors.iter().filter(|c| c.name == "github" || c.name == "acme-docs").collect::<Vec<_>>()),
            ["bots", _, "triggers"] => json!(self.triggers),
            ["bots", _, "skills"] => json!(self.skills),
            ["bots", _, "memories"] => json!(self.memories),
            ["bots", _, "artifacts"] => json!(self.artifacts.iter().map(|(a, _)| a).collect::<Vec<_>>()),
            _ => return None,
        })
    }

    /// A file's type and bytes.
    pub fn download(&self, id: Uuid) -> Option<(String, Vec<u8>)> {
        self.artifacts.iter().find(|(a, _)| a.id == id).map(|(a, b)| (a.mime.clone(), b.clone()))
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

/// A stand-in screenshot (PNG): a page with a coloured header, a sidebar and `rows` content bars.
fn screenshot(accent: u32, rows: u32) -> Vec<u8> {
    let (w, h) = (960u32, 600u32);
    let rgb = |c: u32| image::Rgba([(c >> 16) as u8, (c >> 8) as u8, c as u8, 255]);
    let mut pic = image::RgbaImage::from_pixel(w, h, rgb(0xf7f6f3));
    let mut fill = |x0: u32, y0: u32, x1: u32, y1: u32, c: image::Rgba<u8>| {
        for y in y0..y1.min(h) {
            for x in x0..x1.min(w) {
                pic.put_pixel(x, y, c);
            }
        }
    };
    fill(0, 0, w, 64, rgb(accent));
    fill(0, 64, 200, h, rgb(0xebe9e4));
    for i in 0..rows {
        let y = 100 + i * 70;
        fill(240, y, 240 + 520 - (i * 37) % 200, y + 18, rgb(0xd4d1ca));
        fill(240, y + 28, 240 + 340 - (i * 53) % 160, y + 40, rgb(0xe3e0da));
    }
    fill(780, 100, 920, 140, rgb(accent));
    let mut out = Vec::new();
    let _ = pic.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png);
    out
}
