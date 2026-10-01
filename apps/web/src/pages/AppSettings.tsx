import { useCallback, useEffect, useState } from "react";
import { api, attempt, isTauri } from "../lib/api";
import { cliStatus, invokeCmd, type AppStatus, type CliStatus } from "../lib/desktop";
import { useToast } from "../lib/toast";
import { errMsg } from "../lib/util";
import { Button, Chip, ErrorNote, Field, Icon, inputCls } from "../components/ui";
import pkg from "../../package.json";

type Cli = "claude" | "codex";
interface CodexStatus extends CliStatus { needs_update?: boolean }

const META: Record<Cli, { name: string; install: string; blurb: string }> = {
  claude: { name: "Claude Code", install: "npm i -g @anthropic-ai/claude-code", blurb: "Your Claude subscription runs the teammates set to Claude." },
  codex: { name: "Codex", install: "npm i -g @openai/codex@latest", blurb: "Your ChatGPT plan runs the teammates set to Codex." },
};

function Section({ title, hint, children }: { title: string; hint?: string; children: React.ReactNode }) {
  return (
    <section className="space-y-3">
      <div>
        <h2 className="text-base font-semibold">{title}</h2>
        {hint && <p className="text-sm text-muted">{hint}</p>}
      </div>
      {children}
    </section>
  );
}

function CliCard({ which, st, onChange }: { which: Cli; st: CodexStatus | null; onChange: () => void }) {
  const toast = useToast();
  const m = META[which];
  const run = async (action: string) => {
    try { await invokeCmd<void>("open_cli_terminal", { action }); } catch (e) { toast(errMsg(e), "bad"); }
  };
  const copy = async () => {
    try { await navigator.clipboard.writeText(m.install); toast("Copied"); } catch { toast("Couldn't copy", "bad"); }
  };
  let chip: React.ReactNode = <Chip>Checking…</Chip>;
  if (st) {
    if (!st.installed) chip = <Chip tone="warn">Not installed</Chip>;
    else if (st.needs_update) chip = <Chip tone="warn">Update needed</Chip>;
    else if (!st.logged_in) chip = <Chip tone="warn">Not signed in</Chip>;
    else chip = <Chip tone="ok">Signed in ✓{st.auth_method ? ` via ${st.auth_method}` : ""}{st.version ? ` · ${st.version}` : ""}</Chip>;
  }
  return (
    <div className="card p-4 space-y-3">
      <div className="flex items-center gap-3">
        <span className="size-9 rounded-[10px] bg-accent-soft text-accent grid place-items-center">
          <Icon name={which === "claude" ? "bots" : "plug"} />
        </span>
        <div className="min-w-0 flex-1">
          <p className="font-medium">{m.name}</p>
          <p className="text-xs text-muted">{m.blurb}</p>
        </div>
      </div>
      <div>{chip}</div>
      {st && !st.installed && (
        <div className="flex items-center gap-2 rounded-[10px] bg-sunken px-3 py-2">
          <code className="mono text-xs flex-1 break-all">{m.install}</code>
          <Button tone="ghost" onClick={() => void copy()} aria-label="Copy install command">Copy</Button>
        </div>
      )}
      <div className="flex flex-wrap gap-2">
        {st && !st.installed ? (
          <Button tone="primary" onClick={() => void run(`${which}_install`)}>Install</Button>
        ) : (
          <Button tone={st?.logged_in ? "default" : "primary"} disabled={!st} onClick={() => void run(`${which}_login`)}>
            {st?.logged_in ? "Switch account" : "Sign in"}
          </Button>
        )}
        {which === "codex" && st?.needs_update && <Button tone="primary" onClick={() => void run("codex_update")}>Update Codex</Button>}
        <Button tone="ghost" onClick={onChange}>Check again</Button>
      </div>
    </div>
  );
}

function AiAccounts() {
  const [claude, setClaude] = useState<CodexStatus | null>(null);
  const [codex, setCodex] = useState<CodexStatus | null>(null);
  const check = useCallback(async () => {
    const [c, x] = await Promise.all([cliStatus("claude_status"), cliStatus("codex_status")]);
    setClaude(c); setCodex(x as CodexStatus);
  }, []);
  useEffect(() => {
    if (!isTauri) return;
    void check();
    const f = () => void check();
    window.addEventListener("focus", f);
    return () => window.removeEventListener("focus", f);
  }, [check]);

  if (!isTauri) {
    return (
      <Section title="AI accounts">
        <p className="card p-4 text-sm text-muted">Signing in to Claude Code and Codex happens on the computer running Familiar. Open the Familiar desktop app there to manage these.</p>
      </Section>
    );
  }
  return (
    <Section title="AI accounts" hint="A terminal window opens and your browser finishes the sign-in. Then click Check again.">
      <div className="grid gap-3 md:grid-cols-2">
        <CliCard which="claude" st={claude} onChange={() => void check()} />
        <CliCard which="codex" st={codex} onChange={() => void check()} />
      </div>
    </Section>
  );
}

