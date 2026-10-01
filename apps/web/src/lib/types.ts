// Hand-written row types mirroring docs/ARCHITECTURE.md "Data model".
export type Uuid = string;
export type Json = unknown;

export type BotEngine = "claude" | "codex";
/** claude: sonnet | opus | haiku | fable; codex: free text (e.g. gpt-5-codex). */
export type BotModel = string;
export type RunKind = "chat" | "scheduled" | "proactive" | "handoff";
export type RunStatus = "queued" | "running" | "waiting_approval" | "succeeded" | "failed" | "cancelled";
export type EventKind =
  | "status" | "text" | "thinking" | "tool_call" | "tool_result"
  | "approval" | "artifact" | "error" | "result" | "rate_limit";
export type ApprovalStatus = "pending" | "approved" | "denied" | "expired";
export type RuleDecision = "allow" | "deny" | "ask" | "review";

export interface Bot {
  id: Uuid; slug: string; name: string; persona: string | null;
  model: BotModel; paused: boolean; created_at: string; last_dreamed_at?: string | null;
  engine?: BotEngine; avatar?: Partial<import("../components/Mascot").Avatar> | null;
}
/** Bot as returned by GET /api/overview (bot + derived status). */
export interface BotOverview extends Bot { status: "idle" | "running" | "paused"; last_run_at: string | null; }
export interface Overview { bots: BotOverview[]; pending_approvals: number | unknown[]; devices: Device[]; }
export interface Thread {
  id: Uuid; bot_id: Uuid; title: string | null; claude_session_id: Uuid | null;
  schedule_id: Uuid | null; created_at: string; updated_at: string;
  source?: "telegram" | "trigger" | "schedule" | "handoff" | "web" | string | null;
}
export interface Message {
  id: Uuid; thread_id: Uuid; role: "user" | "assistant" | "system";
  content: string; run_id: Uuid | null; created_at: string;
}
export interface Run {
  id: Uuid; bot_id: Uuid; thread_id: Uuid; kind: RunKind; prompt: string | null;
  status: RunStatus; error: string | null; cost_usd: number | null; usage: Json | null;
  parent_run_id: Uuid | null; started_at: string | null; finished_at: string | null; created_at: string;
}
export interface RunEvent {
  id: number; run_id: Uuid; seq: number; kind: EventKind;
  payload: Record<string, unknown> | null; created_at: string;
}
export interface Approval {
  id: Uuid; run_id: Uuid; bot_id: Uuid; tool_use_id: string | null; tool_name: string;
  input: Record<string, unknown> | null; reason: string | null; status: ApprovalStatus;
  decided_by: "user" | "rule" | "reviewer" | null; response: string | null;
  decided_at: string | null; created_at: string; bot_name?: string | null;
}
export interface Rule {
  id: Uuid; bot_id: Uuid | null; pattern: string; decision: RuleDecision;
  note: string | null; created_at: string;
}
export interface Schedule {
  id: Uuid; bot_id: Uuid; thread_id: Uuid | null; cron: string; prompt: string;
  kind: "scheduled" | "proactive"; enabled: boolean; gate_command?: string | null;
  last_run_at: string | null; next_run_at: string | null;
}
export interface Memory {
  id: Uuid; bot_id: Uuid; content: string; source: "user" | "bot"; created_at: string;
  status?: "active" | "proposed" | "rejected";
}
export interface DeviceInfo {
  utilization?: number | null; resets_at?: string | number | null; throttled?: boolean;
  active_runs?: number; claude_version?: string;
}
export interface Device {
  id: Uuid; name: string | null; version: string | null; last_seen_at: string | null; online?: boolean;
  info?: DeviceInfo | null;
}
export interface Artifact {
  id: Uuid; run_id: Uuid; bot_id: Uuid; name: string; mime: string; bytes: number; created_at: string;
}
export interface Skill { id: Uuid; bot_id: Uuid; name: string; description: string | null; body: string; updated_at: string; }
export interface SecretField { key: string; label: string; help?: string | null }
export interface ConnectorPreset {
  id: string; name: string; description: string; transport: "stdio" | "http";
  command?: string | null; args?: string[] | null; url?: string | null;
  secret_fields: SecretField[]; docs_url?: string | null; verify?: boolean;
}
export interface Connector {
  id: Uuid; name: string; preset?: string | null; transport: "stdio" | "http";
  command?: string | null; args?: string[] | null; url?: string | null;
  env_names?: string[]; header_names?: string[]; has_secrets?: boolean; enabled: boolean; created_at?: string;
}
export interface Channel {
  id: Uuid; kind: string; bound: boolean; pair_code?: string | null; default_bot_id: Uuid | null; enabled: boolean;
}
export interface Trigger {
  id: Uuid; bot_id: Uuid; name: string; prompt: string; kind: "chat" | "scheduled" | "proactive" | string;
  enabled: boolean; last_fired_at: string | null; created_at?: string; url?: string;
}
export interface LiveInfo { url: string; title: string; width: number; height: number; updated_at: string }
