import { createContext, useContext, useEffect, useMemo, useRef, type ReactNode } from "react";
import { api, startStream, stopStream } from "./api";
import { useLive, useLoad, useNow } from "./hooks";
import type { Approval, BotOverview, Overview } from "./types";
import type { MascotState } from "../components/Mascot";

export type BotWithStatus = BotOverview;

interface AppData {
  bots: BotWithStatus[];
  botsLoaded: boolean;
  pendingCount: number;
  /** Pending approvals and ask_user questions across all bots. */
  pending: Approval[];
  reloadPending: () => void;
  stateOf: (bot: BotOverview) => MascotState;
  pcOnline: boolean;
  lastSeen: string | null;
  /** Subscription-limit state from devices.info, or null when not throttled. */
  throttle: { until: Date | null } | null;
  utilization: number | null;
  error: string | null;
  reload: () => void;
}
const Ctx = createContext<AppData | null>(null);

export function AppDataProvider({ children }: { children: ReactNode }) {
  const ov = useLoad(() => api.get<Overview>("/api/overview"), []);
  const pend = useLoad(() => api.get<Approval[]>("/api/approvals?status=pending"), []);
  const now = useNow(30000);

  // one SSE connection for the whole signed-in app
  useEffect(() => { startStream(); return stopStream; }, []);
  useLive(["runs", "approvals", "bots"], () => ov.reload());
  useLive(["approvals"], () => pend.reload());
  // device heartbeats may not produce notices, so poll the overview lightly
  useEffect(() => { const i = setInterval(ov.reload, 60000); return () => clearInterval(i); }, [ov.reload]);

  // keep each bot's object identity while its data is unchanged, so memoized views skip re-rendering
  const botMemo = useRef(new Map<string, { json: string; bot: BotWithStatus }>());
  const value = useMemo<AppData>(() => {
    const d = ov.data;
    const prev = botMemo.current, next = new Map<string, { json: string; bot: BotWithStatus }>();
    const bots = (d?.bots ?? []).map((b) => {
      const nb = { ...b, status: b.paused ? ("paused" as const) : b.status };
      const json = JSON.stringify(nb);
      const old = prev.get(b.id);
      const hit = old && old.json === json ? old : { json, bot: nb };
      next.set(b.id, hit);
      return hit.bot;
    });
    botMemo.current = next;
    const pa = d?.pending_approvals;
    const seen = (d?.devices ?? []).map((x) => x.last_seen_at).filter((x): x is string => !!x).sort().pop() ?? null;
    const anyOnline = (d?.devices ?? []).some((x) => x.online ?? (x.last_seen_at != null && now - new Date(x.last_seen_at).getTime() < 3 * 60 * 1000));
    const info = (d?.devices ?? []).map((x) => x.info).find((i) => i && (i.throttled || i.utilization != null)) ?? null;
    const ra = info?.resets_at;
    const until = ra == null ? null : new Date(typeof ra === "number" ? (ra < 1e12 ? ra * 1000 : ra) : ra);
    return {
      bots,
      throttle: info?.throttled ? { until: until && !isNaN(until.getTime()) ? until : null } : null,
      utilization: info?.utilization ?? null,
      botsLoaded: !ov.loading,
      pendingCount: pend.data ? pend.data.length : Array.isArray(pa) ? pa.length : Number(pa ?? 0),
      pending: pend.data ?? [],
      reloadPending: pend.reload,
      stateOf: (b) => {
        if (b.paused || b.status === "paused") return "paused";
        if ((pend.data ?? []).some((a) => a.bot_id === b.id)) return "needs-you";
        if (b.status === "running") return "working";
        if (b.last_run_at && now - new Date(b.last_run_at).getTime() < 10 * 60 * 1000) return "done";
        return "idle";
      },
      pcOnline: anyOnline,
      lastSeen: seen,
      error: ov.error,
      reload: ov.reload,
    };
  }, [ov.data, ov.loading, ov.error, ov.reload, pend.data, pend.reload, now]);

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useApp(): AppData {
  const v = useContext(Ctx);
  if (!v) throw new Error("AppDataProvider missing");
  return v;
}
