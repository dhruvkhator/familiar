import { useState } from "react";
import { api, setToken } from "../lib/api";
import { errMsg } from "../lib/util";
import { useLoad } from "../lib/hooks";
import { Button, ErrorNote, Field, Spinner, inputCls } from "../components/ui";

export function Login() {
  const state = useLoad(() => api.get<{ setup_needed: boolean }>("/api/auth/state", { auth: false }), []);
  const setup = state.data?.setup_needed === true;
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true); setError(null);
    try {
      const r = await api.post<{ token: string }>(setup ? "/api/auth/setup" : "/api/auth/login", { email: email.trim(), password }, { auth: false });
      setToken(r.token);
    } catch (err) {
      setError(errMsg(err));
      setBusy(false);
    }
  }

  return (
    <div className="min-h-full flex items-center justify-center p-6">
      <div className="w-full max-w-sm">
        <h1 className="text-3xl font-semibold tracking-tight mb-1">Familiar</h1>
        <p className="text-muted mb-8">
          {setup ? "First run. Create the owner account for this server." : "Your always-on teammates. Sign in to check on them."}
        </p>
        {state.loading ? <Spinner /> : state.error ? (
          <div className="space-y-3">
            <ErrorNote error={state.error} />
            <Button onClick={state.reload}>Try again</Button>
          </div>
        ) : (
          <form onSubmit={submit} className="space-y-4">
            <Field label="Email">
              <input type="email" required autoFocus autoComplete="email" className={inputCls} value={email} onChange={(e) => setEmail(e.target.value)} placeholder="you@example.com" />
            </Field>
            <Field label="Password" hint={setup ? "At least 10 characters." : undefined}>
              <input type="password" required minLength={setup ? 10 : 1} autoComplete={setup ? "new-password" : "current-password"} className={inputCls} value={password} onChange={(e) => setPassword(e.target.value)} />
            </Field>
            <ErrorNote error={error} />
            <Button tone="primary" big type="submit" className="w-full" disabled={busy || !email || (setup && password.length < 10) || !password}>
              {busy ? "Please wait…" : setup ? "Create owner account" : "Sign in"}
            </Button>
          </form>
        )}
      </div>
    </div>
  );
}

export function Unconfigured() {
  return (
    <div className="min-h-full flex items-center justify-center p-6">
      <div className="w-full max-w-lg">
        <h1 className="text-2xl font-semibold tracking-tight mb-2">Connect to your Familiar server</h1>
        <p className="text-muted mb-4">The app needs the address of your familiar-server. Add it to <span className="mono">apps/web/.env</span> (or your Vercel project settings), then restart the dev server or rebuild.</p>
        <pre className="mono rounded-lg bg-sunken border border-line p-4 text-[13px] overflow-auto">VITE_API_URL=https://your-familiar-server.onrender.com</pre>
        <p className="text-sm text-muted mt-4"><span className="mono">VITE_API_URL</span> is missing or isn't an http(s) URL.</p>
      </div>
    </div>
  );
}
