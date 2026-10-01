import { api } from "../lib/api";
import { useApp } from "../lib/appdata";
import { useLive, useLoad } from "../lib/hooks";
import type { Approval } from "../lib/types";
import { ApprovalCard } from "../components/ApprovalCard";
import { Empty, ErrorNote, Spinner } from "../components/ui";

export function Approvals() {
  const { bots, reload } = useApp();
  const q = useLoad(() => api.get<Approval[]>("/api/approvals?status=pending"), []);
  useLive(["approvals"], () => q.reload());
  const pending = q.data ?? [];
  const name = (a: Approval) => a.bot_name ?? bots.find((b) => b.id === a.bot_id)?.name ?? "Unknown bot";
  return (
    <div className="max-w-2xl mx-auto p-4 md:p-8">
      <h1 className="text-xl font-semibold tracking-tight mb-1">Approvals</h1>
      <p className="text-sm text-muted mb-4">Bots pause here until you decide. Requests expire after 30 minutes.</p>
      <ErrorNote error={q.error} />
      {q.loading ? <Spinner /> : pending.length === 0 ? (
        <Empty title="Nothing needs you right now">When a bot wants to run something risky, it shows up here.</Empty>
      ) : (
        <div className="space-y-3">
          {pending.map((a) => <ApprovalCard key={a.id} a={a} big botName={name(a)} bot={bots.find((b) => b.id === a.bot_id)} onDone={() => { q.reload(); reload(); }} />)}
        </div>
      )}
    </div>
  );
}
