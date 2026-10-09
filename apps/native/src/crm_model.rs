//! The CRM's plain logic, kept apart from the views so it can be tested: stage and activity words, money and dates,
//! tags, what a change touched, a CSV file's shape before it is imported, the hints a webhook address gets while it is
//! typed, and the stage menu's keyboard steps.

use chrono::{DateTime, Local, NaiveDate, TimeZone as _, Utc};
use familiar_client::{ActivityKind, CrmChange, DealStage};
use familiar_ui::theme::Tone;
use serde_json::Value;

/// Every stage, in board order.
pub const STAGES: [DealStage; 8] = [
    DealStage::New,
    DealStage::Researching,
    DealStage::Contacted,
    DealStage::Replied,
    DealStage::Meeting,
    DealStage::Proposal,
    DealStage::Won,
    DealStage::Lost,
];

pub fn stage_label(s: DealStage) -> &'static str {
    match s {
        DealStage::New => "New",
        DealStage::Researching => "Researching",
        DealStage::Contacted => "Contacted",
        DealStage::Replied => "Replied",
        DealStage::Meeting => "Meeting",
        DealStage::Proposal => "Proposal",
        DealStage::Won => "Won",
        DealStage::Lost => "Lost",
        DealStage::Unknown => "Other",
    }
}

/// Calm by default: the stages where something good is happening are accented, won is green, lost is grey.
pub fn stage_tone(s: DealStage) -> Tone {
    match s {
        DealStage::Replied | DealStage::Meeting | DealStage::Proposal => Tone::Accent,
        DealStage::Won => Tone::Ok,
        _ => Tone::Muted,
    }
}

pub fn stage_index(s: DealStage) -> Option<usize> {
    STAGES.iter().position(|x| *x == s)
}

pub fn kind_label(k: ActivityKind) -> &'static str {
    match k {
        ActivityKind::Note => "Note",
        ActivityKind::Research => "Research",
        ActivityKind::EmailSent => "Email sent",
        ActivityKind::EmailReceived => "Email received",
        ActivityKind::DmSent => "Message sent",
        ActivityKind::DmReceived => "Message received",
        ActivityKind::Post => "Post",
        ActivityKind::Call => "Call",
        ActivityKind::Meeting => "Meeting",
        ActivityKind::StageChange => "Stage change",
        ActivityKind::Unknown => "Activity",
    }
}

pub fn kind_tone(k: ActivityKind) -> Tone {
    match k {
        ActivityKind::EmailReceived | ActivityKind::DmReceived => Tone::Ok,
        ActivityKind::EmailSent | ActivityKind::DmSent | ActivityKind::Post => Tone::Accent,
        ActivityKind::Meeting | ActivityKind::Call => Tone::Warn,
        _ => Tone::Muted,
    }
}

/// A fit score's colour: strong leads read green, weak ones grey.
pub fn fit_tone(score: i32) -> Tone {
    match score {
        70.. => Tone::Ok,
        40..=69 => Tone::Accent,
        _ => Tone::Muted,
    }
}

/// "1,204".
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn currency_sign(currency: &str) -> Option<&'static str> {
    match currency.to_ascii_uppercase().as_str() {
        "" | "USD" => Some("$"),
        "EUR" => Some("€"),
        "GBP" => Some("£"),
        "JPY" => Some("¥"),
        "INR" => Some("₹"),
        _ => None,
    }
}

/// A deal's value: "$12,500", "$12,500.50", "CHF 900".
pub fn money(cents: i64, currency: &str) -> String {
    let neg = cents < 0;
    let c = cents.unsigned_abs();
    let whole = thousands(c / 100);
    let amount = if c % 100 == 0 { whole } else { format!("{whole}.{:02}", c % 100) };
    let amount = match currency_sign(currency) {
        Some(sign) => format!("{sign}{amount}"),
        None => format!("{} {amount}", currency.to_ascii_uppercase()),
    };
    if neg { format!("-{amount}") } else { amount }
}

