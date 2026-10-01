import { api } from "../lib/api";
import { useLoad } from "../lib/hooks";
import { ago } from "../lib/util";
import type { Skill } from "../lib/types";
import type { BotWithStatus } from "../lib/appdata";
import { Empty, ErrorNote, Spinner } from "../components/ui";

export function Skills({ bot }: { bot: BotWithStatus }) {
  const q = useLoad(() => api.get<Skill[]>(`/api/bots/${bot.id}/skills`), [bot.id]);
  const list = q.data ?? [];
  return (
    <div className="space-y-4">
      <p className="text-sm text-muted">Skills are written by the bot; ask it in chat to learn one. For example: "Walk me through filing an expense, then save it as a skill."</p>
      <ErrorNote error={q.error} />
      {q.loading ? <Spinner /> : list.length === 0 ? (
        <Empty title="No skills yet">After a run, skills the bot saved to its workspace appear here.</Empty>
      ) : (
        <ul className="divide-y divide-line rounded-lg border border-line bg-surface">
          {list.map((s) => (
            <li key={s.id}>
              <details className="group">
                <summary className="cursor-pointer list-none px-4 py-3 hover:bg-sunken/60">
                  <div className="flex items-baseline gap-2 flex-wrap">
                    <span className="mono font-medium">{s.name}</span>
                    <span className="text-xs text-muted ml-auto">updated {ago(s.updated_at)}</span>
                  </div>
                  {s.description && <p className="text-sm text-muted mt-0.5">{s.description}</p>}
                </summary>
                <pre className="mono whitespace-pre-wrap break-words bg-sunken px-4 py-3 text-[13px] max-h-96 overflow-auto">{s.body}</pre>
              </details>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
