import { api } from "../lib/api";
import { useLive, useLoad } from "../lib/hooks";
import { when } from "../lib/util";
import type { Artifact } from "../lib/types";
import type { BotWithStatus } from "../lib/appdata";
import { ArtifactView } from "../components/Artifacts";
import { Empty, ErrorNote, Spinner } from "../components/ui";

export function Files({ bot }: { bot: BotWithStatus }) {
  const q = useLoad(() => api.get<Artifact[]>(`/api/bots/${bot.id}/artifacts`), [bot.id]);
  useLive(["artifacts", "events"], () => q.reload(), { bot: bot.id });
  const list = q.data ?? [];
  const images = list.filter((a) => a.mime?.startsWith("image/"));
  const others = list.filter((a) => !a.mime?.startsWith("image/"));
  return (
    <div className="space-y-5">
      <p className="text-sm text-muted">Screenshots and files {bot.name} saved while working.</p>
      <ErrorNote error={q.error} />
      {q.loading ? <Spinner /> : list.length === 0 ? (
        <Empty title="No files yet">Ask {bot.name} to save a result as a file, or let it take screenshots while browsing.</Empty>
      ) : (
        <>
          {images.length > 0 && (
            <div className="grid grid-cols-2 sm:grid-cols-3 gap-4">
              {images.map((a) => (
                <div key={a.id}>
                  <ArtifactView a={a} />
                  <span className="text-xs text-muted">{when(a.created_at)}</span>
                </div>
              ))}
            </div>
          )}
          {others.length > 0 && (
            <ul className="divide-y divide-line rounded-lg border border-line bg-surface">
              {others.map((a) => (
                <li key={a.id} className="flex items-center gap-3 p-3 sm:px-4">
                  <ArtifactView a={a} />
                  <span className="ml-auto text-xs text-muted tnum">{when(a.created_at)}</span>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </div>
  );
}
