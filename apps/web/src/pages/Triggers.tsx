import { useState } from "react";
import { api, attempt } from "../lib/api";
import { useLive, useLoad } from "../lib/hooks";
import { ago, errMsg } from "../lib/util";
import { useToast } from "../lib/toast";
import type { Trigger } from "../lib/types";
import type { BotWithStatus } from "../lib/appdata";
import { Button, Chip, Empty, ErrorNote, Field, Spinner, inputCls } from "../components/ui";

type Created = Trigger & { webhook_url?: string };
const urlOf = (t: Created | { url?: string; webhook_url?: string } | undefined) => t?.url ?? t?.webhook_url ?? "";

export function Triggers({ bot }: { bot: BotWithStatus }) {
  const toast = useToast();
  const q = useLoad(() => api.get<Trigger[]>(`/api/bots/${bot.id}/triggers`), [bot.id]);
  useLive(["triggers"], () => q.reload(), { bot: bot.id });
  const [creating, setCreating] = useState(false);
  const [reveal, setReveal] = useState<{ id: string; name: string; url: string } | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function toggle(t: Trigger) {
    const before = q.data;
    q.set((l) => l?.map((x) => (x.id === t.id ? { ...x, enabled: !t.enabled } : x)));
    const e = await attempt(() => api.patch(`/api/triggers/${t.id}`, { enabled: !t.enabled }));
    if (e) { q.set(before); setError(e); toast(e, "bad"); } else q.reload();
  }
  async function del(t: Trigger) {
    if (!confirm(`Delete trigger "${t.name}"? Its URL stops working.`)) return;
    const e = await attempt(() => api.del(`/api/triggers/${t.id}`));
    if (e) setError(e); else { if (reveal?.id === t.id) setReveal(null); q.reload(); }
  }
  async function rotate(t: Trigger) {
    if (!confirm(`Rotate the URL for "${t.name}"? The old URL stops working.`)) return;
    try {
      const r = await api.post<Created>(`/api/triggers/${t.id}/rotate`);
      setReveal({ id: t.id, name: t.name, url: urlOf(r) });
      q.reload();
    } catch (e) { setError(errMsg(e)); }
  }

  const list = q.data ?? [];
  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between gap-3">
        <p className="text-sm text-muted">Let other tools wake {bot.name} with a webhook: a deploy finishes, a form is submitted, an alert fires.</p>
        {!creating && <Button tone="primary" onClick={() => setCreating(true)} className="shrink-0">New trigger</Button>}
      </div>
      <ErrorNote error={error ?? q.error} />
      {reveal && <RevealUrl r={reveal} onClose={() => setReveal(null)} />}
      {creating && (
        <CreateTrigger
          botId={bot.id}
          onCancel={() => setCreating(false)}
          onCreated={(t) => { setCreating(false); setReveal({ id: t.id, name: t.name, url: urlOf(t) }); q.reload(); }}
        />
      )}
      {q.loading ? <Spinner /> : list.length === 0 && !creating ? (
        <Empty title="No triggers">Create one to get a private URL that starts a run when called.</Empty>
      ) : (
        <ul className="divide-y divide-line rounded-lg border border-line bg-surface">
          {list.map((t) => (
            <li key={t.id} className="p-4 space-y-2">
              <div className="flex items-center gap-2 flex-wrap">
                <span className="font-medium">{t.name}</span>
                <Chip tone={t.kind === "proactive" ? "accent" : "muted"}>{t.kind}</Chip>
                {!t.enabled && <Chip tone="warn">off</Chip>}
                <label className="ml-auto flex items-center gap-2 text-sm cursor-pointer min-h-9">
                  <input type="checkbox" className="size-4 accent-accent" checked={t.enabled} onChange={() => toggle(t)} />
                  Enabled
                </label>
              </div>
              <p className="text-sm whitespace-pre-wrap break-words">{t.prompt}</p>
              <div className="flex items-center gap-2 text-xs text-muted">
                <span>Last fired: {t.last_fired_at ? ago(t.last_fired_at) : "never"}</span>
                <span className="ml-auto flex gap-1">
                  <Button tone="ghost" onClick={() => rotate(t)}>Rotate URL</Button>
                  <Button tone="ghost" onClick={() => del(t)}>Delete</Button>
                </span>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function RevealUrl({ r, onClose }: { r: { name: string; url: string }; onClose: () => void }) {
  const [copied, setCopied] = useState(false);
  const curl = `curl -X POST '${r.url}' \\\n  -H 'Content-Type: application/json' \\\n  -d '{"event":"deploy_finished","status":"ok"}'`;
  async function copy() {
    try { await navigator.clipboard.writeText(r.url); setCopied(true); setTimeout(() => setCopied(false), 1500); } catch { /* clipboard blocked */ }
  }
  return (
    <div className="settle rounded-lg border border-warn/60 bg-surface p-4 space-y-3">
      <p className="font-medium">Webhook URL for "{r.name}"</p>
      <p className="text-sm text-warn">Copy it now. For security it is shown only once; rotate the trigger to get a new one.</p>
      <div className="flex gap-2 items-stretch">
        <code className="mono flex-1 min-w-0 break-all rounded-md bg-sunken px-3 py-2 text-[13px]">{r.url || "(the server did not return a URL)"}</code>
        <Button onClick={copy} disabled={!r.url}>{copied ? "Copied" : "Copy"}</Button>
      </div>
      <details>
        <summary className="text-sm text-muted cursor-pointer">Example request</summary>
        <pre className="mono mt-2 rounded-md bg-sunken p-3 text-xs overflow-auto whitespace-pre">{curl}</pre>
        <p className="text-xs text-muted mt-1">The request body (up to 64 KB, any content type) is appended to the prompt. Limit: 30 calls a minute.</p>
      </details>
      <Button onClick={onClose}>I've saved it</Button>
    </div>
  );
}

function CreateTrigger({ botId, onCancel, onCreated }: { botId: string; onCancel: () => void; onCreated: (t: Created) => void }) {
  const [name, setName] = useState("");
  const [prompt, setPrompt] = useState("");
  const [kind, setKind] = useState("chat");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true); setError(null);
    try {
      onCreated(await api.post<Created>(`/api/bots/${botId}/triggers`, { name: name.trim(), prompt: prompt.trim(), kind }));
    } catch (err) { setError(errMsg(err)); setBusy(false); }
  }
  return (
    <form onSubmit={submit} className="settle rounded-lg border border-line bg-surface p-4 space-y-3">
      <Field label="Name"><input className={inputCls} value={name} onChange={(e) => setName(e.target.value)} placeholder="CI failed" required /></Field>
      <Field label="Prompt" hint="What the bot should do. The webhook body is added after it.">
        <textarea className={inputCls} rows={3} value={prompt} onChange={(e) => setPrompt(e.target.value)} placeholder="A build failed. Read the payload below, find the cause, and tell me what to fix." required />
      </Field>
      <Field label="Kind" hint={kind === "proactive" ? "Research only: the bot can read and browse, but cannot act." : "Works like a chat message: the bot can act, and asks for approval when your rules require it."}>
        <select className={inputCls} value={kind} onChange={(e) => setKind(e.target.value)}>
          <option value="chat">chat</option>
          <option value="proactive">proactive (research only)</option>
        </select>
      </Field>
      <ErrorNote error={error} />
      <div className="flex justify-end gap-2">
        <Button type="button" onClick={onCancel}>Cancel</Button>
        <Button tone="primary" type="submit" disabled={busy || !name.trim() || !prompt.trim()}>Create trigger</Button>
      </div>
    </form>
  );
}
