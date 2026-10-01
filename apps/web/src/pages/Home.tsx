import { useEffect, useState } from "react";
import { prefetchBot } from "../lib/prefetch";
import { Link, useOutletContext } from "react-router-dom";
import { api, subscribeDelta } from "../lib/api";
import { useApp, type BotWithStatus } from "../lib/appdata";
import { useLive, useLoad, useNow } from "../lib/hooks";
import { ago, excerpt, until } from "../lib/util";
import type { Run, Schedule } from "../lib/types";
import { ApprovalCard } from "../components/ApprovalCard";
import { Mascot, STATE_LABEL } from "../components/Mascot";
import { Button, Chip, RunChip, cx } from "../components/ui";

const ACTIVE = ["queued", "running", "waiting_approval"];

function greeting(): string {
  const h = new Date().getHours();
  return h < 5 ? "Still up?" : h < 12 ? "Good morning" : h < 18 ? "Good afternoon" : "Good evening";
}

export function Home() {
  const { bots, botsLoaded, pending, reloadPending, reload, stateOf } = useApp();
  const { openCreate } = useOutletContext<{ openCreate: () => void }>();
  const now = useNow(30000);
  const key = bots.map((b) => b.id).join(",");

  const runs = useLoad(async () => {
    const lists = await Promise.all(bots.slice(0, 10).map(async (b) => {
      try { return await api.get<Run[]>(`/api/bots/${b.id}/runs?limit=6`); } catch { return [] as Run[]; }
    }));
    return lists.flat().sort((a, b) => b.created_at.localeCompare(a.created_at));
  }, [key]);
  useLive(["runs"], () => runs.reload());

  const sched = useLoad(async () => {
    const lists = await Promise.all(bots.slice(0, 10).map(async (b) => {
      try { return await api.get<Schedule[]>(`/api/bots/${b.id}/schedules`); } catch { return [] as Schedule[]; }
    }));
    return lists.flat();
  }, [key]);
  useLive(["schedules"], () => sched.reload());

  const botOf = (id: string) => bots.find((b) => b.id === id);
  const active = (runs.data ?? []).filter((r) => ACTIVE.includes(r.status));
  const done = (runs.data ?? []).filter((r) => !ACTIVE.includes(r.status)).slice(0, 6);
  const upcoming = (sched.data ?? []).filter((s) => s.enabled && s.next_run_at).sort((a, b) => a.next_run_at!.localeCompare(b.next_run_at!)).slice(0, 5);

  if (!botsLoaded) return <div className="p-8 max-w-3xl mx-auto space-y-3"><div className="skeleton h-10 w-60" /><div className="skeleton h-24" /><div className="skeleton h-40" /></div>;

  if (bots.length === 0) {
    return (
      <div className="max-w-md mx-auto p-8 text-center">
        <Mascot id="empty" name="Familiar" state="idle" size={140} className="mb-4" />
        <h1 className="text-2xl font-semibold tracking-tight mb-1">Meet your first teammate</h1>
        <p className="text-muted mb-5">Name it, give it a look and a job, and it starts working beside you.</p>
        <Button tone="primary" big onClick={openCreate}>Create a teammate</Button>
      </div>
    );
  }

  return (
    <div className="max-w-3xl mx-auto p-4 md:p-8 space-y-8">
      <header>
        <h1 className="text-2xl md:text-3xl font-semibold tracking-tight">{greeting()}</h1>
        <p className="text-muted">
          {pending.length > 0 ? `${pending.length} thing${pending.length === 1 ? "" : "s"} waiting on you.` : active.length > 0 ? `${active.length} task${active.length === 1 ? "" : "s"} in progress.` : "Everything is quiet."}
        </p>
      </header>

      <section aria-label="Your teammates" className="flex gap-4 overflow-x-auto no-scrollbar pb-1">
        {bots.map((b) => {
          const st = stateOf(b);
          return (
            <Link key={b.id} to={`/bot/${b.slug}`} onMouseEnter={() => prefetchBot(b.id)} onFocus={() => prefetchBot(b.id)} className="flex flex-col items-center gap-1 w-24 shrink-0 rounded-[14px] p-2 hover:bg-sunken/70">
              <Mascot id={b.id} name={b.name} avatar={b.avatar} state={st} size={64} />
              <span className="text-sm font-medium truncate max-w-full">{b.name}</span>
              <span className={cx("text-xs", st === "needs-you" ? "text-warn" : "text-muted")}>{STATE_LABEL[st]}</span>
            </Link>
          );
        })}
        <button onClick={openCreate} className="flex flex-col items-center gap-1 w-24 shrink-0 rounded-[14px] p-2 text-muted hover:bg-sunken/70 cursor-pointer" aria-label="New teammate">
          <span className="size-16 rounded-full border-2 border-dashed border-line flex items-center justify-center text-2xl">+</span>
          <span className="text-sm">New</span>
        </button>
      </section>

      {pending.length > 0 && (
        <section className="space-y-3" aria-label="Needs you">
          <h2 className="text-lg font-semibold tracking-tight">Needs you</h2>
          {pending.map((a) => <ApprovalCard key={a.id} a={a} bot={botOf(a.bot_id)} botName={a.bot_name ?? botOf(a.bot_id)?.name} onDone={() => { reloadPending(); reload(); }} />)}
        </section>
      )}

      {active.length > 0 && (
        <section className="space-y-3" aria-label="Happening now">
          <h2 className="text-lg font-semibold tracking-tight">Happening now</h2>
          {active.map((r) => <NowCard key={r.id} run={r} bot={botOf(r.bot_id)} />)}
        </section>
      )}

      <section className="space-y-3" aria-label="Recently done">
        <h2 className="text-lg font-semibold tracking-tight">Recently done</h2>
        {runs.loading ? <div className="skeleton h-16" /> : done.length === 0 ? (
          <p className="text-sm text-muted card px-4 py-5">Finished work will appear here. Say hello to a teammate to get started.</p>
        ) : (
          <ul className="card divide-y divide-line">
            {done.map((r) => {
              const b = botOf(r.bot_id);
              return (
                <li key={r.id}>
                  <Link to={`/bot/${b?.slug ?? ""}/activity/${r.id}`} className="flex items-center gap-3 px-4 py-3 hover:bg-sunken/60">
                    {b && <Mascot id={b.id} name={b.name} avatar={b.avatar} size={30} state={r.status === "succeeded" ? "done" : "idle"} />}
                    <span className="min-w-0 flex-1">
                      <span className="block truncate">{excerpt(r.prompt, 90) || "(no prompt)"}</span>
                      <span className="block text-xs text-muted">{b?.name} · {ago(r.finished_at ?? r.created_at, now)}</span>
                    </span>
                    <RunChip status={r.status} />
                  </Link>
                </li>
              );
            })}
          </ul>
        )}
      </section>

      {upcoming.length > 0 && (
        <section className="space-y-3" aria-label="Coming up">
          <h2 className="text-lg font-semibold tracking-tight">Coming up</h2>
          <ul className="card divide-y divide-line">
            {upcoming.map((s) => {
              const b = botOf(s.bot_id);
              return (
                <li key={s.id} className="flex items-center gap-3 px-4 py-3">
                  {b && <Mascot id={b.id} name={b.name} avatar={b.avatar} size={30} />}
                  <span className="min-w-0 flex-1">
                    <span className="block truncate">{excerpt(s.prompt, 80)}</span>
                    <span className="block text-xs text-muted">{b?.name}</span>
                  </span>
                  <Chip tone={s.kind === "proactive" ? "accent" : "muted"}>{until(s.next_run_at, now)}</Chip>
                </li>
              );
            })}
          </ul>
        </section>
      )}
    </div>
  );
}

function NowCard({ run, bot }: { run: Run; bot?: BotWithStatus }) {
  const [live, setLive] = useState("");
  useEffect(() => subscribeDelta((d) => {
    if (d.run === run.id && d.kind === "text") setLive((t) => (t + d.text).slice(-220));
  }), [run.id]);
  return (
    <Link to={`/bot/${bot?.slug ?? ""}/chat/${run.thread_id}`} className="card flex items-start gap-3 p-4 hover:bg-sunken/40">
      {bot && <Mascot id={bot.id} name={bot.name} avatar={bot.avatar} state="working" size={40} />}
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-2"><span className="font-medium">{bot?.name}</span><RunChip status={run.status} /></span>
        <span className="block text-sm text-muted truncate">{excerpt(run.prompt, 100)}</span>
        {live && <span className="block text-sm mt-1 line-clamp-2 whitespace-pre-wrap">{live}</span>}
      </span>
    </Link>
  );
}
