use std::collections::BTreeMap;

use axum::{Json, extract::State};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Body, Id, Q, Row, one_of};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

const WITH_BOT: &str =
    "(to_jsonb(a) - 'owner_id') || jsonb_build_object('bot_name', b.name, 'bot_slug', b.slug)";

#[derive(Deserialize)]
pub struct ListQuery {
    status: Option<String>,
    bot_id: Option<Uuid>,
}

/// Newest first, max 200. No `status` = every status.
pub async fn list(State(st): State<S>, a: Auth, Q(q): Q<ListQuery>) -> R<Json<Vec<Row>>> {
    if let Some(s) = &q.status {
        one_of(s, "status", &["pending", "approved", "denied", "expired", "revise"])?;
    }
    let rows = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select {WITH_BOT} from approvals a join bots b on b.id = a.bot_id
         where a.owner_id = $1 and ($2::text is null or a.status = $2) and ($3::uuid is null or a.bot_id = $3)
         order by a.created_at desc limit 200"
    )))
    .bind(a.user)
    .bind(q.status)
    .bind(q.bot_id)
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

/// Approvals that are a teammate's own question or draft, never a tool call: "Always allow" makes no sense for them.
const NOT_TOOLS: [&str; 2] = ["ask_user", "propose_draft"];

#[derive(Deserialize)]
pub struct Decision {
    /// approve | deny | revise ("Ask for changes", drafts only)
    decision: String,
    /// The answer to an `ask_user` question, or the owner's note on a deny / ask for changes.
    response: Option<String>,
    /// New text for input fields the approval lists as `editable` (approve only).
    edits: Option<BTreeMap<String, String>>,
    /// Approve and add the approval's `allow_rule` for this teammate ("Always allow this").
    always: Option<bool>,
}

pub async fn decide(
    State(st): State<S>,
    a: Auth,
    Id(id): Id,
    Body(d): Body<Decision>,
) -> R<Json<Value>> {
    let status = match d.decision.as_str() {
        "approve" => "approved",
        "deny" => "denied",
        "revise" => "revise",
        _ => return Err(ApiError::bad("decision must be approve, deny or revise")),
    };
    if d.response
        .as_deref()
        .is_some_and(|r| r.chars().count() > 20_000)
    {
        return Err(ApiError::bad("response too long (max 20000)"));
    }
    let edits = d.edits.unwrap_or_default();
    let always = d.always.unwrap_or(false);
    if status != "approved" && (!edits.is_empty() || always) {
        return Err(ApiError::bad("edits and always only go with approve"));
    }
    if always && !edits.is_empty() {
        return Err(ApiError::bad(
            "\"Always allow\" covers the action as proposed; approve an edited one on its own",
        ));
    }

    let mut tx = st.pool.begin().await?;
    // The approval, its teammate (still the owner's) and whether it is still waiting: not past its expiry, and its run
    // not over (a daemon that stopped mid-wait leaves it pending until its next start).
    type Pending = (String, String, Option<sqlx::types::Json<Value>>, Vec<String>, Option<String>, Uuid, bool);
    let row: Option<Pending> = sqlx::query_as(
        "select a.status, a.tool_name, a.input, a.editable, a.allow_rule, a.bot_id,
                coalesce(a.expires_at <= now(), false) or r.status in ('succeeded', 'failed', 'cancelled')
         from approvals a
         join bots b on b.id = a.bot_id and b.owner_id = a.owner_id
         join runs r on r.id = a.run_id
         where a.id = $1 and a.owner_id = $2 for update of a",
    )
    .bind(id)
    .bind(a.user)
    .fetch_optional(&mut *tx)
    .await?;
    let (current, tool, input, editable, allow_rule, bot, stale) = row.ok_or(ApiError::NotFound)?;
    if current != "pending" {
        return Err(ApiError::conflict("approval is no longer pending"));
    }
    if stale {
        sqlx::query(
            "update approvals set status = 'expired', decided_by = 'rule', decided_at = now()
             where id = $1 and status = 'pending'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Err(ApiError::conflict("this request expired before it was decided"));
    }
    if status == "revise" && tool != "propose_draft" {
        return Err(ApiError::bad("only drafts can be sent back for changes"));
    }
    let input = input.map(|j| j.0).unwrap_or_else(|| json!({}));
    let edited = edited_input(&tool, &input, &editable, &edits)?;

    let mut rule = None;
    if always {
        // The rule is the one the daemon offered for exactly this call, re-derived here from the stored tool and
        // input: never text a client chose, and never anything the current checks would not offer.
        let pattern = allow_rule
            .filter(|_| !NOT_TOOLS.contains(&tool.as_str()))
            .filter(|r| familiar_core::permissions::always_allow_rule(&tool, &input).as_ref() == Some(r))
            .ok_or_else(|| ApiError::bad("this action can't be always allowed"))?;
        // The same rule twice adds nothing; the existing one is reported.
        let existing: Option<Row> = sqlx::query_scalar(
            "select to_jsonb(r) - 'owner_id' from rules r
             where owner_id = $1 and bot_id = $2 and pattern = $3 and decision = 'allow' limit 1",
        )
        .bind(a.user)
        .bind(bot)
        .bind(&pattern)
        .fetch_optional(&mut *tx)
        .await?;
        rule = match existing {
            Some(r) => Some(r),
            None => Some(
                sqlx::query_scalar(
                    "insert into rules (owner_id, bot_id, pattern, decision, note)
                     values ($1, $2, $3, 'allow', 'Added with \"Always allow\" on an approval')
                     returning to_jsonb(rules) - 'owner_id'",
                )
                .bind(a.user)
                .bind(bot)
                .bind(&pattern)
                .fetch_one(&mut *tx)
                .await?,
            ),
        };
    }

    let row: Row = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "with a as (
           update approvals set status = $3, decided_by = 'user', response = $4, edited_input = $5, decided_at = now()
           where id = $1 and owner_id = $2 returning *)
         select {WITH_BOT} from a join bots b on b.id = a.bot_id"
    )))
    .bind(id)
    .bind(a.user)
    .bind(status)
    .bind(d.response)
    .bind(edited.map(sqlx::types::Json))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut out = row.0;
    if let Some(r) = rule {
        out["rule"] = r.0;
    }
    Ok(Json(out))
}

