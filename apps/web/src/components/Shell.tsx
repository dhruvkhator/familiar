import { memo, useMemo, useState } from "react";
import { Link, NavLink, Outlet } from "react-router-dom";
import { api, setToken } from "../lib/api";
import { useApp, type BotWithStatus } from "../lib/appdata";
import { useLive, useLoad, useTheme } from "../lib/hooks";
import { ago, excerpt } from "../lib/util";
import type { Thread } from "../lib/types";
import { prefetchBot, prefetchThread } from "../lib/prefetch";
import { Badge, Button, cx, Icon, Led } from "./ui";
import { CreateBotDialog } from "./CreateBotDialog";
import { Mascot, STATE_LABEL, type MascotState } from "./Mascot";

function PcStatus() {
  const { pcOnline, lastSeen, utilization, throttle } = useApp();
  const util = utilization != null ? ` · subscription use ${Math.round(utilization * 100)}%` : "";
  return (
    <div className="flex items-center gap-2 text-sm" title={(lastSeen ? `Last heartbeat ${ago(lastSeen)}` : "No heartbeat yet") + util}>
      <Led status={!pcOnline ? "offline" : throttle ? "paused" : "online"} />
      <span>{pcOnline ? "Computer online" : "Computer offline"}</span>
      {!pcOnline && <span className="text-xs text-muted">{lastSeen ? ago(lastSeen) : "never seen"}</span>}
    </div>
  );
}

const navCls = ({ isActive }: { isActive: boolean }) =>
  cx(
    "flex items-center gap-3 rounded-[10px] px-3 min-h-10 text-sm font-medium",
    isActive ? "bg-accent-soft text-accent" : "text-muted hover:text-ink hover:bg-sunken/70",
  );

const RecentChats = memo(function RecentChats({ bots }: { bots: BotWithStatus[] }) {
  const key = bots.map((b) => b.id).join(",");
  const q = useLoad(async () => {
    const lists = await Promise.all(bots.slice(0, 8).map(async (b) => {
      try { return (await api.get<Thread[]>(`/api/bots/${b.id}/threads`)).slice(0, 4).map((t) => ({ t, b })); } catch { return []; }
    }));
    return lists.flat().sort((x, y) => y.t.updated_at.localeCompare(x.t.updated_at)).slice(0, 7);
  }, [key]);
  useLive(["threads", "runs"], () => q.reload());
  const items = q.data ?? [];
  if (!items.length) return <p className="px-3 text-sm text-muted">Chats you start show up here.</p>;
  return (
    <div className="space-y-0.5">
      {items.map(({ t, b }) => (
        <NavLink key={t.id} to={`/bot/${b.slug}/chat/${t.id}`} onMouseEnter={() => prefetchThread(t.id)} onFocus={() => prefetchThread(t.id)}
          className={({ isActive }) => cx("flex items-center gap-2 rounded-[10px] px-3 min-h-9 text-sm", isActive ? "bg-sunken text-ink" : "text-muted hover:text-ink hover:bg-sunken/70")}>
          <span className="truncate">{excerpt(t.title || "Untitled", 28)}</span>
          <span className="ml-auto text-[11px] text-muted shrink-0">{b.name}</span>
        </NavLink>
      ))}
    </div>
  );
}, (a, b) => a.bots.length === b.bots.length && a.bots.every((x, i) => x.id === b.bots[i].id));

const BotNavItem = memo(function BotNavItem({ b, st }: { b: BotWithStatus; st: MascotState }) {
  return (
    <NavLink to={`/bot/${b.slug}`} onMouseEnter={() => prefetchBot(b.id)} onFocus={() => prefetchBot(b.id)} className={({ isActive }) => cx("flex items-center gap-3 rounded-[10px] px-2 py-1.5", isActive ? "bg-accent-soft" : "hover:bg-sunken/70")}>
      <Mascot id={b.id} name={b.name} avatar={b.avatar} state={st} size={30} />
      <span className="min-w-0">
        <span className="block text-sm font-medium truncate">{b.name}</span>
        <span className={cx("block text-xs truncate", st === "needs-you" ? "text-warn" : "text-muted")}>{STATE_LABEL[st]}</span>
      </span>
    </NavLink>
  );
});

