//! Teammates: the live list from the local Familiar API (the spike's sign-in path, now via `/api/overview` so each
//! teammate carries its derived status), plus sample data for the gallery and the "Today" mock.

use familiar_ui::mascot::{Accessory, Avatar, MascotState, resolve_avatar};
use gpui::SharedString;
use serde::Deserialize;

const API: &str = "http://127.0.0.1:47080/api";

#[derive(Debug, Clone)]
pub struct Teammate {
    pub id: SharedString,
    pub name: SharedString,
    pub avatar: Avatar,
    pub state: MascotState,
    pub model: Option<SharedString>,
}

#[derive(Debug, Clone, Deserialize)]
struct BotRow {
    id: String,
    name: String,
    #[serde(default)]
    avatar: Option<serde_json::Value>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    last_run_at: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Overview {
    bots: Vec<BotRow>,
    #[serde(default)]
    pending_approvals: serde_json::Value,
    #[serde(default)]
    devices: Vec<serde_json::Value>,
}

/// What the shell shows.
#[derive(Debug, Clone)]
pub struct Live {
    pub teammates: Vec<Teammate>,
    pub pending: usize,
    pub pc_online: bool,
}

/// Sign in as the spike did (owner password from `~/.familiar/owner_password.txt` or `FAMILIAR_PASSWORD`) and read
/// the overview.
pub async fn fetch_live() -> anyhow::Result<Live> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default();
    let password = std::env::var("FAMILIAR_PASSWORD").or_else(|_| {
        std::fs::read_to_string(std::path::Path::new(&home).join(".familiar").join("owner_password.txt"))
    })?;
    let email = std::env::var("FAMILIAR_EMAIL").unwrap_or_else(|_| "owner@familiar.local".into());
    let http = reqwest::Client::new();
    let login: serde_json::Value = http
        .post(format!("{API}/auth/login"))
        .json(&serde_json::json!({ "email": email, "password": password.trim() }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let token = login["token"].as_str().ok_or_else(|| anyhow::anyhow!("no token in login response"))?;
    let overview: Overview =
        http.get(format!("{API}/overview")).bearer_auth(token).send().await?.error_for_status()?.json().await?;
    let pending = match &overview.pending_approvals {
        serde_json::Value::Number(n) => n.as_u64().unwrap_or(0) as usize,
        serde_json::Value::Array(a) => a.len(),
        _ => 0,
    };
    let pc_online = overview.devices.iter().any(|d| d["online"].as_bool() == Some(true));
    let teammates = overview
        .bots
        .into_iter()
        .map(|b| {
            // The web's `stateOf` (minus per-bot pending approvals, which need the approvals list).
            let state = if b.paused || b.status.as_deref() == Some("paused") {
                MascotState::Paused
            } else if b.status.as_deref() == Some("running") {
                MascotState::Working
            } else if b.last_run_at.as_deref().is_some_and(recent) {
                MascotState::Done
            } else {
                MascotState::Idle
            };
            Teammate {
                avatar: resolve_avatar(&b.id, b.avatar.as_ref()),
                id: b.id.into(),
                name: b.name.into(),
                state,
                model: b.model.map(Into::into),
            }
        })
        .collect();
    Ok(Live { teammates, pending, pc_online })
}

/// "Finished in the last ten minutes" — RFC 3339 timestamps compared as UTC seconds without a date crate.
fn recent(ts: &str) -> bool {
    let Some(then) = parse_rfc3339_secs(ts) else {
        return false;
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64);
    now.is_ok_and(|now| now - then < 10 * 60)
}

fn parse_rfc3339_secs(ts: &str) -> Option<i64> {
    let (date, rest) = ts.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let hms: Vec<i64> = rest.get(..8)?.split(':').filter_map(|p| p.parse().ok()).collect();
    let [h, mi, s] = hms[..] else { return None };
    let tz = &rest[8..];
    let tz = tz.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = if tz.is_empty() || tz.starts_with('Z') {
        0
    } else {
        let sign = if tz.starts_with('-') { -1 } else { 1 };
        let mut p = tz[1..].split(':').filter_map(|p| p.parse::<i64>().ok());
        sign * (p.next().unwrap_or(0) * 3600 + p.next().unwrap_or(0) * 60)
    };
    // Days from civil (Howard Hinnant).
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + s - offset)
}

/// Sample teammates for the gallery and the offline "Today" mock: one in every state, a spread of shapes and
/// accessories.
pub fn sample_teammates() -> Vec<Teammate> {
    let mk = |id: &str, name: &str, shape, color, eyes, mouth, accessory, state| Teammate {
        id: id.to_owned().into(),
        name: name.to_owned().into(),
        avatar: Avatar { shape, color, eyes, mouth, accessory },
        state,
        model: Some("sonnet".into()),
    };
    vec![
        mk("s-ada", "Ada", 0, 0x7285d5, 1, 0, Accessory::Headphones, MascotState::Working),
        mk("s-milo", "Milo", 2, 0xeda84b, 0, 2, Accessory::None, MascotState::NeedsYou),
        mk("s-juniper", "Juniper", 1, 0x4fb98a, 3, 3, Accessory::Bow, MascotState::Done),
        mk("s-pip", "Pip", 4, 0xa283d8, 2, 1, Accessory::Antenna, MascotState::Idle),
        mk("s-otto", "Otto", 3, 0x4fa9cf, 4, 0, Accessory::Glasses, MascotState::Paused),
        mk("s-rue", "Rue", 0, 0xe58fa4, 0, 3, Accessory::Crown, MascotState::Idle),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_parses_with_offsets_and_fractions() {
        assert_eq!(parse_rfc3339_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_secs("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(parse_rfc3339_secs("2026-10-02T10:00:00.123456+00:00"), Some(1_790_935_200));
    }
}
