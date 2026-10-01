import { useSyncExternalStore } from "react";

const raw = import.meta.env.VITE_API_URL as string | undefined;
export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
// In the desktop app the API is the built-in local server unless a URL was baked in.
export const API_URL = ((raw ?? "").trim() || (isTauri ? "http://127.0.0.1:47080" : "")).replace(/\/+$/, "");
export const isConfigured = /^https?:\/\//.test(API_URL);

export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

// ---- token store -------------------------------------------------------
const TOKEN_KEY = "familiar-token";
const tokenSubs = new Set<() => void>();
let token: string | null = null;
try { token = localStorage.getItem(TOKEN_KEY); } catch { /* ignore */ }

export const getToken = () => token;
export function setToken(t: string | null) {
  token = t;
  clearCaches();
  try { if (t) localStorage.setItem(TOKEN_KEY, t); else localStorage.removeItem(TOKEN_KEY); } catch { /* ignore */ }
  tokenSubs.forEach((f) => f());
}
export function useToken(): string | null {
  return useSyncExternalStore(
    (f) => { tokenSubs.add(f); return () => { tokenSubs.delete(f); }; },
    () => token,
  );
}

// ---- GET cache plumbing ------------------------------------------------
// `loadCache` holds the last result of each useLoad (stale-while-revalidate);
// `inflight` shares one request between identical concurrent GETs;
// `warm` holds one-shot prefetched GETs (cleared by any live notice or mutation).
export const loadCache = new Map<string, { data: unknown; json: string }>();
const inflight = new Map<string, Promise<unknown>>();
const warm = new Map<string, { p: Promise<unknown>; at: number }>();
const WARM_MS = 4000;

function invalidateRequests() { inflight.clear(); warm.clear(); }
function clearCaches() { loadCache.clear(); invalidateRequests(); }

// ---- "waking server" indicator ----------------------------------------
let slow = 0;
const slowSubs = new Set<() => void>();
const bumpSlow = (d: number) => { slow += d; slowSubs.forEach((f) => f()); };
export function useWaking(): boolean {
  return useSyncExternalStore(
    (f) => { slowSubs.add(f); return () => { slowSubs.delete(f); }; },
    () => slow > 0,
  );
}

// ---- fetch client ------------------------------------------------------
interface Opts { auth?: boolean }

async function request<T>(method: string, path: string, body?: unknown, opts: Opts = {}): Promise<T> {
  const attempts = method === "GET" ? 2 : 1;
  for (let i = 0; ; i++) {
    let counted = false;
    const timer = setTimeout(() => { counted = true; bumpSlow(1); }, 3000);
    const done = () => { clearTimeout(timer); if (counted) bumpSlow(-1); };
    let res: Response;
    try {
      res = await fetch(API_URL + path, {
        method,
        headers: {
          ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
          ...(token ? { Authorization: `Bearer ${token}` } : {}),
        },
        body: body !== undefined ? JSON.stringify(body) : undefined,
      });
    } catch {
      done();
      if (i + 1 < attempts) continue;
      throw new ApiError(0, "Can't reach the server. Check your connection and VITE_API_URL.");
    }
    done();
    const text = await res.text();
    let json: unknown = undefined;
    if (text) { try { json = JSON.parse(text); } catch { json = text; } }
    if (!res.ok) {
      const msg = (json && typeof json === "object" && "error" in json ? String((json as { error: unknown }).error) : null)
        ?? (typeof json === "string" && json ? json : res.statusText || `Request failed (${res.status})`);
      if (res.status === 401 && opts.auth !== false) setToken(null);
      throw new ApiError(res.status, msg);
    }
    if (method !== "GET") invalidateRequests();
    return json as T;
  }
}

function dedupGet<T>(path: string, opts?: Opts): Promise<T> {
  const k = (opts?.auth === false ? "na:" : "") + path;
  const w = warm.get(k);
  if (w) {
    warm.delete(k);
    if (Date.now() - w.at < WARM_MS) return w.p as Promise<T>;
  }
  const f = inflight.get(k);
  if (f) return f as Promise<T>;
  const p: Promise<T> = request<T>("GET", path, undefined, opts).finally(() => { if (inflight.get(k) === p) inflight.delete(k); });
  inflight.set(k, p);
  return p;
}

/** Warm a GET so the next identical `api.get` (within a few seconds) resolves without a round trip. */
export function prefetch(path: string) {
  if (!token) return;
  const w = warm.get(path);
  if (w && Date.now() - w.at < WARM_MS) return;
  if (inflight.has(path)) return;
  const p = request("GET", path);
  p.catch(() => { /* a failed prefetch just means the real request goes out */ });
  warm.set(path, { p, at: Date.now() });
}

export const api = {
  get: <T>(path: string, opts?: Opts) => dedupGet<T>(path, opts),
  post: <T = unknown>(path: string, body?: unknown, opts?: Opts) => request<T>("POST", path, body ?? {}, opts),
  patch: <T = unknown>(path: string, body: unknown) => request<T>("PATCH", path, body),
  put: <T = unknown>(path: string, body: unknown) => request<T>("PUT", path, body),
  del: <T = unknown>(path: string) => request<T>("DELETE", path),
};

export function qs(params: Record<string, string | number | undefined | null>): string {
  const u = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v != null && v !== "") u.set(k, String(v));
  const s = u.toString();
  return s ? `?${s}` : "";
}

// ---- live stream (one SSE connection for the whole app) ----------------
export interface Notice { t: string; id?: string | null; op?: string; run?: string | null; bot?: string | null }
type Handler = (n: Notice | null) => void; // null = resync (refetch everything you show)

const handlers = new Set<Handler>();
let es: EventSource | null = null;
let dropped = false;

export function subscribe(h: Handler): () => void {
  handlers.add(h);
  return () => { handlers.delete(h); };
}
const emit = (n: Notice | null) => { invalidateRequests(); handlers.forEach((h) => h(n)); };

export function startStream() {
  if (es || !token || !isConfigured) return;
  es = new EventSource(`${API_URL}/api/stream?token=${encodeURIComponent(token)}`);
  es.addEventListener("notice", (e) => {
    try { emit(JSON.parse((e as MessageEvent).data) as Notice); } catch { /* ignore malformed */ }
  });
  es.addEventListener("resync", () => emit(null));
  es.addEventListener("delta", (e) => {
    try { const d = JSON.parse((e as MessageEvent).data) as Delta; deltaHandlers.forEach((h) => h(d)); } catch { /* ignore */ }
  });
  es.onopen = () => { if (dropped) { dropped = false; emit(null); } };
  es.onerror = () => { dropped = true; };
}
export function stopStream() {
  es?.close();
  es = null;
  dropped = false;
}

/** Run a mutation; resolves to an error message, or null on success. */
export async function attempt(fn: () => Promise<unknown>): Promise<string | null> {
  try { await fn(); return null; } catch (e) { return e instanceof Error ? e.message : String(e); }
}

// ---- live token deltas (best-effort, never persisted) ------------------
export interface Delta { run: string; kind: "text" | "thinking"; text: string }
const deltaHandlers = new Set<(d: Delta) => void>();
export function subscribeDelta(h: (d: Delta) => void): () => void {
  deltaHandlers.add(h);
  return () => { deltaHandlers.delete(h); };
}

export function artifactUrl(id: string): string {
  return `${API_URL}/api/artifacts/${id}/download?token=${encodeURIComponent(token ?? "")}`;
}

export function liveFrameUrl(botId: string, ver: number): string {
  return `${API_URL}/api/bots/${botId}/live.jpg?token=${encodeURIComponent(token ?? "")}&t=${ver}`;
}