export function Shell() {
  const { bots, pendingCount, stateOf } = useApp();
  const [theme, toggle] = useTheme();
  const [creating, setCreating] = useState(false);
  const ctx = useMemo(() => ({ openCreate: () => setCreating(true) }), []);

  return (
    <div className="h-full flex flex-col md:flex-row">
      {/* left rail (desktop) */}
      <aside className="hidden md:flex w-64 shrink-0 flex-col border-r border-line bg-surface">
        <div className="px-4 pt-4 pb-3 flex items-center justify-between">
          <Link to="/" className="text-lg font-semibold tracking-tight">Familiar</Link>
          <Button tone="ghost" onClick={toggle} aria-label="Toggle theme"><Icon name={theme === "dark" ? "sun" : "moon"} /></Button>
        </div>
        <nav className="px-2 flex-1 overflow-auto space-y-5">
          <div className="space-y-0.5">
            <NavLink to="/" end className={navCls}><Icon name="bots" /> Today</NavLink>
            <NavLink to="/approvals" className={navCls}><Icon name="inbox" /> Needs you <Badge n={pendingCount} /></NavLink>
          </div>
          <div>
            <div className="flex items-center justify-between px-3 mb-1">
              <span className="text-xs font-medium text-muted">Teammates</span>
              <button onClick={() => setCreating(true)} className="text-muted hover:text-ink cursor-pointer" aria-label="New teammate"><Icon name="plus" className="size-4" /></button>
            </div>
            <div className="space-y-0.5">
              {bots.map((b) => <BotNavItem key={b.id} b={b} st={stateOf(b)} />)}
              {bots.length === 0 && <p className="px-3 text-sm text-muted">No teammates yet.</p>}
            </div>
          </div>
          <div>
            <span className="block px-3 mb-1 text-xs font-medium text-muted">Recent chats</span>
            <RecentChats bots={bots} />
          </div>
        </nav>
        <div className="border-t border-line p-2 space-y-0.5">
          <NavLink to="/integrations" className={navCls}><Icon name="plug" /> Integrations</NavLink>
          <NavLink to="/rules" className={navCls}><Icon name="shield" /> Global rules</NavLink>
          <NavLink to="/settings" className={navCls}><Icon name="cog" /> Settings</NavLink>
          <div className="px-3 py-2"><PcStatus /></div>
          <Button tone="ghost" className="w-full justify-start" onClick={() => void signOut()}>
            <Icon name="out" className="size-4" /> Sign out
          </Button>
        </div>
      </aside>

      {/* mobile top strip */}
      <header className="md:hidden flex items-center justify-between px-4 h-12 border-b border-line bg-surface shrink-0">
        <span className="font-semibold tracking-tight">Familiar</span>
        <div className="flex items-center gap-1">
          <PcStatus />
          <Button tone="ghost" onClick={toggle} aria-label="Toggle theme"><Icon name={theme === "dark" ? "sun" : "moon"} /></Button>
        </div>
      </header>

      <main className="flex-1 min-h-0 min-w-0 overflow-auto flex flex-col">
        <ThrottleBanner />
        <div className="flex-1 min-h-0">
          <Outlet context={ctx} />
        </div>
      </main>

      {/* mobile bottom tab bar */}
      <nav className="md:hidden shrink-0 grid grid-cols-6 border-t border-line bg-surface safe-b">
        {[
          { to: "/", icon: "bots", label: "Today", end: true },
          { to: "/approvals", icon: "inbox", label: "Needs you", n: pendingCount },
          { to: "/rules", icon: "shield", label: "Rules" },
          { to: "/integrations", icon: "plug", label: "Connect" },
          { to: "/settings", icon: "cog", label: "Settings" },
        ].map((t) => (
          <NavLink key={t.to} to={t.to} end={t.end} className={({ isActive }) => cx("relative flex flex-col items-center justify-center gap-0.5 min-h-14 text-xs", isActive ? "text-accent" : "text-muted")}>
            <Icon name={t.icon} />
            {t.label}
            {t.n ? <span className="absolute top-1.5 left-1/2 ml-2 min-w-4 h-4 px-1 rounded-full bg-warn text-[10px] leading-4 text-black text-center font-semibold">{t.n}</span> : null}
          </NavLink>
        ))}
        <button onClick={() => void signOut()} className="flex flex-col items-center justify-center gap-0.5 min-h-14 text-xs text-muted cursor-pointer">
          <Icon name="out" />Sign out
        </button>
      </nav>

      {creating && <CreateBotDialog onClose={() => setCreating(false)} />}
    </div>
  );
}

async function signOut() {
  try { await api.post("/api/auth/logout"); } catch { /* token may already be invalid */ }
  setToken(null);
}

function ThrottleBanner() {
  const { throttle } = useApp();
  if (!throttle) return null;
  const t = throttle.until?.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  return (
    <div role="status" className="shrink-0 bg-warn-soft text-warn text-sm px-4 py-2">
      Scheduled work paused{t ? ` until ${t}` : ""} (subscription limit). Chats still run.
    </div>
  );
}