/// A column total, short: "$950", "$12.5k", "$1.2M".
pub fn money_short(cents: i64, currency: &str) -> String {
    let units = cents.max(0) as f64 / 100.0;
    let (n, suffix) = if units >= 1_000_000.0 {
        (units / 1_000_000.0, "M")
    } else if units >= 1_000.0 {
        (units / 1_000.0, "k")
    } else {
        return money((units.round() as i64) * 100, currency);
    };
    let n = if n >= 100.0 { format!("{n:.0}") } else { format!("{:.1}", n).trim_end_matches(".0").to_owned() };
    match currency_sign(currency) {
        Some(sign) => format!("{sign}{n}{suffix}"),
        None => format!("{} {n}{suffix}", currency.to_ascii_uppercase()),
    }
}

/// What an owner types as a deal's value: "12500", "12,500.50", "$12.5k", "2m". `None` for anything else; an empty
/// text is `Some(None)`: clear it.
pub fn parse_money(s: &str) -> Option<Option<i64>> {
    let t: String = s.trim().chars().filter(|c| !matches!(c, ',' | ' ' | '$' | '€' | '£' | '¥' | '₹')).collect();
    if t.is_empty() {
        return Some(None);
    }
    let (num, mult) = match t.char_indices().last() {
        Some((i, 'k' | 'K')) => (&t[..i], 1_000.0),
        Some((i, 'm' | 'M')) => (&t[..i], 1_000_000.0),
        _ => (t.as_str(), 1.0),
    };
    let v: f64 = num.parse().ok()?;
    if !v.is_finite() || v < 0.0 || v * mult > 1e13 {
        return None;
    }
    Some(Some((v * mult * 100.0).round() as i64))
}

/// A date the owner types for a deal's next step: `2026-10-20`, `today`, `tomorrow`, `in 3 days`, `+3d`. It lands at
/// 9:00 local time. Empty is `Some(None)` (clear it).
pub fn parse_due(s: &str, today: NaiveDate) -> Option<Option<DateTime<Utc>>> {
    let t = s.trim().to_lowercase();
    if t.is_empty() {
        return Some(None);
    }
    let days = |n: &str| n.trim().parse::<i64>().ok().filter(|n| (0..=3650).contains(n));
    let date = match t.as_str() {
        "today" => Some(today),
        "tomorrow" => today.succ_opt(),
        _ => {
            let rel = t
                .strip_prefix("in ")
                .and_then(|r| r.strip_suffix(" days").or_else(|| r.strip_suffix(" day")))
                .and_then(days)
                .or_else(|| t.strip_prefix('+').and_then(|r| r.strip_suffix('d')).and_then(days));
            match rel {
                Some(n) => today.checked_add_days(chrono::Days::new(n as u64)),
                None => NaiveDate::parse_from_str(&t, "%Y-%m-%d").ok(),
            }
        }
    }?;
    let at = Local.from_local_datetime(&date.and_hms_opt(9, 0, 0)?).earliest()?;
    Some(Some(at.with_timezone(&Utc)))
}

/// A next step's date in words, relative to `now`: "today", "tomorrow", "in 3 days", "2 days late", "Oct 20".
pub fn due_words(t: DateTime<Utc>, now: DateTime<Utc>) -> (String, bool) {
    let (d, today) = (t.with_timezone(&Local).date_naive(), now.with_timezone(&Local).date_naive());
    let days = (d - today).num_days();
    let words = match days {
        0 => "today".to_owned(),
        1 => "tomorrow".to_owned(),
        -1 => "yesterday".to_owned(),
        2..=13 => format!("in {days} days"),
        i64::MIN..=-2 => format!("{} days late", -days),
        _ => t.with_timezone(&Local).format("%b %-d").to_string(),
    };
    (words, days < 0)
}

/// Tags as the owner types them: comma separated, trimmed, without repeats (any case), at most 20.
pub fn parse_tags(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(t)) && out.len() < 20 {
            out.push(t.chars().take(40).collect());
        }
    }
    out
}

