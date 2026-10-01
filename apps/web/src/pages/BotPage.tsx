import { useEffect, useRef, useState } from "react";
import { Navigate, NavLink, useParams } from "react-router-dom";
import { api } from "../lib/api";
import { useApp, type BotWithStatus } from "../lib/appdata";
import { useLive, useLoad, useNow } from "../lib/hooks";
import { ago, excerpt } from "../lib/util";
import type { Run } from "../lib/types";
import { ComputerPanel, useLiveFrame } from "../components/ComputerPanel";
import { Mascot, STATE_LABEL, type MascotState } from "../components/Mascot";
import { Button, Chip, Spinner, cx } from "../components/ui";
import { Chat } from "./Chat";
import { Activity } from "./Activity";
import { Schedules } from "./Schedules";
import { Memory } from "./Memory";
import { prefetchTab } from "../lib/prefetch";
import { Lazy, lazyPage } from "../lib/lazy";

const RulesPanel = lazyPage(() => import("./Rules").then((m) => ({ default: m.RulesPanel })));
const Settings = lazyPage(() => import("./Settings").then((m) => ({ default: m.Settings })));
const Files = lazyPage(() => import("./Files").then((m) => ({ default: m.Files })));
const Skills = lazyPage(() => import("./Skills").then((m) => ({ default: m.Skills })));
const Triggers = lazyPage(() => import("./Triggers").then((m) => ({ default: m.Triggers })));
const BotConnectors = lazyPage(() => import("./BotConnectors").then((m) => ({ default: m.BotConnectors })));

const TABS = ["chat", "activity", "files", "schedules", "triggers", "skills", "connectors", "memory", "rules", "settings"] as const;
type Tab = (typeof TABS)[number];
const TAB_LABEL: Partial<Record<Tab, string>> = { memory: "What I learned", rules: "Rules" };
const label = (t: Tab) => TAB_LABEL[t] ?? t[0].toUpperCase() + t.slice(1);

function StatusLine({ bot, state }: { bot: BotWithStatus; state: MascotState }) {
  const q = useLoad(() => api.get<Run[]>(`/api/bots/${bot.id}/runs?limit=1`), [bot.id]);
  useLive(["runs"], () => q.reload(), { bot: bot.id });
  const now = useNow(30000);
  const r = q.data?.[0];
  let text: string;
  if (state === "paused") text = "Paused. Queued work waits until you resume.";
  else if (state === "needs-you") text = "Waiting for your OK";
  else if (state === "working") text = r?.prompt ? `Working on: ${excerpt(r.prompt, 80)}` : "Working";
  else if (r?.finished_at) text = `${r.status === "failed" ? "Last run failed" : "Finished"} ${ago(r.finished_at, now)}`;
  else text = "Ready when you are";
  return <p className={cx("text-sm truncate", state === "needs-you" ? "text-warn" : "text-muted")}>{text}</p>;
}

export function BotPage() {
  const { slug = "", tab, sub } = useParams();
  const { bots, botsLoaded, stateOf } = useApp();
  const bot = bots.find((b) => b.slug === slug);

  if (!botsLoaded) return <Spinner />;
  if (!bot) return <Navigate to="/" replace />;
  if (!tab) return <Navigate to={`/bot/${slug}/chat`} replace />;
  return <BotView bot={bot} state={stateOf(bot)} tab={(TABS as readonly string[]).includes(tab) ? (tab as Tab) : "chat"} sub={sub} />;
}

