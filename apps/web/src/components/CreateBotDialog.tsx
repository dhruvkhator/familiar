import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { api } from "../lib/api";
import { useApp } from "../lib/appdata";
import { errMsg, kebab } from "../lib/util";
import type { Bot, BotEngine } from "../lib/types";
import { useToast } from "../lib/toast";
import { AvatarBuilder } from "./AvatarBuilder";
import { EngineFields } from "./EngineFields";
import { randomAvatar, type Avatar } from "./Mascot";
import { Button, Dialog, ErrorNote, Field, inputCls } from "./ui";

export function CreateBotDialog({ onClose }: { onClose: () => void }) {
  const { reload } = useApp();
  const nav = useNavigate();
  const toast = useToast();
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [slugTouched, setSlugTouched] = useState(false);
  const [persona, setPersona] = useState("");
  const [engine, setEngine] = useState<BotEngine>("claude");
  const [model, setModel] = useState("sonnet");
  const [avatar, setAvatar] = useState<Avatar>(() => randomAvatar());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const effSlug = slugTouched ? slug : kebab(name);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!name.trim() || !effSlug) return;
    setBusy(true); setError(null);
    try {
      const bot = await api.post<Bot>("/api/bots", { name: name.trim(), slug: effSlug, persona, model, engine, avatar });
      reload();
      toast(`${name.trim()} is ready`);
      onClose();
      nav(`/bot/${bot?.slug ?? effSlug}`);
    } catch (err) {
      setError(errMsg(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog title="New teammate" onClose={onClose}>
      <form onSubmit={submit} className="space-y-4">
        <Field label="Name">
          <input autoFocus className={inputCls} value={name} onChange={(e) => setName(e.target.value)} placeholder="Mochi" required />
        </Field>
        <Field label="Persona" hint="Who it is and how it should work. Becomes the start of its instructions.">
          <textarea className={inputCls} rows={3} value={persona} onChange={(e) => setPersona(e.target.value)} placeholder="You are a careful research assistant. Cite sources and keep answers short." />
        </Field>
        <EngineFields engine={engine} model={model} onChange={(en, m) => { setEngine(en); setModel(m); }} />
        <details className="rounded-[12px] border border-line p-3">
          <summary className="cursor-pointer text-sm font-medium">Customize look</summary>
          <div className="mt-3"><AvatarBuilder value={avatar} onChange={setAvatar} name={name} /></div>
        </details>
        <Field label="Slug" hint="Folder name for its workspace.">
          <input className={inputCls + " mono"} value={effSlug} onChange={(e) => { setSlug(kebab(e.target.value)); setSlugTouched(true); }} required />
        </Field>
        <ErrorNote error={error} />
        <div className="flex justify-end gap-2 pt-1">
          <Button type="button" onClick={onClose}>Cancel</Button>
          <Button tone="primary" type="submit" disabled={busy || !name.trim() || !effSlug}>{busy ? "Creating…" : "Create teammate"}</Button>
        </div>
      </form>
    </Dialog>
  );
}
