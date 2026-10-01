import { useState } from "react";
import { api, attempt } from "../lib/api";
import { useLive, useLoad, useNow } from "../lib/hooks";
import { CRON_PRESETS, DIGEST_PROMPT, ago, until, when } from "../lib/util";
import { useToast } from "../lib/toast";
import type { Schedule } from "../lib/types";
import type { BotWithStatus } from "../lib/appdata";
import { Button, Chip, Empty, ErrorNote, Field, Spinner, inputCls } from "../components/ui";

export function Schedules({ bot }: { bot: BotWithStatus }) {
  const q = useLoad(() => api.get<Schedule[]>(`/api/bots/${bot.id}/schedules`), [bot.id]);
  useLive(["schedules"], () => q.reload(), { bot: bot.id });
  const now = useNow(30000);
  const toast = useToast();
  const [editing, setEditing] = useState<Schedule | "new" | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function toggle(s: Schedule) {
    const before = q.data;
    q.set((l) => l?.map((x) => (x.id === s.id ? { ...x, enabled: !s.enabled } : x)));
    const error = await attempt(() => api.patch(`/api/schedules/${s.id}`, { enabled: !s.enabled }));
    if (error) { q.set(before); setError(error); toast(error, "bad"); } else q.reload();
  }
  async function del(s: Schedule) {
    if (!confirm("Delete this schedule?")) return;
    const error = await attempt(() => api.del(`/api/schedules/${s.id}`));
    if (error) setError(error); else q.reload();
  }

  const list = q.data ?? [];
  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <p className="text-sm text-muted">Run {bot.name} on a timetable, without being asked.</p>
        {editing == null && <Button tone="primary" onClick={() => setEditing("new")}>New schedule</Button>}
      </div>
      <ErrorNote error={error ?? q.error} />
      {editing && (
        <ScheduleForm
          key={editing === "new" ? "new" : editing.id}
          botId={bot.id}
          initial={editing === "new" ? null : editing}
          onDone={() => { setEditing(null); q.reload(); }}
          onCancel={() => setEditing(null)}
        />
      )}
      {q.loading ? <Spinner /> : list.length === 0 && !editing ? (
        <Empty title="No schedules">For example: every weekday at 9:00, summarize what changed overnight.</Empty>
      ) : (
        <ul className="divide-y divide-line rounded-lg border border-line bg-surface">
          {list.map((s) => (
            <li key={s.id} className="p-4 space-y-2">
              <div className="flex items-center gap-2 flex-wrap">
                <code className="mono font-medium">{s.cron}</code>
                <Chip tone={s.kind === "proactive" ? "accent" : "muted"}>{s.kind}</Chip>
                {!s.enabled && <Chip tone="warn">off</Chip>}
                <label className="ml-auto flex items-center gap-2 text-sm cursor-pointer min-h-9">
                  <input type="checkbox" className="size-4 accent-[var(--accent)]" checked={s.enabled} onChange={() => toggle(s)} />
                  Enabled
                </label>
              </div>
              <p className="whitespace-pre-wrap break-words text-sm">{s.prompt}</p>
              {s.gate_command && <p className="text-xs text-muted">Only when <code className="mono">{s.gate_command}</code> prints something</p>}
              <div className="flex items-center gap-3 flex-wrap text-xs text-muted tnum">
                <span>Next: {s.enabled ? `${until(s.next_run_at, now)}${s.next_run_at ? " (" + when(s.next_run_at) + ")" : ""}` : "off"}</span>
                <span>Last: {ago(s.last_run_at, now)}</span>
                <span className="ml-auto flex gap-1">
                  <Button tone="ghost" onClick={() => setEditing(s)}>Edit</Button>
                  <Button tone="ghost" onClick={() => del(s)}>Delete</Button>
                </span>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function ScheduleForm({ botId, initial, onDone, onCancel }: { botId: string; initial: Schedule | null; onDone: () => void; onCancel: () => void }) {
  const [cron, setCron] = useState(initial?.cron ?? "0 9 * * 1-5");
  const [prompt, setPrompt] = useState(initial?.prompt ?? "");
  const [kind, setKind] = useState<Schedule["kind"]>(initial?.kind ?? "scheduled");
  const [enabled, setEnabled] = useState(initial?.enabled ?? true);
  const [gate, setGate] = useState(initial?.gate_command ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const fields = cron.trim().split(/\s+/).length;

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true); setError(null);
    const row = { cron: cron.trim(), prompt: prompt.trim(), kind, enabled, gate_command: gate.trim() || null };
    const error = await attempt(() => initial
      ? api.patch(`/api/schedules/${initial.id}`, row)
      : api.post(`/api/bots/${botId}/schedules`, row));
    setBusy(false);
    if (error) setError(error); else onDone();
  }

  return (
    <form onSubmit={submit} className="rounded-lg border border-line bg-surface p-4 space-y-3 settle">
      <Field label="Cron" hint="Five fields: minute hour day month weekday.">
        <input className={inputCls + " mono"} value={cron} onChange={(e) => setCron(e.target.value)} required />
      </Field>
      <div className="flex flex-wrap gap-1.5">
        {CRON_PRESETS.map((p) => (
          <button type="button" key={p.cron} onClick={() => { setCron(p.cron); if (p.label.startsWith("Daily digest") && !prompt.trim()) setPrompt(DIGEST_PROMPT); }}
            className={"rounded-full border px-3 min-h-8 text-xs cursor-pointer " + (cron === p.cron ? "border-accent text-accent bg-accent-soft" : "border-line text-muted hover:text-ink")}>
            {p.label}
          </button>
        ))}
      </div>
      {fields !== 5 && <p className="text-xs text-warn">A cron expression needs exactly 5 fields.</p>}
      <Field label="Prompt">
        <textarea className={inputCls} rows={3} value={prompt} onChange={(e) => setPrompt(e.target.value)} placeholder="Check my inbox and summarize anything urgent." required />
      </Field>
      <Field label="Kind" hint={kind === "proactive"
        ? "Proactive runs are research only: the bot can read files and browse the web, but cannot act or change anything."
        : "Scheduled runs work like a chat message: the bot can act, and asks for approval when your rules require it."}>
        <select className={inputCls} value={kind} onChange={(e) => setKind(e.target.value as Schedule["kind"])}>
          <option value="scheduled">scheduled</option>
          <option value="proactive">proactive (research only)</option>
        </select>
      </Field>
      <Field label="Only run when this command prints something (optional)" hint="Saves your subscription quota: the bot wakes only if the command outputs text. Its output is added to the prompt. Runs in the bot's workspace; 60 s limit.">
        <input className={inputCls + " mono"} value={gate} onChange={(e) => setGate(e.target.value)} placeholder={'gh pr list --search "review-requested:@me"'} />
      </Field>
      <label className="flex items-center gap-2 text-sm min-h-9"><input type="checkbox" className="size-4 accent-[var(--accent)]" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} /> Enabled</label>
      <ErrorNote error={error} />
      <div className="flex justify-end gap-2">
        <Button type="button" onClick={onCancel}>Cancel</Button>
        <Button tone="primary" type="submit" disabled={busy || fields !== 5 || !prompt.trim()}>{initial ? "Save changes" : "Create schedule"}</Button>
      </div>
    </form>
  );
}
