import { isTauri } from "./api";

export interface AppStatus {
  boot: { phase: "starting" | "database" | "ready" | "error"; message: string; embedded?: boolean; database_url?: string };
  daemon: { running: boolean; error?: string | null; config_path?: string };
}
export interface CliStatus { installed: boolean; version?: string | null; logged_in: boolean; auth_method?: string | null }
export type ClaudeStatus = CliStatus;

export { isTauri };

export async function invokeCmd<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

/** Like invokeCmd, but a missing or failing command reads as "not installed". */
export async function cliStatus(cmd: "claude_status" | "codex_status"): Promise<CliStatus> {
  try { return await invokeCmd<CliStatus>(cmd); } catch { return { installed: false, logged_in: false }; }
}
