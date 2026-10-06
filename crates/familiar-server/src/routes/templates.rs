//! Teammate templates ("hire" a ready-made teammate): a static catalog, and creating a teammate from one in a single
//! transaction: the bot, its schedules (created off, so nothing runs until you turn them on), links to connectors you
//! already have from the suggested presets, and `bots.setup` for the teammate page's "Set up" checklist.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::LazyLock;
use uuid::Uuid;

use super::{Body, Id, Row, bots, one_of, schedules, text};
use crate::{
    S,
    auth::Auth,
    error::{ApiError, R},
};

const TEMPLATES: &str = include_str!("../templates.json");

/// What an unanswered question fills in (the instructions tell the teammate to ask).
const NOT_SET: &str = "(not set yet)";

#[derive(Deserialize)]
struct Question {
    key: String,
}

#[derive(Deserialize)]
struct TemplateSchedule {
    label: String,
    cron: String,
    prompt: String,
}

#[derive(Deserialize)]
struct Login {
    site: String,
    url: String,
}

/// The fields creation needs; the list endpoint serves the file as it is.
#[derive(Deserialize)]
struct Template {
    id: String,
    name: String,
    avatar: Value,
    model: String,
    questions: Vec<Question>,
    instructions: String,
    schedules: Vec<TemplateSchedule>,
    logins: Vec<Login>,
    connectors: Vec<String>,
    first_task: Option<String>,
}

/// Parsed once; the unit tests below keep the file valid.
static CATALOG: LazyLock<Vec<Template>> =
    LazyLock::new(|| serde_json::from_str(TEMPLATES).expect("templates.json is valid"));

pub async fn list(_a: Auth) -> R<Json<Value>> {
    serde_json::from_str(TEMPLATES)
        .map(Json)
        .map_err(|_| ApiError::Internal)
}

/// Replace every `{{key}}` of the template's questions with the trimmed answer (or [`NOT_SET`]).
fn fill(t: &Template, s: &str, answers: &BTreeMap<String, String>) -> String {
    let mut out = s.to_string();
    for q in &t.questions {
        let v = answers.get(&q.key).map(|v| v.trim()).filter(|v| !v.is_empty());
        out = out.replace(&format!("{{{{{}}}}}", q.key), v.unwrap_or(NOT_SET));
    }
    out
}

#[derive(Deserialize)]
pub struct FromTemplate {
    #[serde(default)]
    answers: BTreeMap<String, String>,
    name: Option<String>,
    /// The (edited) instructions; `{{key}}` placeholders in them are still filled from the answers.
    instructions: Option<String>,
    engine: Option<String>,
    model: Option<String>,
    avatar: Option<Value>,
}

pub async fn create(
    State(st): State<S>,
    a: Auth,
    Path(id): Path<String>,
    Body(b): Body<FromTemplate>,
) -> R<(StatusCode, Json<Value>)> {
    let t = CATALOG.iter().find(|t| t.id == id).ok_or(ApiError::NotFound)?;
    if b.answers.values().any(|v| v.chars().count() > 4000) {
        return Err(ApiError::bad("answers must be at most 4000 characters each"));
    }
    let name = text(b.name.as_deref().unwrap_or(&t.name), "name", 100)?;
    let engine = one_of(b.engine.as_deref().unwrap_or("claude"), "engine", &bots::ENGINES)?;
    let model = match b.model.as_deref() {
        Some(m) => bots::check_model(&engine, m)?,
        None if engine == "codex" => return Err(ApiError::bad("set a model for the codex engine")),
        None => bots::check_model(&engine, &t.model)?,
    };
    let avatar = b.avatar.filter(|v| !v.is_null()).unwrap_or_else(|| t.avatar.clone());
    bots::check_avatar(&avatar)?;
    let persona = fill(t, b.instructions.as_deref().unwrap_or(&t.instructions), &b.answers);
    if persona.chars().count() > 20_000 {
        return Err(ApiError::bad("instructions too long (max 20000)"));
    }

    let mut tx = st.pool.begin().await?;
    // A taken slug (a second hire of the same template) gets a short suffix.
    let base = bots::slugify(&name);
    let taken: bool =
        sqlx::query_scalar("select exists(select 1 from bots where owner_id = $1 and slug = $2)")
            .bind(a.user)
            .bind(&base)
            .fetch_one(&mut *tx)
            .await?;
    let slug = if taken {
        let stem: String = base.chars().take(34).collect();
        format!("{}-{}", stem.trim_end_matches('-'), &Uuid::new_v4().simple().to_string()[..4])
    } else {
        base
    };
    let bot: Uuid = sqlx::query_scalar(
        "insert into bots (owner_id, slug, name, persona, model, engine, avatar)
         values ($1, $2, $3, $4, $5, $6, $7) returning id",
    )
    .bind(a.user)
    .bind(&slug)
    .bind(&name)
    .bind(&persona)
    .bind(&model)
    .bind(&engine)
    .bind(sqlx::types::Json(&avatar))
    .fetch_one(&mut *tx)
    .await?;

    let mut schedule_ids = Vec::new();
    for s in &t.schedules {
        // the BEFORE INSERT trigger creates the schedule's thread; name it after the schedule
        let (sid, thread): (Uuid, Uuid) = sqlx::query_as(
            "insert into schedules (owner_id, bot_id, cron, prompt, kind, enabled)
             values ($1, $2, $3, $4, 'scheduled', false) returning id, thread_id",
        )
        .bind(a.user)
        .bind(bot)
        .bind(schedules::cron(&s.cron)?)
        .bind(fill(t, &s.prompt, &b.answers))
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("update threads set source = 'schedule', title = $2 where id = $1")
            .bind(thread)
            .bind(&s.label)
            .execute(&mut *tx)
            .await?;
        schedule_ids.push(sid);
    }

    sqlx::query(
        "insert into bot_connectors (bot_id, connector_id)
         select $1, c.id from connectors c where c.owner_id = $2 and c.preset = any($3)
         on conflict do nothing",
    )
    .bind(bot)
    .bind(a.user)
    .bind(&t.connectors)
    .execute(&mut *tx)
    .await?;

    let setup = json!({
        "template": t.id,
        "logins": t.logins.iter().map(|l| json!({ "site": l.site, "url": l.url, "done": false })).collect::<Vec<_>>(),
        "schedules": schedule_ids,
        "dismissed": false,
    });
    sqlx::query("update bots set setup = $2 where id = $1")
        .bind(bot)
        .bind(sqlx::types::Json(&setup))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let row = bots::bot_json(&st.pool, a.user, bot).await?;
    let first_task = t.first_task.as_deref().map(|f| fill(t, f, &b.answers));
    Ok((
        StatusCode::CREATED,
        Json(json!({ "bot": row.0.0, "first_task": first_task })),
    ))
}

