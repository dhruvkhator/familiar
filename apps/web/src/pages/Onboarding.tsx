import { useEffect, useState } from "react";
import { api, setToken } from "../lib/api";
import { cliStatus, invokeCmd, type AppStatus, type CliStatus } from "../lib/desktop";
import { errMsg, kebab } from "../lib/util";
import type { Bot, BotEngine, Thread } from "../lib/types";
import { Mascot, randomAvatar, type Avatar } from "../components/Mascot";
import { AvatarBuilder } from "../components/AvatarBuilder";
import { EngineFields } from "../components/EngineFields";
import { Button, ErrorNote, Field, inputCls } from "../components/ui";

function Frame({ children, mascot = "working" }: { children: React.ReactNode; mascot?: "working" | "idle" | "needs-you" | "done" }) {
  return (
    <div className="min-h-full flex items-center justify-center p-6 bg-[radial-gradient(60%_50%_at_50%_0%,var(--accent-soft),transparent)]">
      <div className="w-full max-w-md text-center">
        <Mascot id="familiar-welcome" name="Familiar" state={mascot} size={96} className="mb-5" />
        {children}
      </div>
    </div>
  );
}

/** Desktop first run: wait for the built-in server, check Claude, then create the account. */
export function DesktopSetup() {
  const [step, setStep] = useState<"boot" | "claude" | "account">("boot");
  const [boot, setBoot] = useState<AppStatus | null>(null);
  const [bootErr, setBootErr] = useState<string | null>(null);
  const [claude, setClaude] = useState<CliStatus | null>(null);
  const [codex, setCodex] = useState<CliStatus | null>(null);

  // 1. boot progress
  useEffect(() => {
    if (step !== "boot") return;
    let dead = false;
    const tick = async () => {
      try {
        const s = await invokeCmd<AppStatus>("app_status");
        if (dead) return;
        setBoot(s); setBootErr(null);
        if (s.boot.phase === "ready") { setStep("claude"); return; }
      } catch (e) { if (!dead) setBootErr(errMsg(e)); }
      if (!dead) setTimeout(tick, 1000);
    };
    void tick();
    return () => { dead = true; };
  }, [step]);

  // 2. engine check: at least one of Claude Code / Codex must be signed in
  const [checking, setChecking] = useState(false);
  async function checkClaude() {
    setChecking(true);
    const [c, x] = await Promise.all([cliStatus("claude_status"), cliStatus("codex_status")]);
    setClaude(c); setCodex(x);
    if ((c.installed && c.logged_in) || (x.installed && x.logged_in)) {
      try { localStorage.setItem("familiar-engine-hint", c.installed && c.logged_in ? "claude" : "codex"); } catch { /* ignore */ }
      setStep("account");
    }
    setChecking(false);
  }
  useEffect(() => { if (step === "claude") void checkClaude(); }, [step]);

  if (step === "boot") {
    const failed = boot?.boot.phase === "error";
    return (
      <Frame mascot={failed ? "needs-you" : "working"}>
        <h1 className="text-2xl font-semibold tracking-tight mb-2">{failed ? "Setup hit a problem" : "Getting Familiar ready"}</h1>
        <p className="text-muted" role="status">{boot?.boot.message || "Starting up…"}</p>
        {failed && <p className="text-bad text-sm mt-3 break-words">{boot?.boot.message}</p>}
        {bootErr && <p className="text-xs text-muted mt-3">Waiting for the app to respond…</p>}
        {!failed && <div className="mt-6 h-1.5 rounded-full bg-sunken overflow-hidden"><div className="h-full w-1/3 rounded-full bg-accent skeleton" /></div>}
      </Frame>
    );
  }

  if (step === "claude") {
    const ready = !!claude && !!codex;
    return (
      <Frame mascot={ready ? "needs-you" : "working"}>
        <h1 className="text-2xl font-semibold tracking-tight mb-2">Connect an AI account</h1>
        <p className="text-muted">Familiar runs your teammates on your own Claude or Codex subscription. Sign in to at least one.</p>
        <div className="mt-4 space-y-3 text-left">
          <CliCard name="Claude Code" s={claude} install="Install it from claude.com/code, then run `claude` in a terminal and sign in." login="Run `claude` in a terminal and sign in with your Claude account." />
          <CliCard name="Codex" s={codex} install="Install the Codex CLI (npm i -g @openai/codex), then run `codex` and sign in." login="Run `codex` in a terminal and sign in with your ChatGPT account." />
        </div>
        <div className="mt-5 flex flex-col gap-2 items-center">
          <Button tone="primary" big onClick={() => void checkClaude()} disabled={checking}>{checking ? "Checking…" : "Check again"}</Button>
        </div>
      </Frame>
    );
  }
  return <Account />;
}