/// Up to two letters for a monogram: "Acme Robotics" → "AR", "sam" → "S".
pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

/// A record field's name in words, for "updated fit score and tags".
fn field_words(key: &str) -> Option<&'static str> {
    Some(match key {
        "name" => "name",
        "title" => "title",
        "domain" => "domain",
        "website" => "website",
        "industry" => "industry",
        "size" => "size",
        "location" => "location",
        "description" => "description",
        "fit_score" => "fit score",
        "fit_reason" => "fit reason",
        "tags" => "tags",
        "source_urls" => "sources",
        "custom" => "custom fields",
        "company_id" => "company",
        "contact_id" => "contact",
        "email" => "email",
        "linkedin_url" => "LinkedIn",
        "x_handle" => "X handle",
        "notes" => "notes",
        "do_not_contact" => "do-not-contact",
        "dnc_reason" => "do-not-contact reason",
        "stage" => "stage",
        "value_cents" => "value",
        "currency" => "currency",
        "next_step" => "next step",
        "next_step_at" => "next step date",
        _ => return None,
    })
}

/// The fields a change touched, in words (bookkeeping columns left out).
pub fn changed_fields(c: &CrmChange) -> Vec<&'static str> {
    let empty = serde_json::Map::new();
    let before = c.before.as_ref().and_then(Value::as_object).unwrap_or(&empty);
    let after = c.after.as_ref().and_then(Value::as_object).unwrap_or(&empty);
    let mut keys: Vec<&String> = before.keys().chain(after.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut out = Vec::new();
    for k in keys {
        let (b, a) = (before.get(k).unwrap_or(&Value::Null), after.get(k).unwrap_or(&Value::Null));
        if b != a
            && let Some(w) = field_words(k)
            && !out.contains(&w)
        {
            out.push(w);
        }
    }
    out
}

/// "a", "a and b", "a, b and c".
pub fn join_and(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What a change did, in words (without who): "added it", "changed fit score and tags", "deleted it".
pub fn change_words(c: &CrmChange) -> String {
    let deleted = |v: &Option<Value>| v.as_ref().is_some_and(|v| !v["deleted_at"].is_null());
    match c.op.as_str() {
        "create" => "added it".to_owned(),
        "delete" => "deleted it".to_owned(),
        "undo" if deleted(&c.before) && !deleted(&c.after) => "brought it back".to_owned(),
        "undo" => "undid a change".to_owned(),
        _ => {
            if !deleted(&c.before) && deleted(&c.after) {
                return "deleted it".to_owned();
            }
            let dnc = |v: &Option<Value>| v.as_ref().map(|v| v["do_not_contact"] == Value::Bool(true));
            match (dnc(&c.before), dnc(&c.after)) {
                (Some(false) | None, Some(true)) if c.after.as_ref().is_some_and(|a| a.get("do_not_contact").is_some()) => {
                    return "marked them do-not-contact".to_owned();
                }
                (Some(true), Some(false)) => return "allowed contacting them again".to_owned(),
                _ => {}
            }
            let fields = changed_fields(c);
            if fields.is_empty() {
                "saved it".to_owned()
            } else if fields.len() > 4 {
                format!("changed {} and {} more", fields[..3].join(", "), fields.len() - 3)
            } else {
                format!("changed {}", join_and(&fields))
            }
        }
    }
}

/// The change Undo applies to: the newest one that isn't itself an undo and hasn't been undone (the API's rule).
/// `changes` is newest first.
pub fn undoable(changes: &[CrmChange]) -> Option<&CrmChange> {
    changes.iter().find(|c| c.op != "undo" && c.undone_at.is_none())
}

/// Who turned do-not-contact on, and when: the newest change that switched it from off to on.
pub fn dnc_set_by(changes: &[CrmChange]) -> Option<&CrmChange> {
    let on = |v: &Option<Value>| v.as_ref().is_some_and(|v| v["do_not_contact"] == Value::Bool(true));
    changes.iter().find(|c| on(&c.after) && !on(&c.before))
}

/// A CSV file's shape before it is sent: its header names and how many data rows it has (quoted fields may hold
/// commas and line breaks, as RFC 4180 allows). Blank lines are not counted.
#[derive(Debug, Clone, PartialEq)]
pub struct CsvShape {
    pub columns: Vec<String>,
    pub rows: usize,
}

pub fn csv_shape(text: &str) -> CsvShape {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut columns = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut header = true;
    let mut rows = 0usize;
    let mut line_has_content = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                line_has_content = true;
            }
            ',' => {
                line_has_content = true;
                if header {
                    columns.push(std::mem::take(&mut field).trim().to_owned());
                }
            }
            '\r' => {}
            '\n' => {
                if header {
                    if line_has_content || !field.trim().is_empty() {
                        columns.push(std::mem::take(&mut field).trim().to_owned());
                        header = false;
                    }
                } else if line_has_content || !field.trim().is_empty() {
                    rows += 1;
                }
                field.clear();
                line_has_content = false;
            }
            _ => {
                if !c.is_whitespace() {
                    line_has_content = true;
                }
                field.push(c);
            }
        }
    }
    if header {
        if line_has_content || !field.trim().is_empty() {
            columns.push(field.trim().to_owned());
        }
    } else if line_has_content || !field.trim().is_empty() {
        rows += 1;
    }
    CsvShape { columns, rows }
}

