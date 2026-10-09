//! Row types mirroring `apps/web/src/lib/types.ts`. Tolerant by construction: every struct is
//! `#[serde(default)]`, unknown fields are ignored and unknown enum values map to `Unknown`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

macro_rules! tolerant_enum {
    ($(#[$m:meta])* $name:ident { $($var:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $s)] $var,)+
            #[default]
            #[serde(other)]
            Unknown,
        }
        impl $name {
            pub fn as_str(&self) -> &'static str {
                match self { $(Self::$var => $s,)+ Self::Unknown => "unknown" }
            }
        }
    };
}

tolerant_enum!(BotEngine { Claude = "claude", Codex = "codex" });
tolerant_enum!(BotStatus { Idle = "idle", Running = "running", Paused = "paused" });
// followup: the daemon delivering the owner's decision on a draft to its teammate.
tolerant_enum!(RunKind { Chat = "chat", Scheduled = "scheduled", Proactive = "proactive", Handoff = "handoff", Followup = "followup" });
tolerant_enum!(RunStatus {
    Queued = "queued", Running = "running", WaitingApproval = "waiting_approval",
    Succeeded = "succeeded", Failed = "failed", Cancelled = "cancelled",
});
tolerant_enum!(EventKind {
    Status = "status", Text = "text", Thinking = "thinking", ToolCall = "tool_call",
    ToolResult = "tool_result", Approval = "approval", Artifact = "artifact", Error = "error",
    Result = "result", RateLimit = "rate_limit",
});
tolerant_enum!(ApprovalStatus {
    // revise: "Ask for changes" on a draft.
    Pending = "pending", Approved = "approved", Denied = "denied", Expired = "expired", Revise = "revise",
});
tolerant_enum!(RuleDecision { Allow = "allow", Deny = "deny", Ask = "ask", Review = "review" });
tolerant_enum!(Role { User = "user", Assistant = "assistant", System = "system" });
tolerant_enum!(DealStage {
    New = "new", Researching = "researching", Contacted = "contacted", Replied = "replied", Meeting = "meeting",
    Proposal = "proposal", Won = "won", Lost = "lost",
});
tolerant_enum!(ActivityKind {
    Note = "note", Research = "research", EmailSent = "email_sent", EmailReceived = "email_received",
    DmSent = "dm_sent", DmReceived = "dm_received", Post = "post", Call = "call", Meeting = "meeting",
    StageChange = "stage_change",
});

impl RunStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::WaitingApproval)
    }
}

/// `bots.avatar` jsonb; every field optional (missing ones fall back to an id-derived default in the UI).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Avatar {
    pub shape: Option<u32>,
    pub color: Option<String>,
    pub eyes: Option<u32>,
    pub mouth: Option<u32>,
    pub accessory: Option<String>,
}

/// A bot. `status` / `last_run_at` are only present on rows from `/api/overview`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bot {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub persona: Option<String>,
    pub model: String,
    pub paused: bool,
    pub engine: BotEngine,
    pub avatar: Option<Avatar>,
    pub status: Option<BotStatus>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_dreamed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// What the template it was hired from set up (`None` for a teammate made from scratch).
    pub setup: Option<BotSetup>,
    /// It may use this PC's desktop (every step asks you).
    pub desktop: bool,
}

/// `bots.setup`: the template's sign-ins (ticked off by the owner) and the schedules it created (off at first).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BotSetup {
    pub template: Option<String>,
    pub logins: Vec<SetupLogin>,
    pub schedules: Vec<Uuid>,
    /// The template works on the owner's files: the checklist offers "Choose a folder" until one is shared.
    pub folder: bool,
    /// The template works on the desktop: the checklist offers to turn desktop control on.
    pub desktop: bool,
    /// The owner hid the Set up checklist.
    pub dismissed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SetupLogin {
    pub site: String,
    pub url: String,
    pub done: bool,
}