function Account() {
  const toast = useToast();
  const [email, setEmail] = useState("");
  const [orig, setOrig] = useState("");
  const [cur, setCur] = useState("");
  const [next, setNext] = useState("");
  const [again, setAgain] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api.get<{ email: string }>("/api/me").then((m) => { setEmail(m.email); setOrig(m.email); }, () => { /* shown elsewhere */ });
  }, []);

  const emailChanged = email.trim().toLowerCase() !== orig;
  const mismatch = next !== "" && again !== "" && next !== again;
  async function save(e: React.FormEvent) {
    e.preventDefault();
    if (next && next !== again) { setError("The new passwords don't match."); return; }
    if (next && next.length < 10) { setError("New password must be at least 10 characters."); return; }
    setBusy(true); setError(null);
    let out: { email: string } | undefined;
    const err = await attempt(async () => {
      out = await api.patch<{ email: string }>("/api/auth/account", {
        current_password: cur,
        ...(emailChanged ? { email } : {}),
        ...(next ? { new_password: next } : {}),
      });
    });
    setBusy(false);
    if (err) { setError(err); toast(err, "bad"); return; }
    if (out) { setEmail(out.email); setOrig(out.email); }
    setCur(""); setNext(""); setAgain("");
    toast(next ? "Account updated. Other devices were signed out." : "Account updated");
  }
  return (
    <Section title="Your account">
      <form onSubmit={save} className="card p-4 space-y-4 max-w-lg">
        <Field label="Email"><input className={inputCls} type="email" autoComplete="email" value={email} onChange={(e) => setEmail(e.target.value)} required /></Field>
        <Field label="New password" hint="At least 10 characters. Leave blank to keep the current one.">
          <input className={inputCls} type="password" autoComplete="new-password" value={next} onChange={(e) => setNext(e.target.value)} />
        </Field>
        {next && (
          <Field label="Confirm new password" hint={mismatch ? <span className="text-bad">Doesn't match.</span> : undefined}>
            <input className={inputCls} type="password" autoComplete="new-password" value={again} onChange={(e) => setAgain(e.target.value)} />
          </Field>
        )}
        <Field label="Current password" hint="Needed to save any change.">
          <input className={inputCls} type="password" autoComplete="current-password" value={cur} onChange={(e) => setCur(e.target.value)} required />
        </Field>
        <ErrorNote error={error} />
        <Button tone="primary" type="submit" disabled={busy || !cur || (!emailChanged && !next) || mismatch}>Save changes</Button>
      </form>
    </Section>
  );
}

function Computer() {
  const toast = useToast();
  const [app, setApp] = useState<AppStatus | null>(null);
  useEffect(() => {
    if (isTauri) invokeCmd<AppStatus>("app_status").then(setApp, () => { /* older desktop build */ });
  }, []);
  if (!isTauri) return null;
  return (
    <Section title="This computer">
      <div className="card p-4 space-y-3 text-sm">
        <div className="flex items-center justify-between gap-3">
          <span className="text-muted">Built-in database</span>
          <span>{app ? (app.boot.embedded ? "Yes" : "No, using your own") : "…"}</span>
        </div>
        <div className="flex items-start justify-between gap-3">
          <span className="text-muted shrink-0">Config file</span>
          <span className="mono text-xs break-all text-right">{app?.daemon.config_path ?? "…"}</span>
        </div>
        <Button onClick={() => { invokeCmd<void>("open_bots_folder").catch((e: unknown) => toast(errMsg(e), "bad")); }}>Open data folder</Button>
      </div>
    </Section>
  );
}

export function AppSettings() {
  return (
    <div className="max-w-3xl mx-auto p-4 md:p-8 space-y-10">
      <div>
        <h1 className="text-xl font-semibold tracking-tight mb-1">Settings</h1>
        <p className="text-sm text-muted">Accounts, this computer, and about.</p>
      </div>
      <AiAccounts />
      <Account />
      <Computer />
      <Section title="About">
        <div className="card p-4 text-sm space-y-1">
          <p>Familiar <span className="mono">v{pkg.version}</span></p>
          <p className="text-muted">How it all fits together is written up in <span className="mono">docs/ARCHITECTURE.md</span> in the Familiar folder.</p>
        </div>
      </Section>
    </div>
  );
}