function BotView({ bot, state, tab: t, sub }: { bot: BotWithStatus; state: MascotState; tab: Tab; sub?: string }) {
  const { info, ver } = useLiveFrame(bot.id);
  const [panel, setPanel] = useState(false);
  const userClosed = useRef(false);
  // the computer opens by itself the first time the bot uses its browser
  // Open the computer only once the bot is actually on a page (its browser idles on about:blank).
  useEffect(() => {
    if (info && !userClosed.current && info.url && !info.url.startsWith("about:")) setPanel(true);
  }, [info?.updated_at]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { userClosed.current = false; setPanel(false); }, [bot.id]);
  const togglePanel = () => { userClosed.current = panel; setPanel(!panel); };

  return (
    <div className="h-full flex flex-col">
      <div className="shrink-0 border-b border-line bg-surface">
        <div className="flex items-center gap-3 px-4 pt-3">
          <Mascot id={bot.id} name={bot.name} avatar={bot.avatar} state={state} size={54} />
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <h1 className="text-lg font-semibold tracking-tight truncate">{bot.name}</h1>
              <Chip tone={state === "needs-you" ? "warn" : state === "working" ? "accent" : "muted"}>{STATE_LABEL[state]}</Chip>
            </div>
            <StatusLine bot={bot} state={state} />
          </div>
          <span className="mono text-xs text-muted hidden sm:block">{bot.engine === "codex" ? "codex · " : ""}{bot.model}</span>
          {t === "chat" && <Button onClick={togglePanel} aria-pressed={panel}>{panel ? "Hide computer" : "Computer"}</Button>}
        </div>
        <nav className="no-scrollbar flex overflow-x-auto overflow-y-hidden px-2 mt-1" aria-label="Teammate sections">
          {TABS.map((x) => (
            <NavLink
              key={x}
              to={`/bot/${bot.slug}/${x}`}
              onMouseEnter={() => prefetchTab(bot.id, x)}
              onFocus={() => prefetchTab(bot.id, x)}
              className={({ isActive }) =>
                cx("px-3 min-h-10 flex items-center text-sm font-medium border-b-2 -mb-px whitespace-nowrap",
                  isActive ? "border-accent text-ink" : "border-transparent text-muted hover:text-ink")}
            >
              {label(x)}
            </NavLink>
          ))}
        </nav>
      </div>
      <div className="flex-1 min-h-0 flex">
        <div className={cx("flex-1 min-w-0 min-h-0", t !== "chat" && "overflow-auto")}>
          {t === "chat" && <Chat bot={bot} threadId={sub} />}
          {t === "activity" && <div className="max-w-3xl mx-auto p-4 md:p-6"><Activity bot={bot} runId={sub} /></div>}
          {t === "files" && <div className="max-w-3xl mx-auto p-4 md:p-6"><Lazy compact><Files bot={bot} /></Lazy></div>}
          {t === "skills" && <div className="max-w-3xl mx-auto p-4 md:p-6"><Lazy compact><Skills bot={bot} /></Lazy></div>}
          {t === "triggers" && <div className="max-w-3xl mx-auto p-4 md:p-6"><Lazy compact><Triggers bot={bot} /></Lazy></div>}
          {t === "connectors" && <div className="max-w-3xl mx-auto p-4 md:p-6"><Lazy compact><BotConnectors bot={bot} /></Lazy></div>}
          {t === "schedules" && <div className="max-w-3xl mx-auto p-4 md:p-6"><Schedules bot={bot} /></div>}
          {t === "memory" && <div className="max-w-3xl mx-auto p-4 md:p-6"><Memory bot={bot} /></div>}
          {t === "rules" && (
            <div className="max-w-3xl mx-auto p-4 md:p-6">
              <p className="text-sm text-muted mb-4">How much {bot.name} can do without asking. Global rules apply too.</p>
              <Lazy compact><RulesPanel botId={bot.id} /></Lazy>
            </div>
          )}
          {t === "settings" && <div className="max-w-xl mx-auto p-4 md:p-6"><Lazy compact><Settings bot={bot} /></Lazy></div>}
        </div>
        {t === "chat" && panel && (
          <div className="fixed inset-0 z-40 md:static md:inset-auto md:z-auto md:w-[420px] lg:w-[460px] md:shrink-0 settle">
            <ComputerPanel bot={bot} info={info} ver={ver} onClose={() => { userClosed.current = true; setPanel(false); }} />
          </div>
        )}
      </div>
    </div>
  );
}
