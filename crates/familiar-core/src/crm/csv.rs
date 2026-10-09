//! CSV export and import for the CRM (RFC 4180: UTF-8, a header row, `"` quoting, `""` for a quote inside a cell).
//! Cells that start like a spreadsheet formula get a `'` in front on export (and lose it again on import), so a lead
//! named `=HYPERLINK(...)` can't run anything when the file is opened.

use super::{Actor, CompanyInput, ContactInput, CrmError, DealInput, Kind, Outcome, Result};
use crate::db::Db;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::{Acquire, PgConnection};
use std::collections::HashMap;
use uuid::Uuid;

pub const MAX_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_ROWS: usize = 5000;

/// (CSV header, key in the row JSON) per kind, in export order. An import reads the same headers.
fn columns(kind: Kind) -> &'static [(&'static str, &'static str)] {
    match kind {
        Kind::Company => &[
            ("name", "name"), ("domain", "domain"), ("website", "website"), ("industry", "industry"),
            ("size", "size"), ("location", "location"), ("description", "description"), ("fit_score", "fit_score"),
            ("fit_reason", "fit_reason"), ("tags", "tags"), ("source_urls", "source_urls"),
        ],
        Kind::Contact => &[
            ("name", "name"), ("title", "title"), ("email", "email"), ("company", "company_name"),
            ("company_domain", "company_domain"), ("linkedin_url", "linkedin_url"), ("x_handle", "x_handle"),
            ("notes", "notes"), ("tags", "tags"), ("source_urls", "source_urls"),
            ("do_not_contact", "do_not_contact"), ("dnc_reason", "dnc_reason"),
        ],
        _ => &[
            ("title", "title"), ("company", "company_name"), ("company_domain", "company_domain"),
            ("contact", "contact_name"), ("contact_email", "contact_email"), ("stage", "stage"),
            ("value_cents", "value_cents"), ("currency", "currency"), ("next_step", "next_step"),
            ("next_step_at", "next_step_at"),
        ],
    }
}

// ---- RFC 4180 ---------------------------------------------------------------

/// Split CSV text into rows of cells. A leading byte-order mark is dropped; `\r\n` and `\n` both end a row.
pub fn parse(text: &str) -> std::result::Result<Vec<Vec<String>>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    cell.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                _ => cell.push(c),
            }
            continue;
        }
        match c {
            '"' if cell.is_empty() => quoted = true,
            '"' => return Err(format!("row {}: a quote in the middle of a cell", rows.len() + 1)),
            ',' => row.push(std::mem::take(&mut cell)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut cell));
                rows.push(std::mem::take(&mut row));
            }
            _ => cell.push(c),
        }
    }
    if quoted {
        return Err("a quoted cell is never closed".into());
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    Ok(rows)
}

/// A cell that a spreadsheet would run as a formula gets a `'` in front.
pub fn neutralise(cell: &str) -> String {
    if cell.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{cell}") } else { cell.to_string() }
}

/// Undo [`neutralise`] (for the four formula characters).
pub fn restore(cell: &str) -> &str {
    match cell.strip_prefix('\'') {
        Some(rest) if rest.starts_with(['=', '+', '-', '@']) => rest,
        _ => cell,
    }
}

/// Rows to CSV text (CRLF line ends, quoting where needed, formula cells neutralised).
pub fn write(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .map(|c| {
                let c = neutralise(c);
                if c.contains([',', '"', '\n', '\r']) { format!("\"{}\"", c.replace('"', "\"\"")) } else { c }
            })
            .collect();
        out.push_str(&cells.join(","));
        out.push_str("\r\n");
    }
    out
}

// ---- export -----------------------------------------------------------------

fn cell(header: &str, v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => {
            let sep = if header == "tags" { "; " } else { "\n" };
            a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(sep)
        }
        _ => String::new(),
    }
}

