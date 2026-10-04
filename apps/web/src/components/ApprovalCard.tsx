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
// Characters that can make displayed text differ from what runs, by Unicode category so nothing slips through:
// Cc controls (except \t and \n), Cf format (bidi overrides/isolates, zero-width, soft hyphen, word joiner, BOM,
// Arabic letter mark, Mongolian vowel separator, the U+E0000 "tag" block used for ASCII smuggling), Zl/Zp line and
// paragraph separators, Co private use, Cn unassigned — plus blank-looking letters/marks those categories miss:
// combining grapheme joiner, Hangul fillers, Mongolian free variation selectors, variation selectors.
const HIDDEN =
  /(?![\t\n])[\p{Cc}\p{Cf}\p{Zl}\p{Zp}\p{Co}\p{Cn}͏ᅟᅠ᠋-᠍ㅤﾠ︀-️\u{E0100}-\u{E01EF}]/gu;

/** Make hidden characters visible as ⟨U+XXXX⟩ so an approval shows exactly what will run. */
export function revealHidden(text: string): { text: string; hidden: boolean } {
  let hidden = false;
  const out = text.replace(HIDDEN, (c) => {
    hidden = true;
    return `⟨U+${c.codePointAt(0)!.toString(16).toUpperCase().padStart(4, "0")}⟩`;
  });
  return { text: out, hidden };
}

/** Every input field except the one shown as the summary — an approval must never hide parameters. */
export function otherInputs(input: Record<string, unknown> | null, shownKey: string | null): [string, string][] {
  if (!input) return [];
  return Object.entries(input)
    .filter(([k]) => k !== shownKey)
    .map(([k, v]) => [k, typeof v === "string" ? v : JSON.stringify(v, null, 2)]);
}

export function summarizeInput(
  tool: string,
  input: Record<string, unknown> | null,
): { label: string; text: string; key: string } | null {
  if (!input) return null;
  const s = (k: string) => (typeof input[k] === "string" ? (input[k] as string) : null);
  if (tool === "Bash" && s("command")) return { label: "command", text: s("command")!, key: "command" };
  const pathKey = ["file_path", "path", "notebook_path"].find((k) => s(k));
  if (pathKey) return { label: "path", text: s(pathKey)!, key: pathKey };
  if (s("url")) return { label: "url", text: s("url")!, key: "url" };
  if (s("query")) return { label: "query", text: s("query")!, key: "query" };
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
        {question && <p className="whitespace-pre-wrap">{revealHidden(question).text}</p>}
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
        {!isAsk && (() => {
          // Every other parameter is shown too (no blind approvals): e.g. a url plus a body, a query plus a recipient.
          const extra = otherInputs(a.input, sum?.key ?? null);
          if (!extra.length) return null;
          return (
            <dl className="space-y-1.5">
              {extra.map(([k, v]) => {
                const shown = revealHidden(v);
                return (
                  <div key={k}>
                    <dt className="text-xs text-muted">
                      {k}
                      {shown.hidden && <span className="ml-2 font-medium text-bad">hidden characters</span>}
                    </dt>
                    <dd>
                      <pre className="mono max-h-64 overflow-auto whitespace-pre-wrap break-all rounded-md bg-sunken px-3 py-1.5 text-xs">{shown.text}</pre>
                    </dd>
                  </div>
                );
              })}
            </dl>
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