/// The columns an import reads per kind (`companies` | `contacts` | `deals`), as the export writes them.
pub fn csv_columns(kind: &str) -> &'static [&'static str] {
    match kind {
        "companies" => &[
            "name", "domain", "website", "industry", "size", "location", "description", "fit_score", "fit_reason", "tags",
            "source_urls",
        ],
        "contacts" => &[
            "name", "title", "email", "company", "company_domain", "linkedin_url", "x_handle", "notes", "tags",
            "source_urls", "do_not_contact", "dnc_reason",
        ],
        _ => &[
            "title", "company", "company_domain", "contact", "contact_email", "stage", "value_cents", "currency",
            "next_step", "next_step_at",
        ],
    }
}

/// A header cell as the import reads it: "Fit Score" and "fit-score" are `fit_score`.
pub fn header_key(h: &str) -> String {
    h.trim().trim_start_matches('\u{feff}').to_lowercase().replace([' ', '-'], "_")
}

/// Which of a file's columns the import uses, which it ignores, and what it still needs (`None`: nothing).
pub fn csv_fit(kind: &str, columns: &[String]) -> (Vec<String>, Vec<String>, Option<&'static str>) {
    let known = csv_columns(kind);
    let (mut used, mut ignored) = (Vec::new(), Vec::new());
    for c in columns.iter().filter(|c| !c.trim().is_empty()) {
        if known.contains(&header_key(c).as_str()) { used.push(header_key(c)) } else { ignored.push(c.clone()) }
    }
    let has = |k: &str| used.iter().any(|u| u == k);
    let missing = match kind {
        "companies" | "contacts" if !has("name") => Some("a name column"),
        "deals" if !has("title") => Some("a title column"),
        "deals" if !has("company") && !has("company_domain") => Some("a company (or company_domain) column"),
        _ => None,
    };
    (used, ignored, missing)
}

/// The events a webhook can get, with what each means.
pub const WEBHOOK_EVENTS: [(&str, &str); 9] = [
    ("company.created", "A company is added"),
    ("company.updated", "A company changes"),
    ("contact.created", "A contact is added"),
    ("contact.updated", "A contact changes"),
    ("contact.do_not_contact", "Someone is marked do-not-contact"),
    ("deal.created", "A deal is added"),
    ("deal.updated", "A deal changes"),
    ("deal.stage_changed", "A deal moves stage"),
    ("activity.created", "Something happens on the timeline"),
];