#[derive(Deserialize)]
pub struct SetupPatch {
    /// A template login's `site`, ticked (or unticked with `done: false`).
    login: Option<String>,
    done: Option<bool>,
    /// Hide the checklist for good.
    dismissed: Option<bool>,
}

pub async fn setup(
    State(st): State<S>,
    a: Auth,
    Id(id): Id,
    Body(p): Body<SetupPatch>,
) -> R<Json<Row>> {
    let mut tx = st.pool.begin().await?;
    let cur: Option<Option<sqlx::types::Json<Value>>> =
        sqlx::query_scalar("select setup from bots where id = $1 and owner_id = $2 for update")
            .bind(id)
            .bind(a.user)
            .fetch_optional(&mut *tx)
            .await?;
    let mut setup = cur
        .ok_or(ApiError::NotFound)?
        .map(|j| j.0)
        .ok_or_else(|| ApiError::bad("this teammate was not made from a template"))?;
    if let Some(site) = p.login.as_deref() {
        let login = setup["logins"]
            .as_array_mut()
            .and_then(|l| l.iter_mut().find(|l| l["site"] == site))
            .ok_or_else(|| ApiError::bad(format!("no login for '{site}' in this teammate's setup")))?;
        login["done"] = json!(p.done.unwrap_or(true));
    }
    if let Some(d) = p.dismissed {
        setup["dismissed"] = json!(d);
    }
    sqlx::query("update bots set setup = $3 where id = $1 and owner_id = $2")
        .bind(id)
        .bind(a.user)
        .bind(sqlx::types::Json(&setup))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    bots::bot_json(&st.pool, a.user, id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::str::FromStr;

    const CATEGORIES: [&str; 6] = ["Growth & marketing", "Sales", "Social", "Research", "Desk work", "Personal"];
    // The avatar builder's options (apps/native familiar-ui `mascot.rs`, the web's `Mascot.tsx`).
    const PALETTE: [&str; 6] = ["#7285d5", "#e58fa4", "#4fb98a", "#eda84b", "#a283d8", "#4fa9cf"];
    const ACCESSORIES: [&str; 7] = ["none", "hat", "glasses", "headphones", "bow", "antenna", "crown"];

    fn catalog() -> Vec<Value> {
        serde_json::from_str(TEMPLATES).expect("templates.json parses")
    }

    fn placeholders(s: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut rest = s;
        while let Some(i) = rest.find("{{") {
            let after = &rest[i + 2..];
            let end = after.find("}}").unwrap_or_else(|| panic!("unclosed placeholder in {s:?}"));
            out.insert(after[..end].to_string());
            rest = &after[end + 2..];
        }
        out
    }

    fn str_of<'a>(v: &'a Value, k: &str, id: &str) -> &'a str {
        v[k].as_str().unwrap_or_else(|| panic!("{id}: `{k}` must be a string"))
    }

    #[test]
    fn catalog_is_valid() {
        let all = catalog();
        assert!(all.len() >= 9, "expected at least 9 templates");
        assert_eq!(CATALOG.len(), all.len());
        let presets: Vec<Value> = serde_json::from_str(include_str!("../presets.json")).unwrap();
        let preset_ids: BTreeSet<&str> = presets.iter().filter_map(|p| p["id"].as_str()).collect();
        let mut ids = BTreeSet::new();
        for t in &all {
            let id = str_of(t, "id", "?");
            assert!(ids.insert(id), "duplicate template id {id}");
            assert!(bots::slugify(id) == id, "{id}: ids are kebab-case");
            for k in ["name", "summary", "instructions"] {
                assert!(!str_of(t, k, id).trim().is_empty(), "{id}: empty {k}");
            }
            assert!(CATEGORIES.contains(&str_of(t, "category", id)), "{id}: unknown category");
            assert!(bots::MODELS.contains(&str_of(t, "model", id)), "{id}: model must be a Claude alias");
            // Every outward action is proposed as a draft first, and the approved text is used as it comes back.
            let rules = str_of(t, "instructions", id);
            assert!(rules.contains("through `propose_draft` first") && rules.contains("EXACTLY the text"), "{id}: drafts rule");
            assert!(!rules.contains("approved that exact action through `ask_user`"), "{id}: old approval rule");

            let a = &t["avatar"];
            bots::check_avatar(a).unwrap_or_else(|_| panic!("{id}: avatar rejected by the API"));
            assert!(a["shape"].as_u64().is_some_and(|n| n < 5), "{id}: avatar.shape");
            assert!(a["eyes"].as_u64().is_some_and(|n| n < 5), "{id}: avatar.eyes");
            assert!(a["mouth"].as_u64().is_some_and(|n| n < 4), "{id}: avatar.mouth");
            assert!(PALETTE.contains(&str_of(a, "color", id)), "{id}: avatar.color");
            assert!(ACCESSORIES.contains(&str_of(a, "accessory", id)), "{id}: avatar.accessory");

            // every placeholder has a question, and every question is used
            let mut keys = BTreeSet::new();
            for q in t["questions"].as_array().unwrap() {
                let key = str_of(q, "key", id);
                assert!(
                    !key.is_empty() && key.bytes().all(|c| c.is_ascii_lowercase() || c == b'_'),
                    "{id}: question key {key:?}"
                );
                assert!(keys.insert(key.to_string()), "{id}: duplicate question {key}");
                assert!(!str_of(q, "label", id).is_empty() && q["placeholder"].is_string(), "{id}: {key} label");
                assert!(q["multiline"].is_boolean(), "{id}: {key}.multiline");
            }
            let mut used = placeholders(str_of(t, "instructions", id));
            for s in t["schedules"].as_array().unwrap() {
                assert!(!str_of(s, "label", id).trim().is_empty(), "{id}: schedule label");
                let c = str_of(s, "cron", id);
                schedules::cron(c).unwrap_or_else(|_| panic!("{id}: bad cron {c:?}"));
                croner::Cron::from_str(c).unwrap_or_else(|e| panic!("{id}: bad cron {c:?}: {e}"));
                used.extend(placeholders(str_of(s, "prompt", id)));
            }
            if let Some(f) = t["first_task"].as_str() {
                used.extend(placeholders(f));
            }
            assert_eq!(used, keys, "{id}: placeholders and questions differ");

            for l in t["logins"].as_array().unwrap() {
                assert!(!str_of(l, "site", id).trim().is_empty(), "{id}: login site");
                let u = url::Url::parse(str_of(l, "url", id)).unwrap_or_else(|_| panic!("{id}: login url"));
                assert!(u.scheme() == "https" && u.host_str().is_some(), "{id}: login urls are https");
            }
            for c in t["connectors"].as_array().unwrap() {
                let c = c.as_str().unwrap();
                assert!(preset_ids.contains(c), "{id}: unknown connector preset {c}");
            }
        }
    }

    #[test]
    fn fill_replaces_every_placeholder() {
        for t in CATALOG.iter() {
            let answers: BTreeMap<String, String> =
                t.questions.iter().map(|q| (q.key.clone(), format!("<{}>", q.key))).collect();
            let all_text = std::iter::once(t.instructions.as_str())
                .chain(t.schedules.iter().map(|s| s.prompt.as_str()))
                .chain(t.first_task.as_deref());
            for s in all_text {
                let filled = fill(t, s, &answers);
                assert!(!filled.contains("{{"), "{}: left a placeholder", t.id);
                // unanswered questions say so instead of leaving braces behind
                let empty = fill(t, s, &BTreeMap::new());
                assert!(!empty.contains("{{"));
                assert!(placeholders(s).is_empty() || empty.contains(NOT_SET));
            }
            // a generous answer to everything still fits the persona limit
            let long: BTreeMap<String, String> =
                t.questions.iter().map(|q| (q.key.clone(), "x".repeat(600))).collect();
            assert!(fill(t, &t.instructions, &long).chars().count() <= 20_000, "{}: instructions too long", t.id);
        }
    }
}
