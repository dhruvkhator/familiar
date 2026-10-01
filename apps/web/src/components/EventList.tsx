import { memo, useEffect, useRef, useState } from "react";
import { api, subscribeDelta } from "../lib/api";
import { ArtifactView } from "./Artifacts";
import { useLive } from "../lib/hooks";
import { errMsg as errMsgOf, pretty } from "../lib/util";
import type { RunEvent } from "../lib/types";
import { Chip, Spinner, cx } from "./ui";

const TRUNC = 600;

function str(p: Record<string, unknown> | null, ...keys: string[]): string | null {
  if (!p) return null;
  for (const k of keys) if (typeof p[k] === "string" && p[k]) return p[k] as string;
  return null;
}
function toText(v: unknown): string {
  if (typeof v === "string") return v;
  if (Array.isArray(v)) return v.map((x) => (x && typeof x === "object" && "text" in x ? String((x as { text: unknown }).text) : pretty(x))).join("\n");
  return pretty(v);
}

function Expandable({ text, mono = true }: { text: string; mono?: boolean }) {
  const [open, setOpen] = useState(false);
  const long = text.length > TRUNC;
  return (
    <div>
      <pre className={cx(mono && "mono", "whitespace-pre-wrap break-words text-[13px]")}>{open || !long ? text : text.slice(0, TRUNC) + "…"}</pre>
      {long && (
        <button onClick={() => setOpen((o) => !o)} className="text-xs text-accent underline mt-1 cursor-pointer">
          {open ? "Collapse" : `Show all ${text.length.toLocaleString()} chars`}
        </button>
      )}
    </div>
  );
}

const EventRow = memo(function EventRow({ e }: { e: RunEvent }) {
  const p = e.payload;
  let body: React.ReactNode;
  let dot = "bg-line";
  switch (e.kind) {
    case "text":
    case "result":
      dot = e.kind === "result" ? "bg-ok" : "bg-ink";
      body = <p className="whitespace-pre-wrap break-words">{str(p, "text", "content", "result") ?? pretty(p)}</p>;
      break;
    case "thinking":
      body = <p className="whitespace-pre-wrap break-words text-muted italic text-sm">{str(p, "text", "thinking", "content") ?? pretty(p)}</p>;
      break;
    case "tool_call": {
      dot = "bg-accent";
      const name = str(p, "name", "tool_name") ?? "tool";
      const input = p?.input ?? p;
      body = (
        <details className="group">
          <summary className="cursor-pointer list-none flex items-center gap-2">
            <span className="mono font-medium text-accent">{name}</span>
            <span className="mono text-muted truncate">{previewInput(input)}</span>
          </summary>
          <pre className="mono mt-1 max-h-72 overflow-auto rounded-md bg-sunken p-2 text-xs whitespace-pre-wrap break-all">{pretty(input)}</pre>
        </details>
      );
      break;
    }
    case "tool_result": {
      const isErr = p?.is_error === true;
      dot = isErr ? "bg-bad" : "bg-muted";
      body = (
        <div className={cx("rounded-md bg-sunken px-2 py-1.5 border-l-2", isErr ? "border-bad" : "border-line")}>
          <Expandable text={toText(p?.content ?? p?.output ?? p)} />
        </div>
      );
      break;
    }
    case "approval": {
      dot = "bg-warn";
      const tool = str(p, "tool_name", "name");
      const status = str(p, "status", "decision");
      const by = str(p, "decided_by");
      body = (
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <Chip tone={status === "approved" ? "ok" : status === "pending" ? "warn" : "bad"}>{status ?? "approval"}</Chip>
          {tool && <span className="mono">{tool}</span>}
          {by && <span className="text-muted">by {by}</span>}
          {str(p, "reason") && <span className="text-muted">{str(p, "reason")}</span>}
        </div>
      );
      break;
    }
    case "artifact": {
      dot = "bg-accent";
      const id = str(p, "artifact_id", "id");
      body = id ? (
        <ArtifactView a={{ id, name: str(p, "name") ?? "file", mime: str(p, "mime") ?? "", bytes: Number(p?.bytes ?? 0) }} />
      ) : <p className="mono text-xs text-muted">artifact</p>;
      break;
    }
    case "error":
      dot = "bg-bad";
      body = <p className="text-bad whitespace-pre-wrap break-words">{str(p, "message", "error", "text") ?? pretty(p)}</p>;
      break;
    default:
      body = <p className="mono text-xs text-muted break-all">{e.kind}{str(p, "message", "status", "text") ? `: ${str(p, "message", "status", "text")}` : ""}</p>;
  }
  return (
    <li className="relative pl-6 pb-3 cv-auto">
      <span className={cx("absolute left-0 top-[7px] size-2 rounded-full", dot)} />
      <span className="absolute left-[3px] top-4 bottom-0 w-px bg-line" />
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">{body}</div>
        <span className="mono text-[11px] text-muted tnum pt-0.5">{e.seq}</span>
      </div>
    </li>
  );
}, (a, b) => a.e.id === b.e.id && a.e.seq === b.e.seq && a.e.kind === b.e.kind && a.e.payload === b.e.payload);