/// The input with the owner's edits applied, or None when they changed nothing. Only fields the approval lists as
/// editable may change, and only to text; a draft keeps a non-empty body. The daemon re-checks the result before
/// anything runs.
fn edited_input(
    tool: &str,
    input: &Value,
    editable: &[String],
    edits: &BTreeMap<String, String>,
) -> R<Option<Value>> {
    if edits.is_empty() {
        return Ok(None);
    }
    let mut out = input.clone();
    let Some(obj) = out.as_object_mut() else {
        return Err(ApiError::bad("this input can't be edited"));
    };
    for (field, text) in edits {
        if !editable.contains(field) {
            return Err(ApiError::bad(format!("`{field}` can't be edited here")));
        }
        if text.chars().count() > 20_000 {
            return Err(ApiError::bad(format!("{field} too long (max 20000)")));
        }
        let draft = tool == "propose_draft";
        // Drafts are text to post: surrounding blank space is never meant. Commands and files keep theirs.
        let text = if draft { text.trim() } else { text.as_str() };
        if text.trim().is_empty() && (field == "body" || !draft) {
            return Err(ApiError::bad(format!("{field} must not be empty")));
        }
        if draft && text.is_empty() {
            obj.remove(field);
        } else {
            obj.insert(field.clone(), json!(text));
        }
    }
    Ok((&out != input).then_some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edits(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn only_editable_fields_change() {
        let input = json!({ "command": "git status", "description": "look" });
        let editable = vec!["command".to_string()];
        let out = edited_input("Bash", &input, &editable, &edits(&[("command", "git status --short")])).unwrap();
        assert_eq!(out, Some(json!({ "command": "git status --short", "description": "look" })));
        assert!(edited_input("Bash", &input, &editable, &edits(&[("description", "x")])).is_err());
        assert!(edited_input("Bash", &input, &[], &edits(&[("command", "ls")])).is_err());
        assert!(edited_input("Bash", &input, &editable, &edits(&[("command", "  ")])).is_err());
        // Unchanged = no edit.
        assert_eq!(edited_input("Bash", &input, &editable, &edits(&[("command", "git status")])).unwrap(), None);
    }

    #[test]
    fn draft_edits() {
        let input = json!({ "kind": "post", "channel": "X", "body": "Hi", "to": "@a" });
        let editable: Vec<String> = ["body", "subject", "to"].map(String::from).to_vec();
        let out = edited_input("propose_draft", &input, &editable, &edits(&[("body", " Hello \n"), ("to", "")])).unwrap();
        assert_eq!(out, Some(json!({ "kind": "post", "channel": "X", "body": "Hello" })));
        assert!(edited_input("propose_draft", &input, &editable, &edits(&[("body", " ")])).is_err());
        assert!(edited_input("propose_draft", &input, &editable, &edits(&[("channel", "LinkedIn")])).is_err());
    }
}
