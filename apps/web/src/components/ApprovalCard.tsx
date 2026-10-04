import { useState } from "react";
import { api } from "../lib/api";
import { useNow } from "../lib/hooks";
import { ago, errMsg, pretty } from "../lib/util";
import type { Approval } from "../lib/types";
import { useToast } from "../lib/toast";
import type { BotWithStatus } from "../lib/appdata";
import { Mascot } from "./Mascot";
import { Button, Chip, ErrorNote, inputCls } from "./ui";

/** Rough risk level shown on approval cards, like a permission broker. */
export function riskOf(tool: string): { level: "high" | "medium" | "low"; tone: "bad" | "warn" | "muted" } {
  if (tool === "Bash" || tool === "PowerShell") return { level: "high", tone: "bad" };
  if (/^(Write|Edit|MultiEdit|NotebookEdit)$/.test(tool) || /(click|type|fill|select|upload|evaluate|press)/i.test(tool) || /(create|delete|send|post|push|merge|update|write)/i.test(tool)) return { level: "medium", tone: "warn" };
  return { level: "low", tone: "muted" };
}

/** Pull the human-meaningful part out of a tool input. */
// Characters that can make displayed text differ from what runs: controls, bidi overrides/isolates, zero-width.
const HIDDEN = /[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F-\u009F​-‏‪-‮⁠-⁤⁦-⁩﻿]/g;

/** Make hidden characters visible as ⟨U+XXXX⟩ so an approval shows exactly what will run. */
export function revealHidden(text: string): { text: string; hidden: boolean } {
  let hidden = false;
  const out = text.replace(HIDDEN, (c) => {
    hidden = true;
    return `⟨U+${c.codePointAt(0)!.toString(16).toUpperCase().padStart(4, "0")}⟩`;
  });
  return { text: out, hidden };
}

export function summarizeInput(tool: string, input: Record<string, unknown> | null): { label: string; text: string } | null {
  if (!input) return null;
  const s = (k: string) => (typeof input[k] === "string" ? (input[k] as string) : null);
  if (tool === "Bash" && s("command")) return { label: "command", text: s("command")! };
  const path = s("file_path") ?? s("path") ?? s("notebook_path");
  if (path) return { label: "path", text: path };
  const url = s("url");
  if (url) return { label: "url", text: url };
  if (s("query")) return { label: "query", text: s("query")! };
  return null;
}

export function ApprovalCard({ a, botName, bot, big, onDone }: { a: Approval; botName?: string; bot?: BotWithStatus; big?: boolean; onDone?: () => void }) {
  const toast = useToast();
  const now = useNow(15000);
  const [answer, setAnswer] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showRaw, setShowRaw] = useState(false);
  const [gone, setGone] = useState(false); // optimistic: hide at once, bring back if the server refuses
  const isAsk = a.tool_name === "ask_user";
  const sum = summarizeInput(a.tool_name, a.input);
  const question = isAsk ? (typeof a.input?.question === "string" ? (a.input.question as string) : typeof a.input?.prompt === "string" ? (a.input.prompt as string) : null) : null;

  async function decide(status: "approved" | "denied") {
    setBusy(true); setError(null); setGone(true);
    const body: Record<string, unknown> = { decision: status === "approved" ? "approve" : "deny" };
    if (isAsk && status === "approved") body.response = answer;
    try {
      await api.post(`/api/approvals/${a.id}`, body);
      toast(status === "approved" ? (isAsk ? "Answer sent" : "Approved") : (isAsk ? "Skipped" : "Declined"));
      onDone?.();
    } catch (e) {
      setGone(false);
      setError(errMsg(e));
      toast(errMsg(e), "bad");
    } finally {
      setBusy(false);
    }
  }

  if (gone) return null;
  return (
    <div className="settle card !border-warn/50 overflow-hidden">
      <div className="flex flex-wrap items-center gap-2 px-4 pt-3">
        {bot && <Mascot id={bot.id} name={bot.name} avatar={bot.avatar} state="needs-you" size={30} />}
        {botName && <span className="font-medium">{botName}</span>}
        <Chip tone="warn">{isAsk ? "question" : "needs approval"}</Chip>
        {!isAsk && <Chip tone={riskOf(a.tool_name).tone}>{riskOf(a.tool_name).level} risk</Chip>}
        <span className="mono text-muted">{a.tool_name}</span>
        <span className="ml-auto text-xs text-muted tnum">{ago(a.created_at, now)}</span>
      </div>
      <div className="px-4 py-3 space-y-2">
        {question && <p className="whitespace-pre-wrap">{question}</p>}
        {sum && (() => {
          const shown = revealHidden(sum.text);
          return (
            <>
              {shown.hidden && (
                <span className="inline-block rounded-md bg-bad/10 px-2 py-0.5 text-xs font-medium text-bad">
                  Contains hidden characters — check carefully
                </span>
              )}
              <pre className="mono whitespace-pre-wrap break-all rounded-md bg-sunken border-l-4 border-warn px-3 py-2 text-[13px]">{shown.text}</pre>
            </>
          );
        })()}
        {/* The reason comes from the bot/engine, not from Familiar: never let it read like our own risk assessment. */}
        {a.reason && <p className="text-sm text-muted italic">Bot says: {revealHidden(a.reason).text}</p>}
        {a.input && !question && (
          <div>
            <button className="text-xs text-muted underline cursor-pointer" onClick={() => setShowRaw((v) => !v)}>
              {showRaw ? "Hide input" : "Show full input"}
            </button>
            {showRaw && <pre className="mono mt-1 max-h-64 overflow-auto rounded-md bg-sunken p-2 text-xs whitespace-pre-wrap break-all">{revealHidden(pretty(a.input)).text}</pre>}
          </div>
        )}
        {isAsk && (
          <textarea className={inputCls} rows={2} placeholder="Your answer" value={answer} onChange={(e) => setAnswer(e.target.value)} />
        )}
        <ErrorNote error={error} />
      </div>
      <div className="grid grid-cols-2 gap-2 px-4 pb-4">
        {isAsk ? (
          <>
            <Button big={big} onClick={() => decide("denied")} disabled={busy}>Skip</Button>
            <Button big={big} tone="primary" onClick={() => decide("approved")} disabled={busy || !answer.trim()}>Send answer</Button>
          </>
        ) : (
          <>
            <Button big={big} tone="danger" onClick={() => decide("denied")} disabled={busy}>Decline</Button>
            <Button big={big} tone="primary" onClick={() => decide("approved")} disabled={busy}>Approve</Button>
          </>
        )}
      </div>
    </div>
  );
}
