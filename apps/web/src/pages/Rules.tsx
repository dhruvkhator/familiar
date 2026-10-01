import { useState } from "react";
import { api, attempt } from "../lib/api";
import { useLoad } from "../lib/hooks";
import { isDangerousAllow } from "../lib/util";
import { useToast } from "../lib/toast";
import type { Rule, RuleDecision } from "../lib/types";
import { Button, Empty, ErrorNote, Field, Spinner, cx, inputCls } from "../components/ui";

const TIERS: { id: RuleDecision; label: string; hint: string }[] = [
  { id: "allow", label: "Do it without asking", hint: "Runs on its own." },
  { id: "review", label: "Let auto-review decide", hint: "A quick reviewer approves safe calls and sends the rest to you." },
  { id: "ask", label: "Ask me first", hint: "Waits for your Approve or Decline." },
  { id: "deny", label: "Hand it to me", hint: "The bot stops and tells you to do it." },
];

const LOCKED = [
  "Deleting files recursively (rm -rf, Remove-Item -Recurse)",
  "Installing software (npm -g, pip, winget, choco)",
  "Running as administrator (sudo)",
  "Force-pushing or hard-resetting git history",
  "Downloading a script and running it",
  "Editing the registry or scheduled tasks",
];

function Tiers({ value, onChange, compact }: { value: RuleDecision; onChange: (d: RuleDecision) => void; compact?: boolean }) {
  return (
    <div role="radiogroup" aria-label="Autonomy" className={cx("grid gap-1 rounded-[12px] bg-sunken p-1", compact ? "grid-cols-2 sm:grid-cols-4" : "grid-cols-2 sm:grid-cols-4")}>
      {TIERS.map((t) => (
        <button key={t.id} type="button" role="radio" aria-checked={value === t.id} onClick={() => onChange(t.id)}
          className={cx("rounded-[9px] px-2 py-1.5 text-xs font-medium leading-tight min-h-10 cursor-pointer",
            value === t.id ? (t.id === "deny" ? "bg-surface text-bad shadow-[var(--shadow)]" : "bg-surface text-accent shadow-[var(--shadow)]") : "text-muted hover:text-ink")}>
          {t.label}
        </button>
      ))}
    </div>
  );
}

export function RulesPanel({ botId }: { botId: string | null }) {
  const toast = useToast();
  const q = useLoad(async () => {
    // GET /api/rules with no bot_id returns global rules only
    const [own, global] = await Promise.all([
      botId ? api.get<Rule[]>(`/api/rules?bot_id=${botId}`) : Promise.resolve([] as Rule[]),
      api.get<Rule[]>("/api/rules"),
    ]);
    return [...global, ...own.filter((r) => !global.some((g) => g.id === r.id))];
  }, [botId]);

  const [pattern, setPattern] = useState("");
  const [decision, setDecision] = useState<RuleDecision>("ask");
  const [note, setNote] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function add(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true); setError(null);
    const error = await attempt(() => api.post("/api/rules", { ...(botId ? { bot_id: botId } : {}), pattern: pattern.trim(), decision, note: note.trim() || undefined }));
    setBusy(false);
    if (error) { setError(error); return; }
    setPattern(""); setNote(""); toast("Rule added"); q.reload();
  }
  async function del(id: string) {
    const error = await attempt(() => api.del(`/api/rules/${id}`));
    if (error) setError(error); else q.reload();
  }
  async function change(r: Rule, d: RuleDecision) {
    if (d === r.decision) return;
    // prefer an in-place update; fall back to replacing the rule if the server has no PATCH
    let err = await attempt(() => api.patch(`/api/rules/${r.id}`, { decision: d }));
    if (err) {
      err = await attempt(async () => {
        await api.post("/api/rules", { ...(r.bot_id ? { bot_id: r.bot_id } : {}), pattern: r.pattern, decision: d, note: r.note ?? undefined });
        await api.del(`/api/rules/${r.id}`);
      });
    }
    if (err) setError(err); else { toast("Updated"); q.reload(); }
  }

  const rules = q.data ?? [];
  const danger = isDangerousAllow(pattern, decision);

  return (
    <div className="space-y-6">
      <form onSubmit={add} className="card p-4 space-y-3">
        <Field label="When the bot wants to use" hint={<>Examples: <code className="mono">Bash(git status*)</code>, <code className="mono">mcp__browser__browser_click</code>, <code className="mono">Edit</code></>}>
          <input className={inputCls + " mono"} value={pattern} onChange={(e) => setPattern(e.target.value)} placeholder="Bash(git status*)" required />
        </Field>
        <div>
          <span className="block text-sm font-medium mb-1">What should happen</span>
          <Tiers value={decision} onChange={setDecision} />
          <span className="block text-xs text-muted mt-1">{TIERS.find((t) => t.id === decision)!.hint}</span>
        </div>
        <Field label="Note (optional)">
          <input className={inputCls} value={note} onChange={(e) => setNote(e.target.value)} placeholder="Why this rule exists" />
        </Field>
        {danger && <DangerWarning />}
        <ErrorNote error={error} />
        <Button tone="primary" type="submit" disabled={busy || !pattern.trim()}>Add rule</Button>
      </form>

      {q.loading ? <Spinner /> : rules.length === 0 ? (
        <Empty title="No rules yet">Without rules, shell commands, edits and browser actions ask for your approval.</Empty>
      ) : (
        <ul className="card divide-y divide-line">
          {rules.map((r) => (
            <li key={r.id} className="p-3 sm:px-4 space-y-2">
              <div className="flex items-center gap-2 flex-wrap">
                <code className="mono break-all font-medium">{r.pattern}</code>
                {botId && r.bot_id == null && <span className="text-xs rounded bg-sunken text-muted px-1.5 py-0.5">global</span>}
                <Button tone="ghost" className="ml-auto" onClick={() => void del(r.id)} aria-label={`Delete rule ${r.pattern}`}>Delete</Button>
              </div>
              <Tiers value={r.decision} onChange={(d) => void change(r, d)} compact />
              {r.note && <p className="text-sm text-muted">{r.note}</p>}
              {isDangerousAllow(r.pattern, r.decision) && <DangerWarning />}
            </li>
          ))}
        </ul>
      )}

      <section aria-label="Always asks you" className="card p-4">
        <p className="font-medium flex items-center gap-2"><span aria-hidden>🔒</span> Always your call</p>
        <p className="text-sm text-muted mb-2">These never run without you, whatever the rules say.</p>
        <ul className="space-y-1 text-sm">
          {LOCKED.map((l) => <li key={l} className="flex gap-2 text-muted"><span aria-hidden>•</span>{l}</li>)}
        </ul>
      </section>
    </div>
  );
}

function DangerWarning() {
  return (
    <p role="alert" className="rounded-[10px] bg-bad-soft text-bad text-sm px-3 py-2">
      This lets the bot run any shell command on your PC without asking. Prefer narrow patterns like <code className="mono">Bash(git status*)</code>.
    </p>
  );
}

export function GlobalRules() {
  return (
    <div className="max-w-2xl mx-auto p-4 md:p-8">
      <h1 className="text-xl font-semibold tracking-tight mb-1">Global rules</h1>
      <p className="text-sm text-muted mb-4">Apply to every teammate. Teammate-specific rules sit alongside these.</p>
      <RulesPanel botId={null} />
    </div>
  );
}
