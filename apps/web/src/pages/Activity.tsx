import { Link } from "react-router-dom";
import { api } from "../lib/api";
import { useLive, useLoad } from "../lib/hooks";
import { duration, excerpt, money, when } from "../lib/util";
import type { Artifact, Run } from "../lib/types";
import type { BotWithStatus } from "../lib/appdata";
import { EventList } from "../components/EventList";
import { ArtifactView } from "../components/Artifacts";
import { Chip, Empty, Icon, RunChip, Spinner } from "../components/ui";

const ACTIVE = ["queued", "running", "waiting_approval"];

export function Activity({ bot, runId }: { bot: BotWithStatus; runId?: string }) {
  return runId ? <RunDetail bot={bot} runId={runId} /> : <RunList bot={bot} />;
}

export function RunList({ bot }: { bot: BotWithStatus }) {
  const q = useLoad(
    () => api.get<Run[]>(`/api/bots/${bot.id}/runs?limit=100`),
    [bot.id],
  );
  useLive(["runs"], () => q.reload(), { bot: bot.id });
  if (q.loading) return <Spinner />;
  if (q.error) return <p className="text-bad text-sm">{q.error}</p>;
  const runs = q.data ?? [];
  if (!runs.length) return <Empty title="No activity yet">Runs from chats and schedules will be listed here.</Empty>;
  return (
    <ul className="divide-y divide-line rounded-lg border border-line bg-surface">
      {runs.map((r) => (
        <li key={r.id}>
          <Link to={`/bot/${bot.slug}/activity/${r.id}`} className="block px-4 py-3 hover:bg-sunken/60">
            <div className="flex items-center gap-2 flex-wrap">
              <RunChip status={r.status} />
              <Chip tone={r.kind === "handoff" ? "accent" : "muted"}>{r.kind}</Chip>
              {r.parent_run_id && <span className="text-xs text-muted">from another run</span>}
              <span className="text-xs text-muted ml-auto tnum">{when(r.created_at)}</span>
            </div>
            <p className="mt-1 truncate">{excerpt(r.prompt, 140) || "(no prompt)"}</p>
            <p className="mono text-xs text-muted mt-0.5">{duration(r.started_at, r.finished_at)} · notional {money(r.cost_usd)}</p>
          </Link>
        </li>
      ))}
    </ul>
  );
}

function RunDetail({ bot, runId }: { bot: BotWithStatus; runId: string }) {
  const q = useLoad(() => api.get<Run>(`/api/runs/${runId}`), [runId]);
  useLive(["runs"], () => q.reload(), { run: runId });
  const r = q.data;
  return (
    <div>
      <Link to={`/bot/${bot.slug}/activity`} className="inline-flex items-center gap-1 min-h-10 text-sm text-muted"><Icon name="back" className="size-4" /> All runs</Link>
      {q.loading ? <Spinner /> : !r ? <p className="text-bad text-sm">{q.error ?? "Run not found."}</p> : (
        <>
          <div className="flex items-center gap-2 flex-wrap mb-2">
            <RunChip status={r.status} /><Chip tone={r.kind === "handoff" ? "accent" : "muted"}>{r.kind}</Chip>
            {r.parent_run_id && <Link className="text-xs text-accent underline" to={`/bot/${bot.slug}/activity/${r.parent_run_id}`}>from {r.kind === "handoff" ? "parent" : "earlier"} run</Link>}
            <span className="mono text-xs text-muted">{duration(r.started_at, r.finished_at)} · notional {money(r.cost_usd)}</span>
          </div>
          <p className="whitespace-pre-wrap break-words mb-2">{r.prompt}</p>
          {r.error && <p className="rounded-md bg-bad-soft text-bad text-sm px-3 py-2 mb-2 break-words">{r.error}</p>}
          <p className="text-xs text-muted mb-4">Started {when(r.started_at ?? r.created_at)}{r.thread_id && <> · <Link className="underline" to={`/bot/${bot.slug}/chat/${r.thread_id}`}>open thread</Link></>}</p>
          <RunArtifacts runId={r.id} live={ACTIVE.includes(r.status)} />
          <div className="rounded-lg border border-line bg-surface p-4">
            <EventList runId={r.id} live={ACTIVE.includes(r.status)} />
          </div>
        </>
      )}
    </div>
  );
}

function RunArtifacts({ runId, live }: { runId: string; live: boolean }) {
  const q = useLoad(() => api.get<Artifact[]>(`/api/runs/${runId}/artifacts`), [runId]);
  useLive(["artifacts", "events"], () => q.reload(), { run: runId }, live);
  const list = q.data ?? [];
  if (!list.length) return null;
  return (
    <div className="mb-4">
      <p className="text-sm font-medium mb-2">Files from this run</p>
      <div className="flex flex-wrap gap-3 items-start">{list.map((a) => <ArtifactView key={a.id} a={a} />)}</div>
    </div>
  );
}