/// The rows (as [`super::all`] returns them) as CSV text with a header row.
pub fn export(kind: Kind, rows: &[Value]) -> String {
    let cols = columns(kind);
    let mut out = vec![cols.iter().map(|c| c.0.to_string()).collect::<Vec<_>>()];
    for r in rows {
        out.push(cols.iter().map(|(h, k)| cell(h, &r[*k])).collect());
    }
    write(&out)
}

// ---- import -----------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ImportError {
    /// The row in the file (the header is row 1).
    pub row: u32,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportResult {
    pub created: u32,
    pub updated: u32,
    /// Blank rows, and rows that match a record that already has all of it.
    pub skipped: u32,
    pub errors: Vec<ImportError>,
}

fn header(h: &str) -> String {
    h.trim().trim_start_matches('\u{feff}').to_lowercase().replace([' ', '-'], "_")
}

type Fields = HashMap<String, String>;

fn bool_cell(f: &Fields, key: &str) -> Result<Option<bool>> {
    match f.get(key).map(|s| s.to_lowercase()).as_deref() {
        None => Ok(None),
        Some("true" | "yes" | "y" | "1") => Ok(Some(true)),
        Some("false" | "no" | "n" | "0") => Ok(Some(false)),
        Some(_) => Err(CrmError::Invalid(format!("{key} must be true or false"))),
    }
}

fn num_cell<T: std::str::FromStr>(f: &Fields, key: &str) -> Result<Option<T>> {
    f.get(key)
        .map(|s| s.parse().map_err(|_| CrmError::Invalid(format!("{key} must be a whole number"))))
        .transpose()
}

fn tag_cell(f: &Fields) -> Option<Vec<String>> {
    f.get("tags").map(|s| s.split(';').map(str::to_string).collect())
}

fn url_cell(f: &Fields) -> Option<Vec<String>> {
    f.get("source_urls").map(|s| s.split_whitespace().map(str::to_string).collect())
}

/// The company a row names (by domain, else name), created when it doesn't exist yet.
async fn company_ref(tx: &mut PgConnection, owner: Uuid, f: &Fields) -> Result<Option<Uuid>> {
    let (name, dom) = (f.get("company"), f.get("company_domain"));
    if name.is_none() && dom.is_none() {
        return Ok(None);
    }
    let domain = dom
        .map(|d| super::domain(d).ok_or_else(|| CrmError::Invalid("company_domain must be a host name".into())))
        .transpose()?;
    if let Some(id) = super::find_company_in(tx, owner, domain.as_deref(), name.map(String::as_str)).await? {
        return Ok(Some(id));
    }
    let input = CompanyInput { name: name.or(dom).cloned(), domain: dom.cloned(), ..Default::default() };
    let (row, _) = super::upsert_company_in(tx, owner, &Actor::User, &input).await?;
    Ok(Some(super::id_of(&row)))
}