/// Where a webhook may point, checked as it is typed (the API checks again, and again at every delivery): `https` to a
/// public address, or this computer over `http` or `https`; never the local network. `Err` is the message to show.
pub fn webhook_url_problem(s: &str) -> Result<(), &'static str> {
    use std::net::IpAddr;
    let s = s.trim();
    if s.is_empty() {
        return Err("Paste the address the other app gave you.");
    }
    let Ok(url) = reqwest::Url::parse(s) else {
        return Err("That isn't a web address. It starts with https://");
    };
    let scheme = url.scheme();
    if scheme != "https" && scheme != "http" {
        return Err("Use an https:// address.");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Leave the user name and password out of the address.");
    }
    let host = url.host_str().unwrap_or("").trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    if host.is_empty() {
        return Err("The address needs a host name.");
    }
    let local = host == "localhost" || host.ends_with(".localhost");
    match host.parse::<IpAddr>() {
        Ok(ip) => {
            let ip = match ip {
                IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
                v4 => v4,
            };
            if ip.is_loopback() {
                return Ok(());
            }
            let private = match ip {
                IpAddr::V4(v4) => {
                    let o = v4.octets();
                    v4.is_private() || (o[0] == 100 && (64..128).contains(&o[1]))
                }
                IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
            };
            if private {
                return Err("Addresses on your local network aren't allowed: the updates carry customer data. Use a public https address, or one on this computer.");
            }
            let odd = match ip {
                IpAddr::V4(v4) => v4.is_link_local() || v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast() || v4.is_documentation(),
                IpAddr::V6(v6) => v6.is_unspecified() || v6.is_multicast() || (v6.segments()[0] & 0xffc0) == 0xfe80,
            };
            if odd {
                return Err("That address can't receive updates. Use a public https address.");
            }
        }
        Err(_) if local => return Ok(()),
        Err(_) => {}
    }
    if scheme == "http" {
        return Err("Use https:// (plain http is only allowed to this computer).");
    }
    Ok(())
}

/// The stage menu's keyboard: up and down move (wrapping), Home and End jump, a digit picks that stage. `None`: the
/// key isn't the menu's.
pub fn menu_step(at: usize, len: usize, key: &str) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match key {
        "down" | "tab" => Some((at + 1) % len),
        "up" => Some((at + len - 1) % len),
        "home" | "pageup" => Some(0),
        "end" | "pagedown" => Some(len - 1),
        d if d.len() == 1 => d.parse::<usize>().ok().filter(|n| (1..=len).contains(n)).map(|n| n - 1),
        _ => None,
    }
}

