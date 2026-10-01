import { useState } from "react";
import { api, attempt } from "../lib/api";
import { useLive, useLoad } from "../lib/hooks";
import { ago } from "../lib/util";
import { useToast } from "../lib/toast";
import type { Memory as Mem } from "../lib/types";
import { useApp, type BotWithStatus } from "../lib/appdata";
import { Mascot } from "../components/Mascot";
import { Button, Chip, Empty, ErrorNote, Spinner, inputCls } from "../components/ui";

export function Memory({ bot }: { bot: BotWithStatus }) {
  const toast = useToast();
  const { reload: reloadApp } = useApp();
  const q = useLoad(() => api.get<Mem[]>(`/api/bots/${bot.id}/memories`), [bot.id]);
  useLive(["memories"], () => q.reload(), { bot: bot.id });
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<{ id: string; text: string } | null>(null);
  const [dreaming, setDreaming] = useState(false);

  /** Apply a change to the list right away; on failure put the old list back and say so. */
  async function optimistic(change: (l: Mem[]) => Mem[], call: () => Promise<unknown>, ok?: string) {
    const before = q.data;
    q.set((l) => (l ? change(l) : l));
    const error = await attempt(call);
    if (error) { q.set(before); setError(error); toast(error, "bad"); return false; }
    if (ok) toast(ok);
    q.reload();
    return true;
  }
  async function add(e: React.FormEvent) {
    e.preventDefault();
    const error = await attempt(() => api.post(`/api/bots/${bot.id}/memories`, { content: text.trim() }));
    if (error) { setError(error); return; }
    setText(""); setError(null); q.reload();
  }
  async function del(id: string) {
    await optimistic((l) => l.filter((m) => m.id !== id), () => api.del(`/api/memories/${id}`));
  }
  async function decide(id: string, status: "active" | "rejected", content?: string) {
    setEditing(null);
    await optimistic(
      (l) => (status === "rejected" ? l.filter((m) => m.id !== id) : l.map((m) => (m.id === id ? { ...m, status, ...(content != null ? { content } : {}) } : m))),
      () => api.patch(`/api/memories/${id}`, { status, ...(content != null ? { content } : {}) }),
      status === "active" ? "Remembered" : "Dismissed",
    );
  }
  async function dream() {
    setDreaming(true);
    const error = await attempt(() => api.post(`/api/bots/${bot.id}/dream`));
    setDreaming(false);
    if (error) { setError(error); toast(error, "bad"); return; }
    toast(`${bot.name} is going over recent chats`); reloadApp();
  }

  const all = q.data ?? [];
  const proposed = all.filter((m) => m.status === "proposed");
  const active = all.filter((m) => !m.status || m.status === "active");

  return (
    <div className="space-y-6">
      <div className="card p-4 flex items-center gap-3">
        <Mascot id={bot.id} name={bot.name} avatar={bot.avatar} size={44} state={dreaming ? "working" : "idle"} />
        <div className="min-w-0 flex-1">
          <p className="font-medium">What {bot.name} has learned</p>
          <p className="text-sm text-muted">{bot.last_dreamed_at ? `Last reviewed ${ago(bot.last_dreamed_at)}.` : "Not reviewed yet."} Each night it looks back over recent chats and proposes what to remember.</p>
        </div>
        <Button onClick={dream} disabled={dreaming}>{dreaming ? "Starting…" : "Dream now"}</Button>
      </div>
      <ErrorNote error={error ?? q.error} />

      {proposed.length > 0 && (
        <section className="space-y-2" aria-label="Proposed">
          <h2 className="text-base font-semibold">Proposed</h2>
          <p className="text-sm text-muted -mt-1">Nothing here is used until you accept it.</p>
          <ul className="card divide-y divide-line !border-accent/40">
            {proposed.map((m) => (
              <li key={m.id} className="p-3 sm:px-4 space-y-2">
                {editing?.id === m.id ? (
                  <textarea className={inputCls} rows={3} autoFocus value={editing.text} onChange={(e) => setEditing({ id: m.id, text: e.target.value })} />
                ) : <p className="whitespace-pre-wrap break-words">{m.content}</p>}
                <div className="flex gap-2 flex-wrap">
                  {editing?.id === m.id ? (
                    <>
                      <Button tone="primary" onClick={() => void decide(m.id, "active", editing.text.trim())} disabled={!editing.text.trim()}>Save and accept</Button>
                      <Button onClick={() => setEditing(null)}>Cancel</Button>
                    </>
                  ) : (
                    <>
                      <Button tone="primary" onClick={() => void decide(m.id, "active")}>Accept</Button>
                      <Button onClick={() => setEditing({ id: m.id, text: m.content })}>Edit</Button>
                      <Button tone="ghost" onClick={() => void decide(m.id, "rejected")}>Reject</Button>
                    </>
                  )}
                </div>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="space-y-2" aria-label="Remembered">
        <h2 className="text-base font-semibold">Remembered</h2>
        <form onSubmit={add} className="flex gap-2 items-start">
          <textarea className={inputCls + " flex-1"} rows={2} value={text} onChange={(e) => setText(e.target.value)} placeholder="Teach it something: I prefer short answers. My timezone is Europe/Berlin." />
          <Button tone="primary" type="submit" className="!min-h-10" disabled={!text.trim()}>Remember</Button>
        </form>
        {q.loading ? <Spinner /> : active.length === 0 ? (
          <Empty title="Nothing remembered yet">Tell it something worth keeping, or let it suggest things after a few chats.</Empty>
        ) : (
          <ul className="card divide-y divide-line">
            {active.map((m) => (
              <li key={m.id} className="flex items-start gap-3 p-3 sm:px-4">
                <div className="min-w-0 flex-1">
                  <p className="whitespace-pre-wrap break-words">{m.content}</p>
                  <p className="text-xs text-muted mt-1 flex items-center gap-2"><Chip tone={m.source === "bot" ? "accent" : "muted"}>{m.source === "bot" ? "learned" : "you taught"}</Chip>{ago(m.created_at)}</p>
                </div>
                <Button tone="ghost" onClick={() => void del(m.id)} aria-label="Forget this">Forget</Button>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
