import { useCallback, useEffect, useRef, useState } from "react";
import { loadCache, subscribe, type Notice } from "./api";
import { errMsg } from "./util";

/** Bursts of notices (many per second during a run) collapse into one handler call per window. */
const COALESCE_MS = 150;

/**
 * Live updates. `handler` is called for notices whose table (`t`) is in `tables`
 * (and, when scope is given, that aren't for a different bot/run), and with `null`
 * after a resync/reconnect. Consumers refetch whatever the notice points at.
 * Calls are coalesced: at most one per ~150 ms (trailing), with the latest notice
 * (a resync wins over plain notices).
 */
export function useLive(
  tables: string[],
  handler: (n: Notice | null) => void,
  scope?: { bot?: string; run?: string },
  enabled = true,
) {
  const ref = useRef(handler);
  ref.current = handler;
  const key = tables.join(",");
  const bot = scope?.bot, run = scope?.run;
  useEffect(() => {
    if (!enabled) return;
    const list = key.split(",");
    let timer: ReturnType<typeof setTimeout> | undefined;
    let pending: Notice | null | undefined;
    let has = false;
    const fire = () => {
      timer = undefined;
      const n = pending as Notice | null;
      has = false; pending = undefined;
      ref.current(n);
    };
    const queue = (n: Notice | null) => {
      pending = n === null || (has && pending === null) ? null : n;
      has = true;
      if (timer === undefined) timer = setTimeout(fire, COALESCE_MS);
    };
    const off = subscribe((n) => {
      if (n === null) return queue(null);
      if (!list.includes(n.t)) return;
      if (bot && n.bot && n.bot !== bot) return;
      if (run && n.run && n.run !== run) return;
      queue(n);
    });
    return () => { off(); if (timer !== undefined) clearTimeout(timer); };
  }, [key, bot, run, enabled]);
}

export interface Loaded<T> {
  data: T | undefined; error: string | null; loading: boolean; reload: () => void;
  set: React.Dispatch<React.SetStateAction<T | undefined>>;
}

interface LoadState<T> { key: string; data: T | undefined; error: string | null; loading: boolean }

/** Cache key: the loader's source plus its deps, so each call site + inputs has its own entry. */
function keyOf(fn: () => unknown, deps: unknown[]): string | null {
  try { return fn.toString() + "|" + JSON.stringify(deps); } catch { return null; }
}
let uncached = 0;

/** Remember a result; if it is identical to what we hold, hand back the old reference so nothing re-renders. */
function remember<T>(key: string, d: T): T {
  if (key.startsWith("~")) return d;
  let json: string;
  try { json = JSON.stringify(d) ?? ""; } catch { return d; }
  const old = loadCache.get(key);
  if (old && old.json === json) return old.data as T;
  loadCache.set(key, { data: d, json });
  return d;
}

/**
 * Run an async loader when deps change; `reload()` refetches without flashing the loading state.
 * Stale-while-revalidate: the last result for the same loader + deps shows immediately (no
 * skeleton) while a fresh one is fetched; unchanged results keep their reference.
 */
export function useLoad<T>(fn: () => Promise<T>, deps: unknown[]): Loaded<T> {
  const idRef = useRef<string>("");
  if (!idRef.current) idRef.current = "~" + ++uncached;
  const key = keyOf(fn, deps) ?? idRef.current + "|" + deps.length;
  const [tick, setTick] = useState(0);
  const [st0, setSt] = useState<LoadState<T>>(() => {
    const c = loadCache.get(key);
    return { key, data: c?.data as T | undefined, error: null, loading: !c };
  });
  let st = st0;
  if (st.key !== key) {
    // inputs changed: show cached data for the new inputs if we have it, else keep what was on screen
    const c = loadCache.get(key);
    st = { key, data: c ? (c.data as T) : st.data, error: null, loading: !c && st.data === undefined };
    setSt(st);
  }
  const fnRef = useRef(fn);
  fnRef.current = fn;
  useEffect(() => {
    let dead = false;
    fnRef.current().then(
      (d) => {
        if (dead) return;
        const data = remember(key, d);
        setSt((s) => (s.key !== key ? s : s.data === data && s.error === null && !s.loading ? s : { key, data, error: null, loading: false }));
      },
      (e: unknown) => { if (!dead) setSt((s) => (s.key !== key ? s : { ...s, error: errMsg(e), loading: false })); },
    );
    return () => { dead = true; };
  }, [key, tick]);
  const reload = useCallback(() => setTick((t) => t + 1), []);
  const set = useCallback<React.Dispatch<React.SetStateAction<T | undefined>>>((u) => {
    setSt((s) => {
      const data = typeof u === "function" ? (u as (p: T | undefined) => T | undefined)(s.data) : u;
      if (data !== undefined) remember(s.key, data);
      return { ...s, data };
    });
  }, []);
  return { data: st.data, error: st.error, loading: st.loading, reload, set };
}

export function useNow(ms = 20000): number {
  const [n, setN] = useState(() => Date.now());
  useEffect(() => { const i = setInterval(() => setN(Date.now()), ms); return () => clearInterval(i); }, [ms]);
  return n;
}

export function useTheme(): [string, () => void] {
  const [t, setT] = useState<string>(() => {
    const cur = document.documentElement.dataset.theme;
    if (cur) return cur;
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  });
  const toggle = () => {
    const next = t === "dark" ? "light" : "dark";
    document.documentElement.dataset.theme = next;
    try { localStorage.setItem("familiar-theme", next); } catch { /* ignore */ }
    setT(next);
  };
  return [t, toggle];
}
