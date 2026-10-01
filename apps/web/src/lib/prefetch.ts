import { prefetch } from "./api";

/** Warm the GETs a screen will make; cheap and one-shot, never cancels anything. */
export function prefetchBot(botId: string) {
  prefetch(`/api/bots/${botId}/threads`);
  prefetch(`/api/bots/${botId}/runs?limit=1`);
}

export function prefetchThread(threadId: string) {
  prefetch(`/api/threads/${threadId}/messages?limit=200`);
  prefetch(`/api/threads/${threadId}/runs?limit=5`);
}

export function prefetchTab(botId: string, tab: string) {
  switch (tab) {
    case "chat": return prefetch(`/api/bots/${botId}/threads`);
    case "activity": return prefetch(`/api/bots/${botId}/runs?limit=100`);
    case "files": return prefetch(`/api/bots/${botId}/artifacts`);
    case "schedules": return prefetch(`/api/bots/${botId}/schedules`);
    case "triggers": return prefetch(`/api/bots/${botId}/triggers`);
    case "skills": return prefetch(`/api/bots/${botId}/skills`);
    case "memory": return prefetch(`/api/bots/${botId}/memories`);
    case "connectors": prefetch("/api/connectors"); return prefetch(`/api/bots/${botId}/connectors`);
    case "rules": prefetch("/api/rules"); return prefetch(`/api/rules?bot_id=${botId}`);
  }
}
