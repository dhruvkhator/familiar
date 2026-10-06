//! Serde of realistic rows (shapes copied from the server API tests / web types).

use chrono::Utc;
use familiar_client::*;
use serde_json::json;

fn de<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> T {
    serde_json::from_value(v).unwrap()
}

#[test]
fn bot_overview_row_with_unknowns() {
    let b: Bot = de(json!({
        "id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "slug": "research-buddy", "name": "Research Buddy",
        "persona": null, "model": "sonnet", "engine": "claude", "paused": false,
        "avatar": {"shape": 2, "color": "#7285d5", "accessory": "hat", "future_field": 1},
        "status": "running", "last_run_at": "2026-10-02T10:15:30.123456+00:00",
        "last_dreamed_at": null, "created_at": "2026-10-01T08:00:00Z", "brand_new_column": [1, 2]
    }));
    assert_eq!(b.name, "Research Buddy");
    assert_eq!(b.engine, BotEngine::Claude);
    assert_eq!(b.effective_status(), BotStatus::Running);
    assert_eq!(b.avatar.unwrap().accessory.as_deref(), Some("hat"));
    assert!(b.last_run_at.is_some());
    let paused: Bot = de(json!({"id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "paused": true, "status": "idle", "engine": "gemini"}));
    assert_eq!(paused.effective_status(), BotStatus::Paused);
    assert_eq!(paused.engine, BotEngine::Unknown);
}

#[test]
fn sparse_rows_do_not_fail() {
    let r: Run = de(json!({"id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "status": "waiting_approval", "kind": "weird"}));
    assert!(r.status.is_active());
    assert_eq!(r.kind, RunKind::Unknown);
    let _: Thread = de(json!({}));
    let _: Message = de(json!({"role": "assistant", "content": "hi"}));
    let a: Approval = de(json!({"tool_name": "Bash", "input": {"command": "ls"}, "status": "pending", "bot_name": "B"}));
    assert_eq!(a.status, ApprovalStatus::Pending);
    assert_eq!(a.bot_name.as_deref(), Some("B"));
}

#[test]
fn event_payloads() {
    let ev = |kind: &str, payload: serde_json::Value| -> Event {
        de(json!({"id": 1, "run_id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "seq": 3, "kind": kind, "payload": payload, "created_at": "2026-10-02T10:00:00Z"}))
    };
    assert_eq!(ev("text", json!({"text": "hello"})).typed(), TypedEvent::Text("hello".into()));
    assert_eq!(ev("thinking", json!({"thinking": "hmm"})).typed(), TypedEvent::Thinking("hmm".into()));
    let TypedEvent::ToolCall(c) = ev("tool_call", json!({"id": "t1", "name": "Bash", "input": {"command": "ls"}})).typed() else { panic!() };
    assert_eq!((c.name.as_str(), c.input["command"].as_str()), ("Bash", Some("ls")));
    let TypedEvent::ToolResult(r) = ev("tool_result", json!({"tool_use_id": "t1", "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}], "is_error": true})).typed() else { panic!() };
    assert_eq!((r.content.as_str(), r.is_error), ("a\nb", true));
    let TypedEvent::Approval(a) = ev("approval", json!({"approval_id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "tool_name": "Write", "status": "pending", "reason": "x"})).typed() else { panic!() };
    assert!(a.approval_id.is_some() && a.status.as_deref() == Some("pending"));
    let TypedEvent::Artifact(f) = ev("artifact", json!({"artifact_id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "name": "r.md", "mime": "text/markdown", "bytes": 12})).typed() else { panic!() };
    assert_eq!((f.name.as_str(), f.bytes), ("r.md", 12));
    assert_eq!(ev("error", json!({"message": "boom"})).typed(), TypedEvent::Error("boom".into()));
    let TypedEvent::Result(x) = ev("result", json!({"text": "done", "subtype": "success", "num_turns": 4, "cost_usd": 0.12, "duration_ms": 900, "permission_denials": []})).typed() else { panic!() };
    assert_eq!((x.text.as_str(), x.num_turns, x.duration_ms), ("done", Some(4), Some(900)));
    assert!(matches!(ev("rate_limit", json!({"x": 1})).typed(), TypedEvent::Other(EventKind::RateLimit, _)));
    assert!(matches!(ev("status", serde_json::Value::Null).typed(), TypedEvent::Other(EventKind::Status, _)));
}

#[test]
fn overview_devices_and_throttle() {
    let o: Overview = de(json!({
        "bots": [{"id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "name": "a", "status": "idle"}],
        "pending_approvals": 2,
        "devices": [{"id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "name": "pc", "last_seen_at": "2026-10-02T10:00:00Z", "online": true,
                     "info": {"utilization": 0.8, "throttled": true, "resets_at": 1790000000, "active_runs": 1}}]
    }));
    assert_eq!(o.pending_count(), 2);
    assert!(o.pc_online(Utc::now()));
    let info = o.device_info().unwrap();
    assert!(info.throttled);
    assert_eq!(info.resets_at_utc().unwrap().timestamp(), 1790000000);
    let o2: Overview = de(json!({"bots": [], "pending_approvals": [{}, {}, {}], "devices": []}));
    assert_eq!(o2.pending_count(), 3);
}

#[test]
fn misc_rows() {
    let c: Connector = de(json!({"id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "name": "gh", "preset": "github", "transport": "stdio", "env_names": ["TOKEN"], "has_secrets": true, "enabled": true}));
    assert_eq!(c.env_names, ["TOKEN"]);
    let p: ConnectorPreset = de(json!({"id": "github", "name": "GitHub", "description": "d", "transport": "http", "secret_fields": [{"key": "T", "label": "Token"}]}));
    assert_eq!(p.secret_fields[0].label, "Token");
    let t: Trigger = de(json!({"id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "name": "n", "kind": "chat", "enabled": true, "url": "http://x/hooks/abc"}));
    assert_eq!(t.url.as_deref(), Some("http://x/hooks/abc"));
    let s: Session = de(json!({"token": "tok", "user": {"id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "email": "a@b.co"}}));
    assert_eq!(s.user.email, "a@b.co");
    let l: LiveInfo = de(json!({"url": "https://x", "title": "t", "width": 1280, "height": 800, "updated_at": "2026-10-02T10:00:00Z"}));
    assert_eq!(l.width, 1280);
}

#[test]
fn templates_and_bot_setup() {
    let t: Template = de(json!({
        "id": "lead-researcher", "name": "Lead researcher", "category": "Sales", "summary": "s",
        "avatar": {"shape": 2, "color": "#4fa9cf", "eyes": 3, "mouth": 0, "accessory": "glasses"}, "model": "sonnet",
        "questions": [{"key": "icp", "label": "Your ideal customer", "placeholder": "B2B", "multiline": true}],
        "instructions": "Find {{icp}}", "schedules": [{"label": "Find", "cron": "0 8 * * 2,4", "prompt": "p"}],
        "logins": [{"site": "LinkedIn", "url": "https://www.linkedin.com/login"}], "connectors": ["brave-search"],
        "first_task": null, "future": 1
    }));
    assert_eq!((t.questions[0].key.as_str(), t.questions[0].multiline), ("icp", true));
    assert_eq!(t.avatar.unwrap().accessory.as_deref(), Some("glasses"));
    assert_eq!((t.schedules[0].cron.as_str(), t.logins[0].site.as_str(), t.first_task), ("0 8 * * 2,4", "LinkedIn", None));
    let h: Hired = de(json!({"bot": {"id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "name": "Scout", "setup": {
        "template": "lead-researcher", "logins": [{"site": "LinkedIn", "url": "https://x", "done": true}],
        "schedules": ["6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10"], "dismissed": false}}, "first_task": "go"}));
    let setup = h.bot.setup.unwrap();
    assert!(setup.logins[0].done && !setup.dismissed && setup.schedules.len() == 1);
    let plain: Bot = de(json!({"id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "setup": null}));
    assert!(plain.setup.is_none());
    let body = FromTemplate { name: Some("Scout".into()), answers: Some([("icp".to_owned(), "SaaS".to_owned())].into()), ..Default::default() };
    assert_eq!(serde_json::to_value(body).unwrap(), json!({"answers": {"icp": "SaaS"}, "name": "Scout"}));
    assert_eq!(serde_json::to_value(SetupPatch { dismissed: Some(true), ..Default::default() }).unwrap(), json!({"dismissed": true}));
}

#[test]
fn request_bodies_skip_unset() {
    assert_eq!(serde_json::to_value(NewBot { name: Some("x".into()), ..Default::default() }).unwrap(), json!({"name": "x"}));
    assert_eq!(serde_json::to_value(BotPatch { avatar: Some(serde_json::Value::Null), ..Default::default() }).unwrap(), json!({"avatar": null}));
    assert_eq!(serde_json::to_value(LiveInput::click(3, 4)).unwrap(), json!({"type": "click", "x": 3, "y": 4}));
}

#[test]
fn drafts_offers_and_decisions() {
    let a: Approval = de(json!({
        "id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "tool_name": "propose_draft", "status": "revise",
        "input": {"kind": "post", "channel": "X", "body": "Hi"}, "editable": ["body", "subject", "to"],
        "allow_rule": null, "edited_input": {"kind": "post", "channel": "X", "body": "Hello"}, "response": "warmer"
    }));
    assert!(a.is_draft());
    assert_eq!(a.status, ApprovalStatus::Revise);
    assert_eq!(a.editable, ["body", "subject", "to"]);
    assert_eq!(a.edited_input.unwrap()["body"], "Hello");
    // The decide answer: the approval, plus the rule "Always allow" added.
    let d: Decided = de(json!({
        "id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "tool_name": "Bash", "status": "approved", "allow_rule": "Bash(git status)",
        "rule": {"id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f11", "pattern": "Bash(git status)", "decision": "allow"}
    }));
    assert_eq!(d.approval.status, ApprovalStatus::Approved);
    assert!(!d.approval.is_draft());
    assert_eq!(d.rule.map(|r| (r.pattern, r.decision)), Some(("Bash(git status)".to_owned(), RuleDecision::Allow)));
    let plain: Decided = de(json!({"tool_name": "Bash", "status": "denied"}));
    assert!(plain.rule.is_none());

    let body = serde_json::to_value(ApprovalDecision {
        decision: Some("approve".into()),
        edits: Some([("body".to_owned(), "Hello".to_owned())].into()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(body, json!({"decision": "approve", "edits": {"body": "Hello"}}));

    let ev: Event = de(json!({"id": 1, "seq": 2, "kind": "approval",
        "payload": {"tool_name": "propose_draft", "status": "approved", "decided_by": "user", "edited": true}}));
    let TypedEvent::Approval(e) = ev.typed() else { panic!() };
    assert!(e.edited);
}

#[test]
fn schedules_page_rows() {
    let s: Schedule = de(json!({
        "id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f10", "bot_id": "6f1d1c1e-8a52-4a69-9d4e-0a5b6d8b9f11", "cron": "0 9 * * 1-5",
        "prompt": "standup", "kind": "scheduled", "enabled": true, "label": "Morning standup", "bot_name": "Ada",
        "bot_slug": "ada", "last_status": "succeeded", "last_error": null, "last_finished_at": "2026-10-02T10:00:00Z"
    }));
    assert_eq!((s.label.as_deref(), s.bot_name.as_deref()), (Some("Morning standup"), Some("Ada")));
    assert_eq!(s.last_status, Some(RunStatus::Succeeded));
    assert!(s.last_finished_at.is_some());
    let patch = serde_json::to_value(SchedulePatch { label: Some("Standup".into()), ..Default::default() }).unwrap();
    assert_eq!(patch, json!({"label": "Standup"}));
}