/** Create the owner account (first run) or sign in. Used by the desktop flow. */
function Account() {
  const [setup, setSetup] = useState<boolean | null>(null);
  const [email, setEmail] = useState("me@familiar.local");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api.get<{ setup_needed: boolean }>("/api/auth/state", { auth: false }).then((s) => setSetup(s.setup_needed), (e) => setError(errMsg(e)));
  }, []);
  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true); setError(null);
    try {
      const r = await api.post<{ token: string }>(setup ? "/api/auth/setup" : "/api/auth/login", { email: email.trim(), password }, { auth: false });
      setToken(r.token);
    } catch (err) { setError(errMsg(err)); setBusy(false); }
  }
  return (
    <Frame mascot="idle">
      <h1 className="text-2xl font-semibold tracking-tight mb-2">{setup ? "Create your account" : "Welcome back"}</h1>
      <p className="text-muted mb-6">{setup ? "This stays on your computer. Any email works; it's only a login name." : "Sign in to your Familiar."}</p>
      <form onSubmit={submit} className="space-y-4 text-left">
        <Field label="Email"><input className={inputCls} type="email" required value={email} onChange={(e) => setEmail(e.target.value)} /></Field>
        <Field label="Password" hint={setup ? "At least 10 characters." : undefined}>
          <input className={inputCls} type="password" required autoFocus minLength={setup ? 10 : 1} value={password} onChange={(e) => setPassword(e.target.value)} autoComplete={setup ? "new-password" : "current-password"} />
        </Field>
        <ErrorNote error={error} />
        <Button tone="primary" big type="submit" className="w-full" disabled={busy || setup === null || !password || (!!setup && password.length < 10)}>
          {busy ? "Please wait…" : setup ? "Create account" : "Sign in"}
        </Button>
      </form>
    </Frame>
  );
}

/** "Meet your first teammate": name, color, what it does, model, then it introduces itself. */
export function FirstTeammate({ onCreated }: { onCreated: (path: string, slug: string) => void }) {
  const [name, setName] = useState("");
  const [avatar, setAvatar] = useState<Avatar>(() => randomAvatar());
  const [about, setAbout] = useState("");
  const [engine, setEngine] = useState<BotEngine>(() => { try { return localStorage.getItem("familiar-engine-hint") === "codex" ? "codex" : "claude"; } catch { return "claude"; } });
  const [model, setModel] = useState(() => (engine === "codex" ? "gpt-5-codex" : "sonnet"));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true); setError(null);
    try {
      const persona = `You are ${name.trim()}, a personal AI teammate.${about.trim() ? " " + about.trim() : ""}`;
      const bot = await api.post<Bot>("/api/bots", { name: name.trim(), slug: kebab(name) || "teammate", persona, model, engine, avatar });
      const thread = await api.post<Thread>(`/api/bots/${bot.id}/threads`, { title: "Getting started" });
      await api.post(`/api/threads/${thread.id}/messages`, {
        content: "Introduce yourself: say hi, tell me in a few sentences what you can do for me, and ask what I'd like to start with.",
      });
      onCreated(`/bot/${bot.slug}/chat/${thread.id}`, bot.slug);
    } catch (err) { setError(errMsg(err)); setBusy(false); }
  }

  return (
    <div className="min-h-full flex items-center justify-center p-6 bg-[radial-gradient(60%_50%_at_50%_0%,var(--accent-soft),transparent)]">
      <form onSubmit={submit} className="w-full max-w-md">
        <div className="text-center mb-6">
          <Mascot id="preview" name={name || "Your teammate"} avatar={avatar} state={busy ? "working" : "idle"} size={120} className="mb-3" />
          <h1 className="text-2xl font-semibold tracking-tight">Meet your first teammate</h1>
          <p className="text-muted">Give it a name, a look and a job. You can change everything later.</p>
        </div>
        <div className="card p-5 space-y-4">
          <Field label="Name"><input className={inputCls} autoFocus required value={name} onChange={(e) => setName(e.target.value)} placeholder="Mochi" /></Field>
          <details className="rounded-[12px] border border-line p-3" open>
            <summary className="cursor-pointer text-sm font-medium">Make it yours</summary>
            <div className="mt-3"><AvatarBuilder value={avatar} onChange={setAvatar} name={name} /></div>
          </details>
          <Field label="What should it do?" hint="A sentence or two. For example: keeps an eye on my GitHub and tells me what needs review.">
            <textarea className={inputCls} rows={3} value={about} onChange={(e) => setAbout(e.target.value)} />
          </Field>
          <EngineFields engine={engine} model={model} onChange={(en, m) => { setEngine(en); setModel(m); }} />
          <ErrorNote error={error} />
          <Button tone="primary" big type="submit" className="w-full" disabled={busy || !name.trim()}>{busy ? "Waking up…" : "Create and say hello"}</Button>
        </div>
      </form>
    </div>
  );
}

function CliCard({ name, s, install, login }: { name: string; s: CliStatus | null; install: string; login: string }) {
  const ok = !!s?.installed && s.logged_in;
  return (
    <div className="card p-4">
      <div className="flex items-center gap-2">
        <span className="font-medium">{name}</span>
        <span className={"text-xs rounded px-1.5 py-0.5 " + (ok ? "bg-accent-soft text-ok" : "bg-sunken text-muted")}>
          {!s ? "checking" : ok ? "signed in" : s.installed ? "not signed in" : "not installed"}
        </span>
        {s?.version && <span className="mono text-xs text-muted ml-auto">{s.version}</span>}
      </div>
      {s && !ok && <p className="text-sm text-muted mt-1">{s.installed ? login : install}</p>}
    </div>
  );
}