async fn import_row(tx: &mut PgConnection, owner: Uuid, kind: Kind, f: &Fields) -> Result<Outcome> {
    let g = |k: &str| f.get(k).cloned();
    match kind {
        Kind::Company => {
            let i = CompanyInput {
                name: g("name"),
                domain: g("domain"),
                website: g("website"),
                industry: g("industry"),
                size: g("size"),
                location: g("location"),
                description: g("description"),
                // an empty cell is no cell: an import never clears
                fit_score: num_cell(f, "fit_score")?.map(Some),
                fit_reason: g("fit_reason"),
                tags: tag_cell(f),
                source_urls: url_cell(f),
                custom: None,
            };
            Ok(super::upsert_company_in(tx, owner, &Actor::User, &i).await?.1)
        }
        Kind::Contact => {
            let i = ContactInput {
                company_id: company_ref(tx, owner, f).await?,
                name: g("name"),
                title: g("title"),
                email: g("email"),
                linkedin_url: g("linkedin_url"),
                x_handle: g("x_handle"),
                notes: g("notes"),
                tags: tag_cell(f),
                source_urls: url_cell(f),
                custom: None,
                // An import only ever adds a do-not-contact: an old export re-imported must not lift one (or its reason).
                do_not_contact: bool_cell(f, "do_not_contact")?.filter(|dnc| *dnc),
                dnc_reason: g("dnc_reason").filter(|r| !r.trim().is_empty()),
            };
            Ok(super::upsert_contact_in(tx, owner, &Actor::User, &i).await?.1)
        }
        _ => {
            let company = company_ref(tx, owner, f).await?;
            if company.is_none() {
                return Err(CrmError::Invalid("a deal needs a company (or company_domain)".into()));
            }
            let contact = match (f.get("contact_email"), f.get("contact")) {
                (None, None) => None,
                (e, n) => {
                    let e = e.map(|e| super::email(e).ok_or_else(|| CrmError::Invalid("contact_email is not an email".into()))).transpose()?;
                    let found = super::find_contact_in(tx, owner, e.as_deref(), None, n.map(String::as_str), company).await?;
                    Some(found.ok_or_else(|| CrmError::Invalid("the contact is not in the CRM".into()))?)
                }
            };
            let next_step_at = f
                .get("next_step_at")
                .map(|s| {
                    DateTime::parse_from_rfc3339(s)
                        .map(|d| d.with_timezone(&Utc))
                        .map_err(|_| CrmError::Invalid("next_step_at must be an RFC 3339 time".into()))
                })
                .transpose()?;
            let i = DealInput {
                company_id: company,
                contact_id: contact,
                title: g("title"),
                stage: g("stage").map(|s| s.to_lowercase()),
                value_cents: num_cell(f, "value_cents")?.map(Some),
                currency: g("currency"),
                next_step: g("next_step"),
                next_step_at: next_step_at.map(Some),
            };
            Ok(super::upsert_deal_in(tx, owner, &Actor::User, &i).await?.1)
        }
    }
}