function previewInput(input: unknown): string {
  if (input && typeof input === "object") {
    const o = input as Record<string, unknown>;
    for (const k of ["command", "file_path", "path", "url", "query", "pattern"]) if (typeof o[k] === "string") return o[k] as string;
  }
  return "";
}

/** Events already fetched per run, so reopening a run shows them at once and only asks for what's new. */
const seen = new Map<string, RunEvent[]>();

function merge(cur: RunEvent[], more: RunEvent[]): RunEvent[] {
  const have = new Set(cur.map((x) => x.seq));
  const add = more.filter((x) => !have.has(x.seq));
  if (!add.length) return cur;
  return [...cur, ...add].sort((x, y) => x.seq - y.seq);
}

/** Timeline for one run; live-updates while `live`. */
export function EventList({ runId, live }: { runId: string; live: boolean }) {
  const [events, setEvents] = useState<RunEvent[] | undefined>(() => seen.get(runId));
  const [error, setError] = useState<string | null>(null);
  const ref = useRef<RunEvent[] | undefined>(events);
  const [prevRun, setPrevRun] = useState(runId);
  if (prevRun !== runId) { setPrevRun(runId); setEvents(seen.get(runId)); setError(null); }
  ref.current = prevRun === runId ? events : seen.get(runId);

  // fetch only what we haven't seen (after_seq); a burst of notices re-pulls at most once more
  const fetching = useRef(false);
  const again = useRef(false);
  const alive = useRef(true);
  const run = useRef(runId);
  run.current = runId;
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  const pull = async () => {
    if (fetching.current) { again.current = true; return; }
    fetching.current = true;
    const id = runId;
    try {
      do {
        again.current = false;
        const have = seen.get(id);
        const last = have?.length ? have[have.length - 1].seq : -1;
        const more = await api.get<RunEvent[]>(have?.length ? `/api/runs/${id}/events?after_seq=${last}` : `/api/runs/${id}/events`);
        const next = merge(have ?? [], more);
        seen.set(id, next);
        if (alive.current && run.current === id) { setEvents(next); setError(null); }
      } while (again.current);
    } catch (e) {
      if (alive.current && run.current === id && !seen.has(id)) setError(errMsgOf(e));
    } finally { fetching.current = false; }
  };
  useEffect(() => { void pull(); /* eslint-disable-next-line react-hooks/exhaustive-deps */ }, [runId]);
  useLive(["events"], () => { void pull(); }, { run: runId }, live);
  // final events can land right as the run finishes
  useLive(["runs"], () => { void pull(); }, { run: runId }, live);

  const list = ref.current;
  if (!list) return error ? <p className="text-bad text-sm">{error}</p> : <Spinner label="Loading events" />;
  if (!list.length && !live) return <p className="text-sm text-muted py-2">No events recorded.</p>;
  let nText = 0, nThink = 0;
  for (const e of list) { if (e.kind === "text") nText++; else if (e.kind === "thinking") nThink++; }
  return (
    <>
      <ol className="text-[15px]">{list.map((e) => <EventRow key={e.id} e={e} />)}</ol>
      {live && <LiveBubble runId={runId} nText={nText} nThink={nThink} />}
    </>
  );
}

/** Streaming text for an in-progress run. Best-effort; cleared when the persisted event arrives. */
function LiveBubble({ runId, nText, nThink }: { runId: string; nText: number; nThink: number }) {
  const [text, setText] = useState("");
  const [think, setThink] = useState("");
  useEffect(() => subscribeDelta((d) => {
    if (d.run !== runId) return;
    if (d.kind === "text") setText((t) => t + d.text); else setThink((t) => t + d.text);
  }), [runId]);
  useEffect(() => { setText(""); }, [nText]);
  useEffect(() => { setThink(""); }, [nThink]);
  if (!text && !think) return <p className="text-sm text-muted pl-6 pb-2">Working…</p>;
  return (
    <div className="relative pl-6 pb-3">
      <span className="absolute left-0 top-[7px] size-2 rounded-full bg-accent led-run" />
      {think && <p className="whitespace-pre-wrap break-words text-muted italic text-sm line-clamp-3 opacity-80">{think}</p>}
      {text && <p className="whitespace-pre-wrap break-words">{text}<span className="inline-block w-1.5 h-4 align-text-bottom bg-accent ml-0.5 led-run" /></p>}
    </div>
  );
}
