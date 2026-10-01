import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { api, attempt } from "../lib/api";
import { useLoad } from "../lib/hooks";
import type { Connector } from "../lib/types";
import type { BotWithStatus } from "../lib/appdata";
import { Chip, Empty, ErrorNote, Spinner } from "../components/ui";

type Linked = (string | { id?: string; connector_id?: string })[] | { connector_ids?: string[] };

function idsOf(l: Linked | undefined): string[] {
  if (!l) return [];
  if (!Array.isArray(l)) return l.connector_ids ?? [];
  return l.map((x) => (typeof x === "string" ? x : (x.id ?? x.connector_id ?? ""))).filter(Boolean);
}

export function BotConnectors({ bot }: { bot: BotWithStatus }) {
  const all = useLoad(() => api.get<Connector[]>("/api/connectors"), []);
  const linked = useLoad(() => api.get<Linked>(`/api/bots/${bot.id}/connectors`), [bot.id]);
  const [sel, setSel] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => { if (linked.data) setSel(idsOf(linked.data)); }, [linked.data]);

  async function toggle(id: string) {
    const next = sel.includes(id) ? sel.filter((x) => x !== id) : [...sel, id];
    setSel(next); setError(null);
    const e = await attempt(() => api.put(`/api/bots/${bot.id}/connectors`, { connector_ids: next }));
    if (e) { setError(e); setSel(sel); }
  }

  if (all.loading || linked.loading) return <Spinner />;
  const list = all.data ?? [];
  return (
    <div className="space-y-4">
      <p className="text-sm text-muted">Choose which connectors {bot.name} can use. Install more on the <Link className="underline text-accent" to="/integrations">Integrations</Link> page.</p>
      <p className="rounded-md bg-warn-soft text-warn text-sm px-3 py-2">Connector tools ask for approval until you add allow rules for them on the Rules tab.</p>
      <ErrorNote error={error ?? all.error ?? linked.error} />
      {list.length === 0 ? <Empty title="No connectors installed">Add one from the Integrations page.</Empty> : (
        <ul className="divide-y divide-line rounded-lg border border-line bg-surface">
          {list.map((c) => (
            <li key={c.id}>
              <label className="flex items-center gap-3 px-4 min-h-14 cursor-pointer hover:bg-sunken/60">
                <input type="checkbox" className="size-4 accent-accent" checked={sel.includes(c.id)} onChange={() => void toggle(c.id)} />
                <span className="font-medium">{c.name}</span>
                <Chip>{c.transport}</Chip>
                {!c.enabled && <Chip tone="warn">off</Chip>}
                <span className="mono text-xs text-muted ml-auto">mcp__{c.name}__*</span>
              </label>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