/// Import CSV text as the owner, deduplicating like the upserts do (an existing company, contact or deal is updated, or
/// skipped when it already has everything the row says). A bad row is reported and skipped; the others go in. With
/// `dry_run` everything runs for real inside a transaction that is rolled back, so the counts are exact and nothing
/// changes. Limits: 5 MB, 5000 rows.
pub async fn import(db: &Db, kind: Kind, text: &str, dry_run: bool) -> Result<ImportResult> {
    if kind == Kind::Activity {
        return Err(CrmError::Invalid("kind must be companies, contacts or deals".into()));
    }
    if text.len() > MAX_BYTES {
        return Err(CrmError::Invalid("the CSV is too large (max 5 MB)".into()));
    }
    let rows = parse(text).map_err(CrmError::Invalid)?;
    let Some((head, data)) = rows.split_first() else {
        return Err(CrmError::Invalid("the CSV is empty".into()));
    };
    let cols: Vec<String> = head.iter().map(|h| header(h)).collect();
    let has = |c: &str| cols.iter().any(|h| h == c);
    let (need, hint) = match kind {
        Kind::Company | Kind::Contact => (has("name"), "a `name` column"),
        _ => (has("title") && (has("company") || has("company_domain")), "a `title` column and a `company` (or `company_domain`) column"),
    };
    if !need {
        return Err(CrmError::Invalid(format!("the first row must be a header row with {hint}")));
    }
    if data.len() > MAX_ROWS {
        return Err(CrmError::Invalid(format!("too many rows (max {MAX_ROWS})")));
    }
    let known: Vec<&str> = columns(kind).iter().map(|c| c.0).collect();
    let mut out = ImportResult::default();
    let mut tx = db.pool.begin().await?;
    for (i, cells) in data.iter().enumerate() {
        let fields: Fields = cols
            .iter()
            .zip(cells)
            .filter(|(h, _)| known.contains(&h.as_str()))
            .map(|(h, c)| (h.clone(), restore(c.trim()).to_string()))
            .filter(|(_, c)| !c.is_empty())
            .collect();
        if fields.is_empty() {
            out.skipped += 1;
            continue;
        }
        let mut row_tx = tx.begin().await?;
        match import_row(&mut row_tx, db.owner, kind, &fields).await {
            Ok(o) => {
                row_tx.commit().await?;
                match o {
                    Outcome::Created => out.created += 1,
                    Outcome::Updated => out.updated += 1,
                    Outcome::Unchanged => out.skipped += 1,
                }
            }
            Err(CrmError::Db(e)) => return Err(CrmError::Db(e)),
            Err(e) => {
                row_tx.rollback().await?;
                out.errors.push(ImportError { row: i as u32 + 2, message: e.to_string() });
            }
        }
    }
    if dry_run {
        tx.rollback().await?;
    } else {
        tx.commit().await?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(a: &[&[&str]]) -> Vec<Vec<String>> {
        a.iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect()
    }

    #[test]
    fn parse_basics() {
        assert_eq!(parse("a,b\r\n1,2\n3,4").unwrap(), rows(&[&["a", "b"], &["1", "2"], &["3", "4"]]));
        assert_eq!(parse("\u{feff}a,b\n").unwrap(), rows(&[&["a", "b"]]));
        assert_eq!(parse("a,,c\n,,\n").unwrap(), rows(&[&["a", "", "c"], &["", "", ""]]));
        assert_eq!(parse("").unwrap(), Vec::<Vec<String>>::new());
    }

    #[test]
    fn parse_quotes() {
        assert_eq!(
            parse("\"a,b\",\"say \"\"hi\"\"\",\"line1\nline2\"\n").unwrap(),
            rows(&[&["a,b", "say \"hi\"", "line1\nline2"]])
        );
        assert_eq!(parse("\"\",x").unwrap(), rows(&[&["", "x"]]));
        assert!(parse("\"never closed").is_err());
        assert!(parse("ab\"c\",d").is_err());
    }

    #[test]
    fn write_quotes_and_neutralises() {
        let out = write(&rows(&[&["a,b", "say \"hi\"", "two\nlines", "plain"], &["=1+1", "+x", "-x", "@x"]]));
        assert_eq!(
            out,
            "\"a,b\",\"say \"\"hi\"\"\",\"two\nlines\",plain\r\n'=1+1,'+x,'-x,'@x\r\n"
        );
        // and it reads back
        let back = parse(&out).unwrap();
        assert_eq!(back[0], vec!["a,b", "say \"hi\"", "two\nlines", "plain"]);
        assert_eq!(back[1].iter().map(|c| restore(c)).collect::<Vec<_>>(), vec!["=1+1", "+x", "-x", "@x"]);
        // a leading apostrophe on ordinary text is left alone
        assert_eq!(restore("'hello"), "'hello");
        assert_eq!(restore("'=x"), "=x");
    }

    #[test]
    fn export_headers_and_cells() {
        let rows = vec![serde_json::json!({
            "name": "Acme, Inc", "domain": "acme.com", "fit_score": 80, "tags": ["a", "b"],
            "source_urls": ["https://x.com", "https://y.com"], "industry": null
        })];
        let csv = export(Kind::Company, &rows);
        let parsed = parse(&csv).unwrap();
        assert_eq!(parsed[0][0], "name");
        assert_eq!(parsed[1][0], "Acme, Inc");
        assert_eq!(parsed[1][7], "80");
        assert_eq!(parsed[1][9], "a; b");
        assert_eq!(parsed[1][10], "https://x.com\nhttps://y.com");
        assert_eq!(parsed[1][3], "");
    }

    #[test]
    fn cells() {
        let f: Fields = [("n", "12"), ("b", "Yes"), ("x", "abc"), ("tags", "a; b ;;c")].into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        assert_eq!(num_cell::<i32>(&f, "n").unwrap(), Some(12));
        assert!(num_cell::<i32>(&f, "x").is_err());
        assert_eq!(num_cell::<i32>(&f, "missing").unwrap(), None);
        assert_eq!(bool_cell(&f, "b").unwrap(), Some(true));
        assert!(bool_cell(&f, "x").is_err());
        assert_eq!(tag_cell(&f).unwrap(), vec!["a", " b ", "", "c"]);
        assert_eq!(header(" Company Domain "), "company_domain");
    }
}