impl Bot {
    /// Status with `paused` winning, as the web UI shows it.
    pub fn effective_status(&self) -> BotStatus {
        if self.paused { BotStatus::Paused } else { self.status.unwrap_or(BotStatus::Idle) }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Thread {
    pub id: Uuid,
    pub bot_id: Uuid,
    pub title: Option<String>,
    pub claude_session_id: Option<Uuid>,
    pub schedule_id: Option<Uuid>,
    /// telegram | trigger | schedule | handoff | web | ...
    pub source: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Message {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub role: Role,
    pub content: String,
    pub run_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Run {
    pub id: Uuid,
    pub bot_id: Uuid,
    pub thread_id: Uuid,
    pub kind: RunKind,
    pub prompt: Option<String>,
    pub status: RunStatus,
    pub error: Option<String>,
    pub cost_usd: Option<f64>,
    pub usage: Option<Value>,
    pub parent_run_id: Option<Uuid>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Event {
    pub id: i64,
    pub run_id: Uuid,
    pub seq: i32,
    pub kind: EventKind,
    pub payload: Option<Value>,
    pub created_at: DateTime<Utc>,
}

// ---- typed event payloads ---------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolCall {
    pub id: Option<String>,
    pub name: String,
    pub input: Value,
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolResult {
    pub tool_use_id: Option<String>,
    /// Flattened to text (string content, or the text blocks of an array).
    pub content: String,
    pub is_error: bool,
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ApprovalEvent {
    pub approval_id: Option<Uuid>,
    pub tool_name: Option<String>,
    pub input: Option<Value>,
    pub status: Option<String>,
    pub decided_by: Option<String>,
    pub reason: Option<String>,
    /// The owner changed the input (or the draft) before approving.
    pub edited: bool,
    /// Sent as this approved draft, without asking again (`approval_id` is the send's own record).
    pub draft_id: Option<Uuid>,
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtifactEvent {
    pub artifact_id: Uuid,
    pub name: String,
    pub mime: String,
    pub bytes: u64,
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResultEvent {
    pub text: String,
    pub subtype: Option<String>,
    pub num_turns: Option<u32>,
    pub cost_usd: Option<f64>,
    pub duration_ms: Option<u64>,
}

/// An [`Event`] payload decoded by kind (see [`Event::typed`]).
#[derive(Debug, Clone, PartialEq)]
pub enum TypedEvent {
    Text(String),
    Thinking(String),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    Approval(ApprovalEvent),
    Artifact(ArtifactEvent),
    Error(String),
    Result(ResultEvent),
    /// status / rate_limit / unknown kinds: the raw payload.
    Other(EventKind, Value),
}

fn s(p: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| p.get(*k).and_then(Value::as_str)).map(str::to_string)
}

fn flatten_content(v: &Value) -> String {
    match v {
        Value::String(t) => t.clone(),
        Value::Array(a) => a
            .iter()
            .map(|b| b.get("text").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| flatten_content(b)))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        o => serde_json::to_string_pretty(o).unwrap_or_default(),
    }
}

impl Event {
    /// Decode the payload by kind, with the same fallbacks as the web `EventList`.
    pub fn typed(&self) -> TypedEvent {
        let null = Value::Null;
        let p = self.payload.as_ref().unwrap_or(&null);
        match self.kind {
            EventKind::Text => TypedEvent::Text(s(p, &["text", "content", "result"]).unwrap_or_else(|| flatten_content(p))),
            EventKind::Thinking => TypedEvent::Thinking(s(p, &["text", "thinking", "content"]).unwrap_or_else(|| flatten_content(p))),
            EventKind::ToolCall => TypedEvent::ToolCall(ToolCall {
                id: s(p, &["id"]),
                name: s(p, &["name", "tool_name"]).unwrap_or_else(|| "tool".into()),
                input: p.get("input").cloned().unwrap_or_else(|| p.clone()),
            }),
            EventKind::ToolResult => TypedEvent::ToolResult(ToolResult {
                tool_use_id: s(p, &["tool_use_id"]),
                content: flatten_content(p.get("content").or_else(|| p.get("output")).unwrap_or(p)),
                is_error: p.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            }),
            EventKind::Approval => TypedEvent::Approval(ApprovalEvent {
                approval_id: s(p, &["approval_id"]).and_then(|x| x.parse().ok()),
                tool_name: s(p, &["tool_name", "name"]),
                input: p.get("input").cloned(),
                status: s(p, &["status", "decision"]),
                decided_by: s(p, &["decided_by"]),
                reason: s(p, &["reason"]),
                edited: p.get("edited").and_then(Value::as_bool).unwrap_or(false),
                draft_id: s(p, &["draft_id"]).and_then(|x| x.parse().ok()),
            }),
            EventKind::Artifact => match s(p, &["artifact_id", "id"]).and_then(|x| x.parse().ok()) {
                Some(artifact_id) => TypedEvent::Artifact(ArtifactEvent {
                    artifact_id,
                    name: s(p, &["name"]).unwrap_or_else(|| "file".into()),
                    mime: s(p, &["mime"]).unwrap_or_default(),
                    bytes: p.get("bytes").and_then(Value::as_u64).unwrap_or(0),
                }),
                None => TypedEvent::Other(self.kind, p.clone()),
            },
            EventKind::Error => TypedEvent::Error(s(p, &["message", "error", "text"]).unwrap_or_else(|| flatten_content(p))),
            EventKind::Result => TypedEvent::Result(ResultEvent {
                text: s(p, &["text", "content", "result"]).unwrap_or_default(),
                subtype: s(p, &["subtype"]),
                num_turns: p.get("num_turns").and_then(Value::as_u64).map(|n| n as u32),
                cost_usd: p.get("cost_usd").and_then(Value::as_f64),
                duration_ms: p.get("duration_ms").and_then(Value::as_u64),
            }),
            k => TypedEvent::Other(k, p.clone()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Approval {
    pub id: Uuid,
    pub run_id: Uuid,
    pub bot_id: Uuid,
    pub tool_use_id: Option<String>,
    pub tool_name: String,
    pub input: Option<Value>,
    pub reason: Option<String>,
    pub status: ApprovalStatus,
    /// user | rule | reviewer
    pub decided_by: Option<String>,
    pub response: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub bot_name: Option<String>,
    /// Input fields the owner may rewrite before approving ("Edit & approve"; a draft's body, subject and to).
    pub editable: Vec<String>,
    /// The rule "Always allow this" adds for the teammate; None = not offered.
    pub allow_rule: Option<String>,
    /// What the owner approved when they changed it (`input` keeps the proposal).
    pub edited_input: Option<Value>,
    /// A desktop step with a picture of the screen around its target (`/api/approvals/{id}/preview`, while waiting).
    pub has_preview: bool,
    /// A draft whose follow-up run was given its approved text: that run may send it once, exactly as approved,
    /// without asking again.
    pub send_granted: bool,
    /// For a call Familiar let through as an approved draft's one send ("Sent as approved (draft #…)", `decided_by`
    /// rule): that draft's id.
    pub draft_id: Option<Uuid>,
}

impl Approval {
    /// A teammate's draft (`propose_draft`): a post, reply, email, DM or comment waiting for the owner.
    pub fn is_draft(&self) -> bool {
        self.tool_name == "propose_draft"
    }

    /// The send of an approved draft that went through without asking again (see [`Approval::draft_id`]).
    pub fn sent_as_approved(&self) -> bool {
        self.draft_id.is_some()
    }
}

/// `POST /api/approvals/{id}`'s answer: the decided approval, and the rule "Always allow" added (or found).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Decided {
    #[serde(flatten)]
    pub approval: Approval,
    pub rule: Option<Rule>,
}

/// A folder on this PC shared with a teammate (`/api/bots/{id}/folders`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Folder {
    pub id: Uuid,
    pub bot_id: Uuid,
    /// As Familiar resolved it when it was shared.
    pub path: String,
    /// read (read only) | write (read & write)
    pub mode: String,
    pub created_at: DateTime<Utc>,
}

impl Folder {
    pub fn writable(&self) -> bool {
        self.mode == "write"
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub id: Uuid,
    pub bot_id: Option<Uuid>,
    pub pattern: String,
    pub decision: RuleDecision,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub id: Uuid,
    pub bot_id: Uuid,
    pub thread_id: Option<Uuid>,
    pub cron: String,
    pub prompt: String,
    pub kind: RunKind,
    pub enabled: bool,
    pub gate_command: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    // Only from `GET /api/schedules` (every teammate's schedules):
    /// Its name: the title of its thread.
    pub label: Option<String>,
    pub bot_name: Option<String>,
    /// How its newest run went.
    pub last_status: Option<RunStatus>,
    pub last_error: Option<String>,
    pub last_finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Memory {
    pub id: Uuid,
    pub bot_id: Uuid,
    pub content: String,
    /// user | bot
    pub source: String,
    /// active | proposed | rejected
    pub status: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Skill {
    pub id: Uuid,
    pub bot_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub body: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Artifact {
    pub id: Uuid,
    pub run_id: Uuid,
    pub bot_id: Uuid,
    pub name: String,
    pub mime: String,
    pub bytes: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecretField {
    pub key: String,
    pub label: String,
    pub help: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectorPreset {
    pub id: String,
    pub name: String,
    pub description: String,
    /// stdio | http
    pub transport: String,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub url: Option<String>,
    pub secret_fields: Vec<SecretField>,
    pub docs_url: Option<String>,
    pub verify: bool,
}

/// A ready-made teammate from `GET /api/templates`. Answers to `questions` fill the `{{key}}` placeholders in
/// `instructions`, the schedule prompts and `first_task`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub category: String,
    pub summary: String,
    pub avatar: Option<Avatar>,
    /// A Claude alias (`sonnet`, ...).
    pub model: String,
    pub questions: Vec<TemplateQuestion>,
    pub instructions: String,
    pub schedules: Vec<TemplateSchedule>,
    pub logins: Vec<TemplateLogin>,
    /// Connector preset ids it benefits from.
    pub connectors: Vec<String>,
    pub first_task: Option<String>,
    /// It works on the owner's files: its Set up checklist asks for a folder.
    pub folder: bool,
    /// It works on the desktop: its Set up checklist offers to turn desktop control on.
    pub desktop: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TemplateQuestion {
    pub key: String,
    pub label: String,
    pub placeholder: String,
    pub multiline: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TemplateSchedule {
    pub label: String,
    /// 5-field cron in the owner's local time.
    pub cron: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TemplateLogin {
    pub site: String,
    pub url: String,
}

/// Response of `POST /api/templates/{id}/create`: the new teammate and its first task, filled in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hired {
    pub bot: Bot,
    pub first_task: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Connector {
    pub id: Uuid,
    pub name: String,
    pub preset: Option<String>,
    pub transport: String,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub url: Option<String>,
    pub env_names: Vec<String>,
    pub header_names: Vec<String>,
    /// The names (never the values) of its stored secrets, as the API reports them.
    pub secret_names: SecretNames,
    pub has_secrets: bool,
    pub enabled: bool,
    pub created_at: Option<DateTime<Utc>>,
}

/// `secret_names` of a connector: which environment variables (stdio) and headers (http) are stored.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecretNames {
    pub env: Vec<String>,
    pub headers: Vec<String>,
}

impl Connector {
    /// The stored secrets' names: `secret_names` (what the API sends), else the older `env_names` / `header_names`.
    pub fn stored_secret_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.secret_names.env.iter().chain(&self.secret_names.headers).cloned().collect();
        if names.is_empty() {
            names = self.env_names.iter().chain(&self.header_names).cloned().collect();
        }
        names
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Channel {
    pub id: Uuid,
    pub kind: String,
    pub bound: bool,
    pub pair_code: Option<String>,
    pub default_bot_id: Option<Uuid>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Trigger {
    pub id: Uuid,
    pub bot_id: Uuid,
    pub name: String,
    pub prompt: String,
    pub kind: String,
    pub enabled: bool,
    pub last_fired_at: Option<DateTime<Utc>>,
    pub created_at: Option<DateTime<Utc>>,
    /// Webhook URL (only on create / rotate).
    pub url: Option<String>,
}

// ---- CRM (`/api/crm/...`) ------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrmCompany {
    pub id: Uuid,
    pub name: String,
    /// Lowercase host name, e.g. `acme.com`.
    pub domain: Option<String>,
    pub website: Option<String>,
    pub industry: Option<String>,
    pub size: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    /// 0-100: how good a lead it is.
    pub fit_score: Option<i32>,
    /// A real reason to contact them.
    pub fit_reason: Option<String>,
    pub tags: Vec<String>,
    /// Where the facts came from.
    pub source_urls: Vec<String>,
    pub custom: Value,
    /// The teammate that added it (None: the owner).
    pub created_by_bot: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrmContact {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    /// Joined from the company.
    pub company_name: Option<String>,
    pub company_domain: Option<String>,
    pub name: String,
    pub title: Option<String>,
    /// Lowercase.
    pub email: Option<String>,
    pub linkedin_url: Option<String>,
    pub x_handle: Option<String>,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    pub source_urls: Vec<String>,
    pub custom: Value,
    /// They asked not to be contacted: teammates must not write to them.
    pub do_not_contact: bool,
    pub dnc_reason: Option<String>,
    pub dnc_at: Option<DateTime<Utc>>,
    pub created_by_bot: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrmDeal {
    pub id: Uuid,
    pub company_id: Uuid,
    pub contact_id: Option<Uuid>,
    /// Joined from the company and the contact.
    pub company_name: Option<String>,
    pub company_domain: Option<String>,
    pub contact_name: Option<String>,
    pub contact_email: Option<String>,
    /// The contact asked not to be contacted.
    pub contact_do_not_contact: bool,
    pub title: String,
    pub stage: DealStage,
    pub stage_changed_at: DateTime<Utc>,
    pub value_cents: Option<i64>,
    pub currency: String,
    pub next_step: Option<String>,
    pub next_step_at: Option<DateTime<Utc>>,
    pub created_by_bot: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One entry of a company's, contact's or deal's timeline.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrmActivity {
    pub id: Uuid,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub deal_id: Option<Uuid>,
    pub kind: ActivityKind,
    pub summary: String,
    pub body: Option<String>,
    pub url: Option<String>,
    /// The approved draft this came from (an email or DM that was sent).
    pub approval_id: Option<Uuid>,
    pub bot_id: Option<Uuid>,
    pub bot_name: Option<String>,
    /// bot | user
    pub actor_kind: String,
    pub occurred_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// A write to a company, contact or deal (or a timeline entry): the row before and after.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrmChange {
    pub id: Uuid,
    /// company | contact | deal | activity
    pub entity: String,
    pub entity_id: Uuid,
    /// create | update | delete | undo
    pub op: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
    /// bot | user
    pub actor_kind: String,
    pub bot_id: Option<Uuid>,
    pub bot_name: Option<String>,
    pub run_id: Option<Uuid>,
    pub at: DateTime<Utc>,
    /// Set once this change was undone.
    pub undone_at: Option<DateTime<Utc>>,
}

/// A deal on the pipeline board.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PipelineDeal {
    pub id: Uuid,
    pub stage: DealStage,
    pub title: String,
    pub company_id: Uuid,
    pub company_name: Option<String>,
    pub contact_id: Option<Uuid>,
    pub contact_name: Option<String>,
    /// The contact asked not to be contacted.
    pub contact_do_not_contact: bool,
    pub stage_changed_at: Option<DateTime<Utc>>,
    pub value_cents: Option<i64>,
    pub currency: String,
    pub next_step: Option<String>,
    pub next_step_at: Option<DateTime<Utc>>,
}

/// One column of the pipeline board: every stage is present, in order. `count` and `value_cents` cover all the stage's
/// deals; `deals` holds the newest 100.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PipelineStage {
    pub stage: DealStage,
    pub count: i64,
    pub value_cents: i64,
    pub deals: Vec<PipelineDeal>,
}

/// What a CSV import did (or, as a dry run, would do).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrmImportResult {
    pub created: u32,
    pub updated: u32,
    /// Blank rows, and rows whose record already has all of it.
    pub skipped: u32,
    pub errors: Vec<CrmImportError>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrmImportError {
    /// The row in the file (the header is row 1).
    pub row: u32,
    pub message: String,
}

/// An outgoing CRM webhook: every change in `events` is POSTed to `url`, signed with the webhook's secret
/// (`Familiar-Signature: t=<unix>,v1=<hex HMAC-SHA256 of "<t>.<body>">`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrmWebhook {
    pub id: Uuid,
    pub url: String,
    /// company.created | company.updated | contact.created | contact.updated | contact.do_not_contact | deal.created |
    /// deal.updated | deal.stage_changed | activity.created
    pub events: Vec<String>,
    pub enabled: bool,
    pub created_at: Option<DateTime<Utc>>,
    /// The signing secret: only in the answer to create. Store it then; it is never shown again.
    pub secret: Option<String>,
    /// The newest delivery, if any.
    pub last_delivery: Option<CrmWebhookDelivery>,
}

/// One delivery of an event to a webhook (a `ping` for the Test button).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrmWebhookDelivery {
    /// Also the `Familiar-Delivery` header and the payload's `id`.
    pub id: Uuid,
    pub webhook_id: Option<Uuid>,
    pub event: String,
    /// `{id, event, at, data, previous?, actor: {kind, bot_id?, bot_slug?}}` (not in a webhook's `last_delivery`).
    pub payload: Option<Value>,
    /// pending | delivered | failed
    pub status: String,
    pub attempts: i32,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
}

/// Teammates hired together (`GET /api/templates/bundles`), from one set of shared `questions`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TemplateBundle {
    pub id: String,
    pub name: String,
    pub summary: String,
    /// Template ids, in hiring order.
    pub templates: Vec<String>,
    pub questions: Vec<TemplateQuestion>,
}

/// Response of `POST /api/templates/bundles/{id}/create`: every new teammate, in the bundle's order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BundleHired {
    pub hired: Vec<Hired>,
}

/// List filters for companies, contacts and deals; a filter that doesn't apply to the kind is ignored.
#[derive(Debug, Clone, Default)]
pub struct CrmListParams {
    /// Text to look for.
    pub q: Option<String>,
    pub tag: Option<String>,
    /// Deals: new | researching | contacted | replied | meeting | proposal | won | lost.
    pub stage: Option<String>,
    /// Contacts and deals.
    pub company_id: Option<Uuid>,
    /// Contacts: only those (not) marked do-not-contact.
    pub dnc: Option<bool>,
    /// updated (default) | name | fit.
    pub sort: Option<String>,
    /// At most 200 (default 50).
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// Timeline filters; newest first.
#[derive(Debug, Clone, Default)]
pub struct CrmActivityParams {
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub deal_id: Option<Uuid>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// Change-log filters; newest first.
#[derive(Debug, Clone, Default)]
pub struct CrmChangeParams {
    /// company | contact | deal | activity
    pub entity: Option<String>,
    pub entity_id: Option<Uuid>,
    pub bot_id: Option<Uuid>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceInfo {
    pub utilization: Option<f64>,
    /// Unix seconds, unix millis or an RFC 3339 string; see [`DeviceInfo::resets_at_utc`].
    pub resets_at: Option<Value>,
    pub throttled: bool,
    pub active_runs: Option<u32>,
    pub claude_version: Option<String>,
}

impl DeviceInfo {
    pub fn resets_at_utc(&self) -> Option<DateTime<Utc>> {
        match self.resets_at.as_ref()? {
            Value::Number(n) => {
                let v = n.as_f64()?;
                let ms = if v < 1e12 { v * 1000.0 } else { v };
                DateTime::from_timestamp_millis(ms as i64)
            }
            Value::String(t) => DateTime::parse_from_rfc3339(t).ok().map(|d| d.with_timezone(&Utc)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Device {
    pub id: Uuid,
    pub name: Option<String>,
    pub version: Option<String>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub online: Option<bool>,
    pub info: Option<DeviceInfo>,
}

impl Device {
    /// Server-provided `online`, else seen within 3 minutes.
    pub fn is_online(&self, now: DateTime<Utc>) -> bool {
        self.online.unwrap_or_else(|| self.last_seen_at.is_some_and(|t| now - t < chrono::Duration::minutes(3)))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overview {
    pub bots: Vec<Bot>,
    /// A count or a list depending on server version; see [`Overview::pending_count`].
    pub pending_approvals: Value,
    pub devices: Vec<Device>,
}

impl Overview {
    pub fn pending_count(&self) -> usize {
        match &self.pending_approvals {
            Value::Array(a) => a.len(),
            Value::Number(n) => n.as_u64().unwrap_or(0) as usize,
            _ => 0,
        }
    }
    pub fn pc_online(&self, now: DateTime<Utc>) -> bool {
        self.devices.iter().any(|d| d.is_online(now))
    }
    pub fn last_seen(&self) -> Option<DateTime<Utc>> {
        self.devices.iter().filter_map(|d| d.last_seen_at).max()
    }
    /// First device info that is throttled or reports utilisation.
    pub fn device_info(&self) -> Option<&DeviceInfo> {
        self.devices.iter().filter_map(|d| d.info.as_ref()).find(|i| i.throttled || i.utilization.is_some())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LiveInfo {
    pub url: String,
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthState {
    pub setup_needed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct User {
    pub id: Uuid,
    pub email: String,
}

/// Response of setup / login.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub token: String,
    pub user: User,
}

// ---- request bodies ----------------------------------------------------

macro_rules! body {
    ($(#[$m:meta])* $name:ident { $($f:ident : $t:ty),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Default, Serialize)]
        pub struct $name { $(#[serde(skip_serializing_if = "Option::is_none")] pub $f: Option<$t>,)* }
    };
}

macro_rules! secret_body {
    ($(#[$m:meta])* $name:ident { $($f:ident : $t:ty),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Default, Serialize)]
        pub struct $name { $(#[serde(skip_serializing_if = "Option::is_none")] pub $f: Option<$t>,)* }
        /// Carries a secret: `{:?}` says which fields are set, never their values.
        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($name))
                    $(.field(stringify!($f), &if self.$f.is_some() { "<set>" } else { "<unset>" }))*
                    .finish()
            }
        }
    };
}

body!(NewBot { name: String, slug: String, persona: String, model: String, engine: String, avatar: Avatar });
body!(
    /// `avatar: Some(Value::Null)` clears the avatar. `desktop`: "Can use this PC's desktop".
    BotPatch { name: String, persona: String, model: String, paused: bool, engine: String, avatar: Value, desktop: bool }
);
body!(NewSchedule { cron: String, prompt: String, kind: String, enabled: bool, gate_command: String });
body!(SchedulePatch { label: String, cron: String, prompt: String, kind: String, enabled: bool, gate_command: String });
body!(
    /// An owner's decision. `decision`: approve | deny | revise (drafts: "Ask for changes"). `response`: the answer to
    /// a question, or the note on a denial / request for changes. `edits`: new text for the approval's `editable`
    /// fields. `always`: approve and add its `allow_rule`.
    ApprovalDecision {
        decision: String, response: String, edits: std::collections::BTreeMap<String, String>, always: bool,
    }
);
body!(NewRule { bot_id: Uuid, pattern: String, decision: String, note: String });
body!(MemoryPatch { content: String, status: String });
body!(NewTrigger { name: String, prompt: String, kind: String });
body!(TriggerPatch { name: String, prompt: String, kind: String, enabled: bool });
secret_body!(NewChannel { kind: String, token: String, default_bot_id: Uuid });
secret_body!(ChannelPatch { token: String, default_bot_id: Uuid, enabled: bool });
secret_body!(AccountUpdate { current_password: String, email: String, new_password: String });
body!(
    /// Hire from a template; unset fields take the template's.
    FromTemplate {
        answers: std::collections::BTreeMap<String, String>, name: String, instructions: String, engine: String,
        model: String, avatar: Avatar,
    }
);
body!(
    /// Tick a template login (`done` defaults to true) and/or hide the Set up checklist.
    SetupPatch { login: String, done: bool, dismissed: bool }
);

body!(
    /// Create a company, or update the one with the same domain (else the same name). An empty text clears a field.
    NewCompany {
        name: String, domain: String, website: String, industry: String, size: String, location: String,
        description: String, fit_score: i32, fit_reason: String, tags: Vec<String>, source_urls: Vec<String>, custom: Value,
    }
);
body!(
    /// Change a company: unset fields stay as they are; an empty text clears a text field. `fit_score`: `Some(Some(n))`
    /// sets it, `Some(None)` clears it (sent as `null`).
    CompanyPatch {
        name: String, domain: String, website: String, industry: String, size: String, location: String,
        description: String, fit_score: Option<i32>, fit_reason: String, tags: Vec<String>, source_urls: Vec<String>,
        custom: Value,
    }
);
body!(
    /// Create a contact, or update the one with the same email (else LinkedIn link, else name at the company).
    NewContact {
        company_id: Uuid, name: String, title: String, email: String, linkedin_url: String, x_handle: String,
        notes: String, tags: Vec<String>, source_urls: Vec<String>, custom: Value, do_not_contact: bool,
        dnc_reason: String,
    }
);
body!(ContactPatch {
    company_id: Uuid, name: String, title: String, email: String, linkedin_url: String, x_handle: String,
    notes: String, tags: Vec<String>, source_urls: Vec<String>, custom: Value, do_not_contact: bool,
    dnc_reason: String,
});
body!(
    /// Create a deal, or update the one at the same company with the same title. `stage` defaults to new.
    NewDeal {
        company_id: Uuid, contact_id: Uuid, title: String, stage: String, value_cents: i64, currency: String,
        next_step: String, next_step_at: DateTime<Utc>,
    }
);
body!(
    /// Change a deal: unset fields stay as they are; an empty text clears a text field. `value_cents` and
    /// `next_step_at`: `Some(Some(v))` sets, `Some(None)` clears (sent as `null`).
    DealPatch {
        company_id: Uuid, contact_id: Uuid, title: String, stage: String, value_cents: Option<i64>, currency: String,
        next_step: String, next_step_at: Option<DateTime<Utc>>,
    }
);
body!(
    /// A timeline entry; give at least one of `company_id`, `contact_id`, `deal_id`.
    NewActivity {
        company_id: Uuid, contact_id: Uuid, deal_id: Uuid, kind: String, summary: String, body: String, url: String,
        approval_id: Uuid, occurred_at: DateTime<Utc>,
    }
);

body!(
    /// A webhook: an `https` URL (or `http` to this computer) and the events it gets. `enabled` defaults to true.
    NewCrmWebhook { url: String, events: Vec<String>, enabled: bool }
);
body!(
    /// Turning a webhook off fails its deliveries still waiting.
    CrmWebhookPatch { url: String, events: Vec<String>, enabled: bool }
);
body!(
    /// Hire a bundle: the shared answers by question key.
    FromBundle { answers: std::collections::BTreeMap<String, String> }
);

/// Secrets for a connector: env vars (stdio) and/or headers (http). Write-only.
#[derive(Clone, Default, Serialize)]
pub struct ConnectorSecrets {
    pub env: std::collections::BTreeMap<String, String>,
    pub headers: std::collections::BTreeMap<String, String>,
}

/// `{:?}` names the secrets, never their values.
impl std::fmt::Debug for ConnectorSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectorSecrets").field("env", &self.env.keys().collect::<Vec<_>>()).field("headers", &self.headers.keys().collect::<Vec<_>>()).finish()
    }
}
body!(NewConnector {
    name: String, preset: String, transport: String, command: String, args: Vec<String>,
    url: String, secrets: ConnectorSecrets, enabled: bool,
});
body!(ConnectorPatch {
    name: String, preset: String, transport: String, command: String, args: Vec<String>,
    url: String, secrets: ConnectorSecrets, enabled: bool,
});

/// Input to the live browser view (`type` is click | type | key | scroll | navigate).
#[derive(Debug, Clone, Default, Serialize)]
pub struct LiveInput {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dy: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl LiveInput {
    pub fn click(x: i64, y: i64) -> Self {
        Self { kind: "click".into(), x: Some(x), y: Some(y), ..Default::default() }
    }
    pub fn type_text(t: impl Into<String>) -> Self {
        Self { kind: "type".into(), text: Some(t.into()), ..Default::default() }
    }
    pub fn key(k: impl Into<String>) -> Self {
        Self { kind: "key".into(), key: Some(k.into()), ..Default::default() }
    }
    pub fn scroll(dy: i64) -> Self {
        Self { kind: "scroll".into(), dy: Some(dy), ..Default::default() }
    }
    pub fn navigate(u: impl Into<String>) -> Self {
        Self { kind: "navigate".into(), url: Some(u.into()), ..Default::default() }
    }
}
