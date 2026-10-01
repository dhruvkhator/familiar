import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { api, attempt } from "../lib/api";
import { useApp, type BotWithStatus } from "../lib/appdata";
import { useToast } from "../lib/toast";
import type { BotEngine } from "../lib/types";
import { AvatarBuilder } from "../components/AvatarBuilder";
import { EngineFields } from "../components/EngineFields";
import { resolveAvatar, type Avatar } from "../components/Mascot";
import { Button, ErrorNote, Field, inputCls } from "../components/ui";

export function Settings({ bot }: { bot: BotWithStatus }) {
  const { reload } = useApp();
  const nav = useNavigate();
  const toast = useToast();
  const [name, setName] = useState(bot.name);
  const [persona, setPersona] = useState(bot.persona ?? "");
  const [engine, setEngine] = useState<BotEngine>(bot.engine ?? "claude");
  const [model, setModel] = useState(bot.model);
  const [paused, setPaused] = useState(bot.paused);
  const [avatar, setAvatar] = useState<Avatar>(() => resolveAvatar(bot.id, bot.avatar));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function save(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true); setError(null);
    const error = await attempt(() => api.patch(`/api/bots/${bot.id}`, { name: name.trim(), persona, model, engine, paused, avatar }));
    setBusy(false);
    if (error) { setError(error); toast(error, "bad"); return; }
    toast("Saved"); reload();
  }
  async function del() {
    if (!confirm(`Delete ${bot.name}? Its threads, runs, memories and schedules go with it.`)) return;
    const error = await attempt(() => api.del(`/api/bots/${bot.id}`));
    if (error) { setError(error); return; }
    reload(); nav("/");
  }

  return (
    <div className="space-y-8">
      <form onSubmit={save} className="space-y-5">
        <Field label="Name"><input className={inputCls} value={name} onChange={(e) => setName(e.target.value)} required /></Field>
        <div className="card p-4">
          <p className="text-sm font-medium mb-3">Look</p>
          <AvatarBuilder value={avatar} onChange={setAvatar} name={name} />
        </div>
        <Field label="Persona" hint="Written at the top of its instructions.">
          <textarea className={inputCls} rows={6} value={persona} onChange={(e) => setPersona(e.target.value)} />
        </Field>
        <EngineFields engine={engine} model={model} onChange={(en, m) => { setEngine(en); setModel(m); }} />
        <label className="flex items-start gap-3 min-h-10 cursor-pointer">
          <input type="checkbox" className="size-4 mt-1 accent-accent" checked={paused} onChange={(e) => setPaused(e.target.checked)} />
          <span><span className="font-medium">Paused</span><span className="block text-sm text-muted">Queued runs wait until you resume this teammate.</span></span>
        </label>
        <ErrorNote error={error} />
        <Button tone="primary" type="submit" disabled={busy || !name.trim()}>Save changes</Button>
        <p className="text-xs text-muted">Slug: <span className="mono">{bot.slug}</span> (fixed, it names the workspace folder)</p>
      </form>
      <div className="rounded-[14px] border border-bad/50 p-4">
        <p className="font-medium">Delete this teammate</p>
        <p className="text-sm text-muted mb-3">This removes its history from the database. The workspace folder on your PC is left alone.</p>
        <Button tone="danger" onClick={del}>Delete {bot.name}</Button>
      </div>
    </div>
  );
}
