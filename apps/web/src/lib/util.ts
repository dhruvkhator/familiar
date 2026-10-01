export function ago(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return "never";
  const s = Math.max(0, Math.round((now - new Date(iso).getTime()) / 1000));
  if (s < 45) return "just now";
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}
export function until(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return "pending";
  const s = Math.round((new Date(iso).getTime() - now) / 1000);
  if (s <= 0) return "due";
  if (s < 3600) return `in ${Math.max(1, Math.round(s / 60))}m`;
  if (s < 86400) return `in ${Math.round(s / 3600)}h`;
  return `in ${Math.round(s / 86400)}d`;
}
export function duration(a: string | null, b: string | null): string {
  if (!a) return "-";
  const ms = (b ? new Date(b).getTime() : Date.now()) - new Date(a).getTime();
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
}
export function when(iso: string | null | undefined): string {
  if (!iso) return "-";
  return new Date(iso).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}
export function kebab(s: string): string {
  return s.toLowerCase().trim().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40);
}
export function excerpt(s: string | null | undefined, n = 90): string {
  const t = (s ?? "").replace(/\s+/g, " ").trim();
  return t.length > n ? t.slice(0, n - 1) + "…" : t;
}
export function money(n: number | null | undefined): string {
  if (n == null) return "-";
  const v = Number(n);
  return `$${v.toFixed(v < 1 ? 4 : 2)}`;
}
export function pretty(v: unknown): string {
  try { return JSON.stringify(v, null, 2) ?? String(v); } catch { return String(v); }
}
export function errMsg(e: unknown): string {
  if (e instanceof Error) return e.message;
  const m = (e as { message?: string } | null)?.message;
  return m ?? String(e);
}
export const DIGEST_PROMPT =
  "Write my daily digest: summarize what you did yesterday and what changed, then lay out today's plan and anything that needs my attention. Send it to me with notify_user.";

export const CRON_PRESETS: { label: string; cron: string }[] = [
  { label: "Daily digest 8:00", cron: "0 8 * * *" },
  { label: "Every weekday 9:00", cron: "0 9 * * 1-5" },
  { label: "Every day 8:00", cron: "0 8 * * *" },
  { label: "Every hour", cron: "0 * * * *" },
  { label: "Every 15 minutes", cron: "*/15 * * * *" },
  { label: "Mondays 10:00", cron: "0 10 * * 1" },
  { label: "First of the month 9:00", cron: "0 9 1 * *" },
];
export function isDangerousAllow(pattern: string, decision: string): boolean {
  if (decision !== "allow") return false;
  const p = pattern.trim();
  return p === "*" || p === "Bash" || p === "Bash(*)";
}