/// Text search on what the board shows (the board's cards are filtered here; the tables search on the server).
pub fn matches(q: &str, fields: &[Option<&str>]) -> bool {
    let q = q.trim().to_lowercase();
    q.is_empty() || fields.iter().flatten().any(|f| f.to_lowercase().contains(&q))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn money_reads_well() {
        assert_eq!(money(1_250_000, "USD"), "$12,500");
        assert_eq!(money(1_250_050, "usd"), "$12,500.50");
        assert_eq!(money(90_000, "CHF"), "CHF 900");
        assert_eq!(money(0, "EUR"), "€0");
        assert_eq!(money_short(95_000, "USD"), "$950");
        assert_eq!(money_short(1_250_000, "USD"), "$12.5k");
        assert_eq!(money_short(100_000_00, "USD"), "$100k");
        assert_eq!(money_short(120_000_000, "GBP"), "£1.2M");
        assert_eq!(money_short(2_000_000, "USD"), "$20k");
        assert_eq!(thousands(1_204), "1,204");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000_000), "1,000,000");
    }

    #[test]
    fn money_parses() {
        assert_eq!(parse_money("12500"), Some(Some(1_250_000)));
        assert_eq!(parse_money("$12,500.50"), Some(Some(1_250_050)));
        assert_eq!(parse_money("12.5k"), Some(Some(1_250_000)));
        assert_eq!(parse_money("2M"), Some(Some(200_000_000)));
        assert_eq!(parse_money("  "), Some(None));
        for bad in ["-5", "abc", "1e400", "12.5x"] {
            assert_eq!(parse_money(bad), None, "{bad}");
        }
    }

    #[test]
    fn dates_parse_and_read() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let day = |d: Option<Option<DateTime<Utc>>>| d.flatten().map(|t| t.with_timezone(&Local).date_naive());
        assert_eq!(day(parse_due("2026-10-20", today)), NaiveDate::from_ymd_opt(2026, 10, 20));
        assert_eq!(day(parse_due("tomorrow", today)), NaiveDate::from_ymd_opt(2026, 10, 10));
        assert_eq!(day(parse_due("in 3 days", today)), NaiveDate::from_ymd_opt(2026, 10, 12));
        assert_eq!(day(parse_due("+7d", today)), NaiveDate::from_ymd_opt(2026, 10, 16));
        assert_eq!(parse_due("", today), Some(None));
        assert_eq!(parse_due("someday", today), None);
        let now = Utc::now();
        assert_eq!(due_words(now, now), ("today".to_owned(), false));
        assert_eq!(due_words(now + chrono::Duration::days(3), now).0, "in 3 days");
        assert_eq!(due_words(now - chrono::Duration::days(2), now), ("2 days late".to_owned(), true));
    }

    #[test]
    fn tags_and_initials() {
        assert_eq!(parse_tags(" saas, Fintech ,saas,, eu "), vec!["saas", "Fintech", "eu"]);
        assert_eq!(parse_tags(""), Vec::<String>::new());
        assert_eq!(initials("Acme Robotics Inc"), "AR");
        assert_eq!(initials("sam"), "S");
        assert_eq!(initials("  "), "");
    }

    #[test]
    fn changes_in_words() {
        let ch = |op: &str, before: Value, after: Value| CrmChange {
            op: op.into(),
            before: Some(before),
            after: Some(after),
            ..Default::default()
        };
        let c = ch("update", json!({"fit_score": 40, "tags": [], "updated_at": "a"}), json!({"fit_score": 80, "tags": ["x"], "updated_at": "b"}));
        assert_eq!(changed_fields(&c), vec!["fit score", "tags"]);
        assert_eq!(change_words(&c), "changed fit score and tags");
        assert_eq!(change_words(&ch("create", Value::Null, json!({}))), "added it");
        assert_eq!(change_words(&ch("update", json!({"deleted_at": null}), json!({"deleted_at": "t"}))), "deleted it");
        assert_eq!(change_words(&ch("undo", json!({"deleted_at": "t"}), json!({"deleted_at": null}))), "brought it back");
        let many = ch(
            "update",
            json!({"name": "a", "domain": "a", "industry": "a", "size": "a", "location": "a"}),
            json!({"name": "b", "domain": "b", "industry": "b", "size": "b", "location": "b"}),
        );
        assert_eq!(change_words(&many), "changed domain, industry, location and 2 more");
        let dnc = ch("update", json!({"do_not_contact": false}), json!({"do_not_contact": true, "dnc_reason": "x"}));
        assert_eq!(change_words(&dnc), "marked them do-not-contact");
        let cleared = ch("update", json!({"do_not_contact": true}), json!({"do_not_contact": false}));
        assert_eq!(change_words(&cleared), "allowed contacting them again");
    }

    #[test]
    fn undo_and_dnc_lookups() {
        let mk = |op: &str, undone: bool, dnc: Option<(bool, bool)>| CrmChange {
            op: op.into(),
            undone_at: undone.then(Utc::now),
            before: dnc.map(|(b, _)| json!({"do_not_contact": b})),
            after: dnc.map(|(_, a)| json!({"do_not_contact": a})),
            ..Default::default()
        };
        let list = vec![mk("undo", false, None), mk("update", true, None), mk("update", false, Some((false, true))), mk("create", false, None)];
        assert_eq!(undoable(&list).map(|c| c.op.as_str()), Some("update"));
        assert!(std::ptr::eq(undoable(&list).unwrap(), &list[2]));
        assert!(std::ptr::eq(dnc_set_by(&list).unwrap(), &list[2]));
        assert!(undoable(&[mk("undo", false, None)]).is_none());
    }

    #[test]
    fn csv_shape_counts_rows() {
        let s = csv_shape("\u{feff}name,domain, tags\r\nAcme,acme.com,\"a, b\"\r\n\r\n\"Multi\nline\",x.com,\nLast,y.com,z");
        assert_eq!(s.columns, vec!["name", "domain", "tags"]);
        assert_eq!(s.rows, 3);
        assert_eq!(csv_shape("").rows, 0);
        assert_eq!(csv_shape("name\n").columns, vec!["name"]);
        assert_eq!(csv_shape("name\n\n\n").rows, 0);
        assert_eq!(csv_shape("\"say \"\"hi\"\"\",b\n1,2\n").columns, vec!["say \"hi\"", "b"]);
    }

    #[test]
    fn csv_columns_fit() {
        let cols = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (used, ignored, missing) = csv_fit("companies", &cols(&["Name", "Fit Score", "notes", ""]));
        assert_eq!(used, vec!["name", "fit_score"]);
        assert_eq!(ignored, vec!["notes"]);
        assert_eq!(missing, None);
        assert_eq!(csv_fit("contacts", &cols(&["email"])).2, Some("a name column"));
        assert_eq!(csv_fit("deals", &cols(&["title"])).2, Some("a company (or company_domain) column"));
        assert_eq!(csv_fit("deals", &cols(&["title", "company-domain"])).2, None);
        assert_eq!(header_key("\u{feff}Next Step "), "next_step");
        assert_eq!(header_key(" fit-score"), "fit_score");
    }

    #[test]
    fn webhook_addresses() {
        for ok in ["https://hooks.zapier.com/hooks/catch/1/abc", "http://localhost:8080/x", "http://127.0.0.1:9000", "https://[::1]/h", "https://8.8.8.8/x"] {
            assert_eq!(webhook_url_problem(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            "hooks.example.com",
            "ftp://example.com",
            "http://example.com/hook",
            "https://user:pw@example.com/",
            "https://192.168.1.20/hook",
            "https://10.0.0.5",
            "https://172.20.1.1",
            "https://100.100.1.1",
            "https://[fd00::1]/",
            "https://169.254.169.254/latest",
            "https://0.0.0.0/",
            "https://[::ffff:192.168.0.1]/",
        ] {
            assert!(webhook_url_problem(bad).is_err(), "{bad}");
        }
        assert!(webhook_url_problem("https://192.168.1.20/hook").unwrap_err().contains("local network"));
    }

    #[test]
    fn menu_keys() {
        assert_eq!(menu_step(0, 8, "down"), Some(1));
        assert_eq!(menu_step(7, 8, "down"), Some(0));
        assert_eq!(menu_step(0, 8, "up"), Some(7));
        assert_eq!(menu_step(3, 8, "end"), Some(7));
        assert_eq!(menu_step(3, 8, "home"), Some(0));
        assert_eq!(menu_step(3, 8, "5"), Some(4));
        assert_eq!(menu_step(3, 8, "9"), None);
        assert_eq!(menu_step(3, 8, "a"), None);
        assert_eq!(menu_step(0, 0, "down"), None);
    }

    #[test]
    fn stages_and_search() {
        assert_eq!(stage_index(DealStage::Won), Some(6));
        assert_eq!(stage_label(DealStage::Meeting), "Meeting");
        assert!(matches("acme", &[Some("Pilot"), Some("ACME Robotics"), None]));
        assert!(!matches("zzz", &[Some("Pilot")]));
        assert!(matches(" ", &[]));
    }
}
