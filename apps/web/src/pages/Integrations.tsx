import { useState } from "react";
import { Link } from "react-router-dom";
import { api, attempt } from "../lib/api";
import { useLive, useLoad } from "../lib/hooks";
import { errMsg } from "../lib/util";
import { useApp } from "../lib/appdata";
import type { Channel, Connector, ConnectorPreset } from "../lib/types";
import { Button, Chip, Empty, ErrorNote, Field, Spinner, inputCls } from "../components/ui";

export function Integrations() {
  return (
    <div className="max-w-3xl mx-auto p-4 md:p-8 space-y-10">
      <div>
        <h1 className="text-xl font-semibold tracking-tight mb-1">Integrations</h1>
        <p className="text-sm text-muted">Give your bots tools and ways to reach you.</p>
      </div>
      <Connectors />
      <Channels />
    </div>
  );
}

// ---------------------------------------------------------------- connectors

type Editing = { preset?: ConnectorPreset; connector?: Connector } | null;

function Connectors() {
  const presets = useLoad(() => api.get<ConnectorPreset[]>("/api/connectors/presets"), []);
  const list = useLoad(() => api.get<Connector[]>("/api/connectors"), []);
  useLive(["connectors"], () => list.reload());
  const [editing, setEditing] = useState<Editing>(null);
  const [error, setError] = useState<string | null>(null);

  async function toggle(c: Connector) {
    const e = await attempt(() => api.patch(`/api/connectors/${c.id}`, { enabled: !c.enabled }));
    if (e) setError(e); else list.reload();
  }
  async function del(c: Connector) {
    if (!confirm(`Delete connector "${c.name}"? Bots lose access to its tools.`)) return;
    const e = await attempt(() => api.del(`/api/connectors/${c.id}`));
    if (e) setError(e); else list.reload();
  }

  const installed = list.data ?? [];
  return (
    <section className="space-y-4">
      <div>
        <h2 className="text-lg font-semibold tracking-tight">Connectors</h2>
        <p className="text-sm text-muted">Connectors are MCP servers that give bots tools like GitHub or Notion. Secrets are stored encrypted and never shown again.</p>
      </div>
      <p className="rounded-md bg-warn-soft text-warn text-sm px-3 py-2">
        Connector tools ask for your approval until you add allow rules for them, for example <code className="mono">mcp__github__list_issues</code> on the <Link className="underline" to="/rules">rules page</Link>.
      </p>
      <ErrorNote error={error ?? list.error} />

      {editing && (
        <ConnectorForm
          key={editing.connector?.id ?? editing.preset?.id ?? "custom"}
          preset={editing.preset}
          connector={editing.connector}
          onCancel={() => setEditing(null)}
          onDone={() => { setEditing(null); list.reload(); }}
        />
      )}

      <h3 className="text-sm font-medium">Installed</h3>
      {list.loading ? <Spinner /> : installed.length === 0 ? (
        <Empty title="No connectors yet">Pick one from the catalog below, or add your own MCP server.</Empty>
      ) : (
        <ul className="divide-y divide-line rounded-lg border border-line bg-surface">
          {installed.map((c) => (
            <li key={c.id} className="p-4 flex items-start gap-3 flex-wrap">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2 flex-wrap">
                  <span className="font-medium">{c.name}</span>
                  <Chip>{c.transport}</Chip>
                  {c.has_secrets && <Chip tone="ok">secrets stored</Chip>}
                  {!c.enabled && <Chip tone="warn">off</Chip>}
                </div>
                <p className="mono text-xs text-muted truncate">{c.transport === "stdio" ? [c.command, ...(c.args ?? [])].join(" ") : c.url}</p>
              </div>
              <label className="flex items-center gap-2 text-sm cursor-pointer min-h-9">
                <input type="checkbox" className="size-4 accent-accent" checked={c.enabled} onChange={() => toggle(c)} /> Enabled
              </label>
              <Button tone="ghost" onClick={() => setEditing({ connector: c })}>Edit</Button>
              <Button tone="ghost" onClick={() => del(c)}>Delete</Button>
            </li>
          ))}
        </ul>
      )}

      <div className="flex items-center justify-between">
        <h3 className="text-sm font-medium">Catalog</h3>
        <Button onClick={() => setEditing({})}>Add custom MCP server</Button>
      </div>
      {presets.loading ? <Spinner /> : presets.error ? <ErrorNote error={presets.error} /> : (
        <div className="grid sm:grid-cols-2 gap-3">
          {(presets.data ?? []).map((p) => (
            <div key={p.id} className="rounded-lg border border-line bg-surface p-4 flex flex-col gap-2">
              <div className="flex items-center gap-2 flex-wrap">
                <span className="font-medium">{p.name}</span>
                <Chip>{p.transport}</Chip>
                {p.verify && <Chip tone="warn">verify</Chip>}
              </div>
              <p className="text-sm text-muted flex-1">{p.description}</p>
              {p.verify && <p className="text-xs text-warn">The package name wasn't confirmed. Check the docs before installing.</p>}
              <div className="flex items-center gap-2">
                <Button tone="primary" onClick={() => setEditing({ preset: p })}>Install</Button>
                {p.docs_url && <a className="text-xs text-accent underline" href={p.docs_url} target="_blank" rel="noreferrer">Docs</a>}
              </div>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

function parseLines(text: string, sep: RegExp): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of text.split("\n")) {
    const t = line.trim();
    if (!t) continue;
    const m = t.match(sep);
    if (!m || m.index == null) continue;
    out[t.slice(0, m.index).trim()] = t.slice(m.index + m[0].length).trim();
  }
  return out;
}

function ConnectorForm({ preset, connector, onCancel, onDone }: { preset?: ConnectorPreset; connector?: Connector; onCancel: () => void; onDone: () => void }) {
  const editing = !!connector;
  const [name, setName] = useState(connector?.name ?? preset?.id ?? "");
  const [transport, setTransport] = useState<"stdio" | "http">(connector?.transport ?? preset?.transport ?? "stdio");
  const [command, setCommand] = useState(connector?.command ?? preset?.command ?? "");
  const [args, setArgs] = useState((connector?.args ?? preset?.args ?? []).join(" "));
  const [url, setUrl] = useState(connector?.url ?? preset?.url ?? "");
  const [secretVals, setSecretVals] = useState<Record<string, string>>({});
  const [extra, setExtra] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const isPreset = !!preset && !editing;
  const existingNames = [...(connector?.env_names ?? []), ...(connector?.header_names ?? [])];

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true); setError(null);
    const bucket = transport === "stdio" ? "env" : "headers";
    const secrets: Record<string, string> = {};
    for (const [k, v] of Object.entries(secretVals)) if (v) secrets[k] = v;
    Object.assign(secrets, transport === "stdio" ? parseLines(extra, /=/) : parseLines(extra, /:/));
    const body: Record<string, unknown> = {
      name: name.trim(), transport,
      ...(preset ? { preset: preset.id } : {}),
      ...(transport === "stdio"
        ? { command: command.trim(), args: args.trim() ? args.trim().split(/\s+/) : [] }
        : { url: url.trim() }),
    };
    if (Object.keys(secrets).length) body.secrets = { [bucket]: secrets };
    const err = await attempt(() => (editing ? api.patch(`/api/connectors/${connector!.id}`, body) : api.post("/api/connectors", body)));
    setBusy(false);
    if (err) setError(err); else onDone();
  }

  return (
    <form onSubmit={submit} className="settle rounded-lg border border-accent/50 bg-surface p-4 space-y-3">
      <p className="font-medium">{editing ? `Edit ${connector!.name}` : isPreset ? `Install ${preset!.name}` : "Custom MCP server"}</p>
      <Field label="Name" hint={<>Tools appear as <code className="mono">mcp__{name || "name"}__*</code>. Use letters, digits and dashes.</>}>
        <input className={inputCls} value={name} onChange={(e) => setName(e.target.value)} required />
      </Field>
      {!isPreset && (
        <Field label="Type">
          <select className={inputCls} value={transport} onChange={(e) => setTransport(e.target.value as "stdio" | "http")}>
            <option value="stdio">Local command (stdio)</option>
            <option value="http">Remote URL (http)</option>
          </select>
        </Field>
      )}
      {transport === "stdio" ? (
        <>
          <Field label="Command"><input className={inputCls + " mono"} value={command} onChange={(e) => setCommand(e.target.value)} placeholder="npx" required readOnly={isPreset} /></Field>
          <Field label="Arguments" hint="Separated by spaces."><input className={inputCls + " mono"} value={args} onChange={(e) => setArgs(e.target.value)} placeholder="-y @modelcontextprotocol/server-memory" readOnly={isPreset} /></Field>
        </>
      ) : (
        <Field label="URL"><input className={inputCls + " mono"} type="url" value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://example.com/mcp" required readOnly={isPreset} /></Field>
      )}
      {isPreset && preset!.secret_fields.map((f) => (
        <Field key={f.key} label={f.label} hint={f.help}>
          <input className={inputCls + " mono"} type="password" autoComplete="off" value={secretVals[f.key] ?? ""} onChange={(e) => setSecretVals({ ...secretVals, [f.key]: e.target.value })} />
        </Field>
      ))}
      {!isPreset && (
        <Field label={transport === "stdio" ? "Environment variables" : "Headers"} hint={transport === "stdio" ? "One per line: NAME=value" : "One per line: Header: value"}>
          <textarea className={inputCls + " mono"} rows={3} autoComplete="off" value={extra} onChange={(e) => setExtra(e.target.value)} placeholder={transport === "stdio" ? "API_KEY=..." : "Authorization: Bearer ..."} />
        </Field>
      )}
      {editing && connector!.has_secrets && (
        <p className="text-xs text-muted">
          Stored secrets{existingNames.length ? `: ${existingNames.join(", ")}` : ""}. Leave the secret fields empty to keep them. Entering any replaces all of them.
        </p>
      )}
      {editing && connector!.preset && (connector!.preset !== "custom") && !isPreset && (
        <p className="text-xs text-muted">To change a preset's secrets, add them as {transport === "stdio" ? "NAME=value" : "Header: value"} lines above.</p>
      )}
      <ErrorNote error={error} />
      <div className="flex justify-end gap-2">
        <Button type="button" onClick={onCancel}>Cancel</Button>
        <Button tone="primary" type="submit" disabled={busy || !name.trim()}>{editing ? "Save changes" : "Install"}</Button>
      </div>
    </form>
  );
}

// ------------------------------------------------------------------ channels

function Channels() {
  const { bots } = useApp();
  const q = useLoad(() => api.get<Channel[]>("/api/channels"), []);
  useLive(["channels"], () => q.reload());
  const tg = (q.data ?? []).find((c) => c.kind === "telegram");

  return (
    <section className="space-y-4">
      <div>
        <h2 className="text-lg font-semibold tracking-tight">Channels</h2>
        <p className="text-sm text-muted">Talk to your bots and approve actions from your phone's chat app.</p>
      </div>
      <ErrorNote error={q.error} />
      {q.loading ? <Spinner /> : tg ? (
        <TelegramStatus ch={tg} onChange={q.reload} />
      ) : (
        <TelegramWizard onDone={q.reload} defaultBot={bots[0]?.id ?? ""} />
      )}
    </section>
  );
}

function BotSelect({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const { bots } = useApp();
  return (
    <select className={inputCls} value={value} onChange={(e) => onChange(e.target.value)}>
      {bots.length === 0 && <option value="">No bots yet</option>}
      {bots.map((b) => <option key={b.id} value={b.id}>{b.name}</option>)}
    </select>
  );
}

function TelegramWizard({ onDone, defaultBot }: { onDone: () => void; defaultBot: string }) {
  const [token, setToken] = useState("");
  const [bot, setBot] = useState(defaultBot);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const botId = bot || defaultBot;

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true); setError(null);
    try {
      await api.post("/api/channels", { kind: "telegram", token: token.trim(), default_bot_id: botId });
      onDone();
    } catch (err) { setError(errMsg(err)); setBusy(false); }
  }
  return (
    <form onSubmit={submit} className="rounded-lg border border-line bg-surface p-4 space-y-4">
      <p className="font-medium">Connect Telegram</p>
      <ol className="space-y-4 list-decimal pl-5 marker:text-muted">
        <li>
          <p className="text-sm">Open <span className="mono">@BotFather</span> in Telegram, send <span className="mono">/newbot</span>, and follow the prompts. It gives you a token.</p>
        </li>
        <li>
          <Field label="Paste the bot token">
            <input className={inputCls + " mono"} type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value)} placeholder="123456789:AA..." required />
          </Field>
        </li>
        <li>
          <Field label="Default bot" hint="Plain messages go to this bot. Use /bot <slug> <text> to reach another.">
            <BotSelect value={botId} onChange={setBot} />
          </Field>
        </li>
      </ol>
      <ErrorNote error={error} />
      <Button tone="primary" type="submit" disabled={busy || !token.trim() || !botId}>Save and get pairing code</Button>
    </form>
  );
}

function TelegramStatus({ ch, onChange }: { ch: Channel; onChange: () => void }) {
  const [error, setError] = useState<string | null>(null);
  async function patch(body: Record<string, unknown>) {
    const e = await attempt(() => api.patch(`/api/channels/${ch.id}`, body));
    if (e) setError(e); else onChange();
  }
  async function del() {
    if (!confirm("Disconnect Telegram?")) return;
    const e = await attempt(() => api.del(`/api/channels/${ch.id}`));
    if (e) setError(e); else onChange();
  }
  return (
    <div className="rounded-lg border border-line bg-surface p-4 space-y-4">
      <div className="flex items-center gap-2 flex-wrap">
        <span className="font-medium">Telegram</span>
        {ch.bound ? <Chip tone="ok">connected</Chip> : <Chip tone="warn">waiting for pairing</Chip>}
        {!ch.enabled && <Chip tone="warn">off</Chip>}
      </div>
      {!ch.bound && (
        <div className="rounded-md bg-sunken p-3 space-y-1" role="status">
          <p className="text-sm">Open your new bot in Telegram and send this message:</p>
          <p className="mono text-base font-medium break-all select-all">/start {ch.pair_code ?? "…"}</p>
          <p className="text-xs text-muted">This page updates when pairing succeeds. Only that chat can talk to your bots.</p>
        </div>
      )}
      <Field label="Default bot">
        <BotSelect value={ch.default_bot_id ?? ""} onChange={(v) => void patch({ default_bot_id: v })} />
      </Field>
      <ErrorNote error={error} />
      <div className="flex items-center gap-3">
        <label className="flex items-center gap-2 text-sm cursor-pointer min-h-9">
          <input type="checkbox" className="size-4 accent-accent" checked={ch.enabled} onChange={() => void patch({ enabled: !ch.enabled })} /> Enabled
        </label>
        <Button tone="ghost" className="ml-auto" onClick={del}>Disconnect</Button>
      </div>
    </div>
  );
}
