import { memo, useEffect, useRef, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { api, attempt } from "../lib/api";
import { useLive, useLoad } from "../lib/hooks";
import { ago, errMsg, excerpt } from "../lib/util";
import type { Approval, Message, Run, Thread } from "../lib/types";
import type { BotWithStatus } from "../lib/appdata";
import { ApprovalCard } from "../components/ApprovalCard";
import { EventList } from "../components/EventList";
import { Mascot } from "../components/Mascot";
import { prefetchThread } from "../lib/prefetch";
import { Button, Chip, Empty, ErrorNote, Icon, RunChip, Spinner, cx } from "../components/ui";

const ACTIVE = ["queued", "running", "waiting_approval"];

export const Chat = memo(function Chat({ bot, threadId }: { bot: BotWithStatus; threadId?: string }) {
  const nav = useNavigate();
  const threads = useLoad(() => api.get<Thread[]>(`/api/bots/${bot.id}/threads`), [bot.id]);
  useLive(["threads", "runs"], () => threads.reload(), { bot: bot.id });
  const [error, setError] = useState<string | null>(null);

  async function newThread() {
    setError(null);
    try {
      const t = await api.post<Thread>(`/api/bots/${bot.id}/threads`, { title: "New chat" });
      threads.reload();
      nav(`/bot/${bot.slug}/chat/${t.id}`);
    } catch (e) { setError(errMsg(e)); }
  }

  const list = threads.data ?? [];
  return (
    <div className="h-full flex">
      <div className={cx("w-full md:w-60 md:shrink-0 md:border-r border-line flex-col min-h-0", threadId ? "hidden md:flex" : "flex")}>
        <div className="p-3 shrink-0">
          <Button tone="primary" className="w-full" onClick={newThread}><Icon name="plus" className="size-4" /> New thread</Button>
          {error && <div className="mt-2"><ErrorNote error={error} /></div>}
        </div>
        <div className="flex-1 overflow-auto px-2 pb-3">
          {threads.loading ? <Spinner /> : list.length === 0 ? (
            <div className="p-2"><Empty title="No threads">Start one and say hello to {bot.name}.</Empty></div>
          ) : (
            list.map((t) => (
              <Link
                key={t.id}
                to={`/bot/${bot.slug}/chat/${t.id}`}
                onMouseEnter={() => prefetchThread(t.id)}
                onFocus={() => prefetchThread(t.id)}
                className={cx("block rounded-[10px] px-3 py-2 min-h-12", t.id === threadId ? "bg-sunken" : "hover:bg-sunken/60")}
              >
                <div className="text-sm font-medium truncate">{t.title || "Untitled"}</div>
                <div className="text-xs text-muted flex items-center gap-1.5">{(t.source ?? (t.schedule_id ? "schedule" : null)) && t.source !== "web" && <Chip tone="accent">{t.source ?? "schedule"}</Chip>}{ago(t.updated_at)}</div>
              </Link>
            ))
          )}
        </div>
      </div>
      <div className={cx("flex-1 min-w-0 min-h-0", threadId ? "flex" : "hidden md:flex")}>
        {threadId ? (
          <ThreadView key={threadId} bot={bot} threadId={threadId} thread={list.find((t) => t.id === threadId)} onTitled={threads.reload} />
        ) : (
          <div className="m-auto text-center px-6">
            <Mascot id={bot.id} name={bot.name} avatar={bot.avatar} size={180} state={bot.status === "paused" ? "paused" : "idle"} className="mb-3" />
            <p className="text-lg font-medium">Chat with {bot.name}</p>
            <p className="text-sm text-muted mb-4">Pick a thread, or start a new one.</p>
            <Button tone="primary" onClick={newThread}>New thread</Button>
          </div>
        )}
      </div>
    </div>
  );
});

type BotLook = Pick<BotWithStatus, "id" | "name" | "avatar">;

const MessageRow = memo(function MessageRow({ m, bot, pending }: { m: Message; bot: BotLook; pending?: boolean }) {
  return m.role === "user" ? (
    <div className={cx("settle flex justify-end", pending && "opacity-60")}>
      <div className="max-w-[85%] rounded-[16px] rounded-br-[4px] bg-accent-soft px-4 py-2.5 whitespace-pre-wrap break-words" title={pending ? "Sending…" : ago(m.created_at)}>{m.content}</div>
    </div>
  ) : (
    <div className="settle flex gap-3 items-start">
      <Mascot id={bot.id} name={bot.name} avatar={bot.avatar} size={30} className="mt-0.5" />
      <div className="min-w-0 flex-1">
        <div className="text-xs text-muted mb-0.5">{m.role === "assistant" ? bot.name : "System"} · {ago(m.created_at)}</div>
        <div className="whitespace-pre-wrap break-words">{m.content}</div>
      </div>
    </div>
  );
}, (a, b) => a.m.id === b.m.id && a.m.content === b.m.content && a.m.created_at === b.m.created_at && a.m.role === b.m.role
  && !a.pending === !b.pending && a.bot.id === b.bot.id && a.bot.name === b.bot.name && a.bot.avatar === b.bot.avatar);

interface Outgoing { id: string; content: string; known: Set<string>; at: string }

function ThreadView({ bot, threadId, thread, onTitled }: { bot: BotWithStatus; threadId: string; thread?: Thread; onTitled: () => void }) {
  const msgs = useLoad(
    async () => (await api.get<Message[]>(`/api/threads/${threadId}/messages?limit=200`)).slice().sort((a, b) => a.created_at.localeCompare(b.created_at)),
    [threadId],
  );
  const runs = useLoad(() => api.get<Run[]>(`/api/threads/${threadId}/runs?limit=5`), [threadId]);
  useLive(["messages"], () => msgs.reload());
  useLive(["runs"], () => runs.reload(), { bot: bot.id });

  // runs come newest first
  const run = runs.data?.[0];
  const active = !!run && ACTIVE.includes(run.status);

  const approvals = useLoad(
    async () => (run ? (await api.get<Approval[]>("/api/approvals?status=pending")).filter((a) => a.run_id === run.id) : []),
    [run?.id, run?.status],
  );
  useLive(["approvals"], () => approvals.reload(), undefined, !!run && active);

  // stick to bottom while content grows, unless the user scrolled up
  const scroller = useRef<HTMLDivElement>(null);
  const inner = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  useEffect(() => {
    const s = scroller.current, i = inner.current;
    if (!s || !i) return;
    const ro = new ResizeObserver(() => { if (stick.current) s.scrollTop = s.scrollHeight; });
    ro.observe(i);
    return () => ro.disconnect();
  }, []);

  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // optimistic: messages we have sent that the server hasn't echoed back yet
  const [outgoing, setOutgoing] = useState<Outgoing[]>([]);
  const seq = useRef(0);
  useEffect(() => {
    if (!msgs.data) return;
    const have = new Set(msgs.data.map((m) => m.id));
    setOutgoing((o) => {
      const left = o.filter((p) => !msgs.data!.some((m) => m.role === "user" && m.content === p.content && !p.known.has(m.id) && have.has(m.id)));
      return left.length === o.length ? o : left;
    });
  }, [msgs.data]);

  async function send(retryContent?: string) {
    const content = (retryContent ?? text).trim();
    if (!content || sending) return;
    setSending(true); setError(null);
    const mine: Outgoing = { id: "pending-" + ++seq.current, content, known: new Set((msgs.data ?? []).map((m) => m.id)), at: new Date().toISOString() };
    setOutgoing((o) => [...o, mine]);
    if (retryContent === undefined) setText("");
    stick.current = true;
    const error = await attempt(() => api.post(`/api/threads/${threadId}/messages`, { content }));
    setSending(false);
    if (error) {
      setOutgoing((o) => o.filter((p) => p.id !== mine.id));
      if (retryContent === undefined) setText((t) => t || content);
      setError(error);
      return;
    }
    msgs.reload(); runs.reload();
    // safety net: never leave a ghost bubble if the server row doesn't match
    setTimeout(() => setOutgoing((o) => o.filter((p) => p.id !== mine.id)), 6000);
    if (!thread?.title || thread.title === "New chat") {
      await attempt(() => api.patch(`/api/threads/${threadId}`, { title: excerpt(content, 48) }));
      onTitled();
    }
  }
  async function cancel() {
    if (!run) return;
    const error = await attempt(() => api.post(`/api/runs/${run.id}/cancel`));
    if (error) setError(error); else runs.reload();
  }
  function onKey(e: React.KeyboardEvent) {
    const coarse = window.matchMedia("(pointer: coarse)").matches;
    if (e.key === "Enter" && !e.shiftKey && !coarse) { e.preventDefault(); void send(); }
  }

  const messages = msgs.data ?? [];
  const lastUser = [...messages].reverse().find((m) => m.role === "user");
  return (
    <div className="flex-1 min-w-0 flex flex-col min-h-0">
      <div className="md:hidden shrink-0 px-2 pt-1">
        <Link to={`/bot/${bot.slug}/chat`} className="inline-flex items-center gap-1 min-h-10 px-2 text-sm text-muted"><Icon name="back" className="size-4" /> Threads</Link>
      </div>
      <div
        ref={scroller}
        className="flex-1 min-h-0 overflow-auto"
        onScroll={(e) => { const s = e.currentTarget; stick.current = s.scrollHeight - s.scrollTop - s.clientHeight < 80; }}
      >
        <div ref={inner} className="max-w-2xl mx-auto px-4 py-4 space-y-5">
          {msgs.loading ? <Spinner /> : messages.length === 0 && outgoing.length === 0 && !run ? (
            <div className="text-center py-8">
              <Mascot id={bot.id} name={bot.name} avatar={bot.avatar} size={180} state={bot.status === "paused" ? "paused" : "idle"} className="mb-3" />
              <p className="text-lg font-medium">Say hello to {bot.name}</p>
              <p className="text-sm text-muted">Ask for something, or tell it what to keep an eye on.</p>
            </div>
          ) : null}
          {messages.map((m) => <MessageRow key={m.id} m={m} bot={bot} />)}
          {outgoing.map((p) => (
            <MessageRow key={p.id} pending bot={bot} m={{ id: p.id, role: "user", content: p.content, created_at: p.at } as Message} />
          ))}
          {run && active && (
            <div className="card">
              <div className="flex items-center gap-2 px-3 py-2 border-b border-line">
                <RunChip status={run.status} />
                {run.kind === "handoff" ? <Chip tone="accent">handoff</Chip> : <span className="text-xs text-muted">{run.kind} run</span>}
                <Link to={`/bot/${bot.slug}/activity/${run.id}`} className="text-xs text-accent underline ml-auto">Details</Link>
                <Button tone="danger" className="!min-h-8" onClick={cancel}>Cancel</Button>
              </div>
              <div className="px-3 pt-3">
                <EventList runId={run.id} live />
              </div>
              {(approvals.data ?? []).length > 0 && (
                <div className="p-3 space-y-3">
                  {(approvals.data ?? []).map((a) => <ApprovalCard key={a.id} a={a} onDone={approvals.reload} />)}
                </div>
              )}
            </div>
          )}
          {run && (run.status === "failed" || run.status === "cancelled") && lastUser && (
            <div className="flex justify-end items-center gap-2 text-xs text-muted">
              <span className="text-bad truncate max-w-[60%]" title={run.error ?? ""}>{run.status === "cancelled" ? "Cancelled" : "Didn't finish"}{run.error ? ` · ${excerpt(run.error, 80)}` : ""}</span>
              <Link to={`/bot/${bot.slug}/activity/${run.id}`} className="text-accent underline">Details</Link>
              <Button className="!min-h-7" onClick={() => void send(lastUser.content)} disabled={sending}>Retry</Button>
            </div>
          )}
        </div>
      </div>
      <div className="shrink-0 p-3 safe-b">
        <div className="max-w-2xl mx-auto">
          <ErrorNote error={error} />
          <div className="flex gap-2 items-end rounded-[13px] border border-line bg-surface p-2 shadow-[var(--shadow)] focus-within:border-accent">
            <textarea
              className="flex-1 bg-transparent px-2 py-1.5 max-h-40 min-h-9 resize-none outline-none"
              rows={Math.min(6, Math.max(1, text.split("\n").length))}
              value={text}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={onKey}
              placeholder={`Message ${bot.name}`}
              aria-label="Message"
            />
            <Button tone="primary" className="!min-h-9" onClick={() => void send()} disabled={sending || !text.trim()}>Send</Button>
          </div>
        </div>
      </div>
    </div>
  );
}
