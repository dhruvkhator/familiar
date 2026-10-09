# Familiar (formerly zed) — architecture (v0 contract, reviewed)

Open-source, self-hosted "always-on AI teammate" in the spirit of OpenAI Dots / xAI Grok Bot / Hermes Agent.
A **bot** has its own computer (workspace + browser + terminal), remembers what you teach it, runs on schedules
without being asked, and stops for your approval before doing anything risky. Control it from the desktop app or
your phone (web). Single-owner, self-hosted.

## Hard constraints
- **LLM = the user's Claude subscription** via the locally installed Claude Code CLI (`claude`), like t3code.
  We never touch OAuth tokens; we spawn `claude` and it uses its own login. Never pass `--bare` (it ignores OAuth).
  Later, for containers: `claude setup-token` → `CLAUDE_CODE_OAUTH_TOKEN`.
- **No vendor lock-in.** Storage is *any* PostgreSQL ≥ 13 (Supabase, Neon, RDS, local) used as a plain database:
  no RLS, no provider auth, no provider realtime, no pg_cron, no provider SDK in the frontend. Files: any
  S3-compatible store (R2). Free tiers: Postgres on Supabase/Neon, API on Render, web on Vercel, files on R2.
- **No Electron.** Rust: Tauri 2 desktop shell, Rust daemon, Rust API server.

## Topology
```
 browser / phone ─> apps/web (Vercel static SPA) ─┐
 desktop window ──> apps/web bundled in Tauri ────┤  HTTPS: REST + SSE, bearer token
                                                  ▼
                        familiar-server (Rust/axum, Render free web service)
                          ├─ owner auth (argon2 password → session token)
                          └─ REST CRUD, SSE live stream (LISTEN Familiar → per-owner fan-out)
                                                  │ sqlx
                                                  ▼
                        PostgreSQL (any provider)  ← migrations/ (sqlx, run at startup)
                                                  ▲ sqlx, LISTEN Familiar as a wake-up signal
 your PC: familiar-core daemon (in Tauri tray app, or `familiard`)
   ├─ run queue (per-thread FIFO, per-bot serial, throttled by rate_limit_event)
   ├─ fires schedules + event retention (replaces pg_cron)
   ├─ spawns `claude -p` per run, stream-json both ways, answers can_use_tool on stdin
   ├─ permission decisions (rules → reviewer → human approval)
   ├─ pings familiar-server /healthz every 10 min (keeps the free Render instance awake while the PC is on)
   └─ R2 uploads (P2)
 bot computer = ~/.familiar/bots/<slug>/ (workspace, .claude/skills, .browser profile); instructions in ~/.familiar/bots/.prompts/<slug>.md
```
The UI only talks to familiar-server. familiar-server and the daemon are both trusted backend components of this codebase and
share the database; the daemon never opens a port. Tauri commands exist only for local things (daemon status, open
workspace folder). Render free instances sleep after 15 min idle (cold start ~30–60 s): the daemon's ping keeps it
warm, and the UI shows a "waking server…" state on slow first requests.

## Repo layout
```
Cargo.toml                 workspace
migrations/                plain SQL migrations (sqlx), shared by server and daemon
crates/familiar-core/           daemon: db, queue, claude protocol, permissions, workspace
crates/familiard/               headless daemon binary
crates/familiar-server/         HTTP API (axum) + Dockerfile for Render
apps/desktop/src-tauri/    Tauri 2: tray (close-to-tray), notifications, hosts familiar-core; frontend = apps/web/dist
apps/web/                  Vite + React + TS + Tailwind SPA (API client only)
```

## Data model
Every domain table has `owner_id uuid not null references users(id)`. There is no row-level security: **every
query in both server and daemon filters by owner** (server: the session's user; daemon: `config.owner_id`).

- `users(id, email unique, password_hash argon2id, created_at)`; `sessions(token_hash sha256-hex pk, user_id,
  created_at, expires_at)`. First-run setup creates the owner; after that setup is closed (single-owner).
- `bots(id, slug unique per owner, name, persona, model default 'sonnet', paused bool default false, created_at)`
  — running/idle is derived (view `bot_status(id, status, last_run_at)`).
- `threads(id, bot_id, title, claude_session_id uuid null, schedule_id null, created_at, updated_at)`
- `messages(id, thread_id, role user|assistant|system, content, run_id null, created_at)`
- `runs(id, bot_id, thread_id not null, kind chat|scheduled|proactive|handoff, prompt, status
   queued|running|waiting_approval|succeeded|failed|cancelled, error, cost_usd numeric (notional, list price),
   usage jsonb, parent_run_id null, started_at, finished_at, created_at)`
- `events(id bigserial, run_id, seq int, kind, payload jsonb, created_at, unique(run_id, seq))`
  kinds `status|text|thinking|tool_call|tool_result|approval|artifact|error|result|rate_limit`. Payloads:
  text/thinking `{text}`; tool_call `{id,name,input}`; tool_result `{tool_use_id,content,is_error}`;
  approval `{approval_id?,tool_name,input?,status,decided_by?,reason}`; error `{message}`;
  result `{text,subtype,num_turns,cost_usd,duration_ms,permission_denials}`. Strings > 32 KB are truncated
  (`payload.truncated=true`). The daemon deletes events older than 30 days, daily.
- `approvals(id, run_id, bot_id, tool_use_id, tool_name, input jsonb, reason, status
   pending|approved|denied|expired, decided_by user|rule|reviewer, response text null, decided_at, created_at)`
  — also used for `ask_user` questions (tool_name `ask_user`, `input.question`, answer in `response`).
- `rules(id, bot_id null = all bots, pattern, decision allow|deny|ask|review, note, created_at)`
- `schedules(id, bot_id, thread_id, cron, prompt, kind scheduled|proactive, enabled, last_run_at, next_run_at)`
- `memories(id, bot_id, content, source user|bot, created_at)`
- `skills(id, bot_id, name, description, body, updated_at)` — mirror of `<ws>/.claude/skills/*/SKILL.md` (P2)
- `artifacts(id, run_id, bot_id, r2_key, mime, bytes, signed_url, signed_until, created_at)` (P2)
- `devices(id, name, version, last_seen_at)` — daemon heartbeat every 60 s; "PC offline" when > 3 min old.

Triggers (plain PL/pgSQL):
- insert `messages` role=user → insert `runs(kind chat, status queued, prompt = content)` same tx; bump thread.
- `pg_notify('Familiar', {"t":table,"id","op","owner","run","bot"})` on runs, approvals, messages, schedules, events,
  threads, bots, memories. All values are JSON strings (or null).
- insert `schedules` without `thread_id` → create a thread; cron change / re-enable → `next_run_at = null`.
- `familiar_enqueue_due_schedules()` — called by the daemon each tick (no pg_cron): enabled schedules with
  `next_run_at <= now()` get a queued run and `next_run_at = null`; the daemon then computes the next fire time.

## API (familiar-server)
JSON over HTTPS; `Authorization: Bearer <token>` on everything except auth and health. Errors: `{"error": "..."}`.
CORS: origins from `FAMILIAR_WEB_ORIGINS` (comma list) plus `tauri://localhost` and `http://tauri.localhost`.
```
GET  /healthz                                → "ok"
GET  /api/auth/state                         → {setup_needed}
POST /api/auth/setup  {email,password}       → {token,user}   only while no user exists
POST /api/auth/login  {email,password}       → {token,user}   sessions last 30 days
POST /api/auth/logout                        → 204
GET  /api/me                                 → {id,email}
GET  /api/overview   → {bots:[bot+status+last_run_at], pending_approvals, devices:[device+online]}
GET|POST          /api/bots                  POST {name,slug?,persona?,model?}
GET|PATCH|DELETE  /api/bots/:id              PATCH {name?,persona?,model?,paused?}
GET|POST          /api/bots/:id/threads      POST {title?}
PATCH|DELETE      /api/threads/:id           PATCH {title}
GET|POST          /api/threads/:id/messages  GET ?before=&limit=; POST {content} (trigger queues the run)
GET               /api/threads/:id/runs      ?limit= newest first
GET               /api/bots/:id/runs         ?limit=&before=
GET               /api/runs/:id
GET               /api/runs/:id/events       ?after_seq= ascending
POST              /api/runs/:id/cancel       cancels if queued|running|waiting_approval
GET               /api/approvals             ?status=pending&bot_id= (with bot name)
POST              /api/approvals/:id         {decision: approve|deny, response?} only if pending
GET|POST          /api/bots/:id/schedules    POST {cron,prompt,kind,enabled?} cron validated server-side
PATCH|DELETE      /api/schedules/:id         PATCH {cron?,prompt?,kind?,enabled?}
GET|POST          /api/bots/:id/memories     POST {content}
DELETE            /api/memories/:id
GET|POST          /api/rules                 ?bot_id= (omit = global only, all=1 = everything); POST {bot_id?,pattern,decision,note?}
DELETE            /api/rules/:id
GET               /api/stream?token=         SSE: `event: notice` data {t,id,op,run,bot} for the owner; ping every 25 s
```
The UI refetches whatever a notice points at. SSE takes the token as a query param because EventSource can't set
headers.

## Run lifecycle (familiar-core)
1. Start: upsert `devices`; `LISTEN Familiar`; mark this owner's `running` runs `failed` ("daemon restarted").
   **On every (re)connect**: sweep queued runs and pending approvals (notifications during a gap are lost).
2. Scheduling: a run is eligible when it is the oldest queued run of its thread, its bot has no running run, the bot
   is not paused, and active runs < `max_parallel` (default 2). If the last `rate_limit_event` shows five_hour
   utilization ≥ 0.9, hold `scheduled|proactive` runs (chat still runs).
3. Workspace `~/.familiar/bots/<slug>/` (slug validated as a path segment). Render persona + house rules + memories to
   `~/.familiar/bots/.prompts/<slug>.md` — outside the workspace, so the bot cannot edit its own instructions.
4. Thread's first run: generate a UUID, write it to `threads.claude_session_id`, spawn with `--session-id <uuid>`.
   Later runs: `--resume <uuid>`. If resume fails with "No conversation found": clear it, start a new session
   seeded with the last 20 messages as context.
5. Spawn (cwd = workspace, stdin stays open for the whole run):
   ```
   claude -p --verbose --output-format stream-json --input-format stream-json
     --model <bot.model> (--session-id|--resume) <uuid>
     --restricted --strict-mcp-config --tools <FULL_TOOLS | research tools> --mcp-config <bots>/.prompts/<slug>.<run>.mcp.json (private file outside the workspace, deleted after the run)
     --system-prompt-snapshot off --append-system-prompt-file <prompt file>
     --permission-prompt-tool stdio
     --settings '{"permissions":{"allow":[..],"deny":[..],"ask":[..review rules]}}'
   ```
   Write the prompt as `{"type":"user","message":{"role":"user","content":"<prompt>"}}\n`.
   `--restricted` (verified): ignores settings files and skill `allowed-tools` (no self-granted permissions), confines
   file tools to the workspace, and skips CLAUDE.md — hence the appended prompt file; snapshot off so persona/memory
   edits apply on resume. Proactive runs get Read,Glob,Grep,WebSearch,WebFetch with WebFetch pre-allowed.
6. Parse stdout JSONL:
   - `assistant` → content blocks: `text` → event text, `thinking` → event thinking, `tool_use` → event tool_call.
   - `user` → `tool_result` blocks → event tool_result.
   - `control_request` subtype `can_use_tool` → permission flow (below) → reply on stdin.
   - `rate_limit_event` → event rate_limit + update throttle state.
   - `result` → cost/usage, final text → `messages(role assistant)`; `subtype != success` → run failed.
   After `result`, close stdin and wait for exit. stderr is captured into the error on failure.
7. Cancel: UI sets `runs.status='cancelled'` → NOTIFY → daemon writes
   `{"type":"control_request","request_id":"<uuid>","request":{"subtype":"interrupt"}}`; after 10 s,
   kill the process tree (`taskkill /T /F /PID` on Windows, process group on unix).

### Stdio permission protocol (verified on 2.1.282)
Request (stdout):
`{"type":"control_request","request_id":"R","request":{"subtype":"can_use_tool","tool_name":"Write","input":{...},"tool_use_id":"toolu_..","description":"..","permission_suggestions":[...]}}`
Reply (stdin):
`{"type":"control_response","response":{"subtype":"success","request_id":"R","response":{"behavior":"allow","updatedInput":{...}}}}`
or `..."response":{"behavior":"deny","message":"why"}}}`.

## Permissions
Claude Code itself enforces `allow`/`deny` rules (passed as flags) and auto-allows read-only tools. Only calls that
would prompt reach `can_use_tool`. The daemon then:
1. `review` rule matches (pattern = exact tool name or `Tool(glob)` on the Bash command / file path) → reviewer:
   `claude -p --model haiku --tools "" --strict-mcp-config --no-session-persistence --permission-prompts none
   --json-schema <{verdict: allow|escalate, reason}>` with the run goal, the action and the rules.
   `allow` → allow; `escalate` → human.
2. Otherwise → human: insert `approvals(pending)`, set run `waiting_approval`, desktop notification, wait for
   decision via NOTIFY (+ sweep on reconnect). Timeout 30 min → `expired` → deny.
Every decision is an `approval` event (who decided and why), so the activity view shows everything.
**Security stance (P0/P1):** the bot runs on your host. Default is `ask` for Bash, Edit, Write and browser actions.
The UI warns against `Bash(*)` allow rules. Proactive runs get `--tools` read-only. Docker "computer" mode (P2) is the
real isolation.

## Familiar MCP tools (P1; daemon serves streamable HTTP on 127.0.0.1:<random port>, per-run bearer token)
`remember`, `forget`, `notify_user`, `ask_user` (blocks like an approval; set per-server `timeout` in mcp.json —
HTTP MCP tools idle-time out at 5 min by default), `schedule_task`, `list_schedules`, `cancel_schedule`,
`handoff(bot_slug, task)`, `save_artifact(path)`. Browser: `@playwright/mcp` (pinned, pre-installed; npx cold start
counts against the 30 s MCP startup wait) with `--user-data-dir <ws>/.browser`.
Skills: the bot writes `.claude/skills/<name>/SKILL.md` itself; Claude Code loads them natively. "Learn from
demonstration" = walk it through a task in chat, ask it to save the procedure as a skill, then schedule it.

## Config (`~/.familiar/config.toml`, env `FAMILIAR_*` overrides)
`database_url` (any Postgres; on Supabase use the session pooler on port 5432, since LISTEN needs session mode), `owner_id`, `server_url` (keep-alive pings), `max_parallel`,
`claude_bin` (default `claude`), `codex_bin` (default `codex`; the npm `.cmd` shim is resolved on Windows), `bots_dir`
(default `~/.familiar/bots`), later `r2.*`. Secrets never reach the web app.

## Codex engine (`bots.engine = 'codex'`, `crates/familiar-core/src/codex.rs`)
Same workspace, prompt file, gate, MCP servers, browser and permission decision as Claude; only the protocol differs.
One `codex app-server` per run (JSON-RPC over stdio, protocol v2, verified on codex-cli 0.159.3 with a ChatGPT login;
0.46 only has the v1 `newConversation` API and the backend serves it no models, so it fails with an upgrade hint):
`initialize` → `config/read` (the owner's own Codex MCP servers are disabled for the thread) → `model/list` (a Claude
alias such as `sonnet` means Codex's default model) → `thread/start` (id → `threads.codex_thread_id`) or `thread/resume`
("no rollout found" → fresh thread seeded with history) → `turn/start`. Params: `developerInstructions` = prompt file,
`approvalPolicy untrusted`, sandbox `danger-full-access` (research: `read-only`; with a sandbox Codex runs sandboxed
writes without asking), config overrides: Codex apps/plugins/browser/computer use/sub-agents off, no AGENTS.md, MCP
servers with `default_tools_approval_mode = "prompt"` (Familiar's own: `approve`), `mcp_optional_startup_grace_ms`.
Events: `item/agentMessage/delta` + `item/reasoning/*Delta` → deltas; `item/started|completed` (agentMessage, reasoning,
commandExecution, fileChange, mcpToolCall, webSearch) → text/thinking/tool_call/tool_result; `turn/completed` → result.
Approvals (server requests) → `runner::decide_tool` after owner deny/allow presets: `item/commandExecution/requestApproval`
→ `Bash {command}` (shell wrapper stripped), `item/fileChange/requestApproval` → `Edit {file_path, file_paths, changes}`,
`mcpServer/elicitation/request` with `_meta.codex_approval_kind = mcp_tool_call` → `mcp__<server>__<tool>`; replies
`accept`/`decline` (no reason text reaches the model). Cancel: `turn/interrupt`, kill the tree after 10 s. Cost is not
reported (usage = summed per-request token counts). Codex's known-safe read-only commands run without asking.

## Crates
tokio, sqlx (postgres, runtime-tokio, tls-rustls, uuid, chrono, json; `PgListener`), serde/serde_json, uuid, chrono,
cron (next fire times), toml, directories, tracing. P1: rmcp 3.x (server + streamable HTTP, axum). Desktop: tauri 2,
tauri-plugin-notification.

## Phases
- **P0**: migrations; familiar-server API + SSE; familiar-core end-to-end chat turn (queue → spawn → events/messages) including stdio approvals and
  cancel; `familiard`; web UI: magic-link login, bots, threads/chat, activity feed, approvals inbox, rules, schedules.
- **P1**: Tauri desktop (tray, notifications); schedules firing; browser MCP; Familiar MCP tools; batched
  delta streaming; deploy web to Vercel; reviewer.
- **P2**: handoff, skills mirror, R2 artifacts/screenshots, Docker computer mode.

## P1/P2 contract additions
**Live token streaming (no DB writes).** The daemon runs claude with `--include-partial-messages`, batches
`text_delta`/`thinking_delta` chunks for ≥150 ms and sends `pg_notify('familiar_delta', {"owner","run","kind":"text"|"thinking","text"})`
(payload kept < 7 KB; longer batches are split). familiar-server `LISTEN familiar_delta` and forwards to the owner's SSE stream as
`event: delta` data `{run, kind, text}`. The UI appends deltas to a live bubble for that run and drops the bubble when the
persisted `text` event (or run end) arrives. Deltas are best-effort; persisted events stay the source of truth.

**Familiar MCP server (daemon, 127.0.0.1 random port, streamable HTTP, per-run bearer token).** Tools the bot can call:
`remember(content)` → memories(source bot) · `notify_user(message)` → assistant message in the run's thread + desktop/
channel notification · `ask_user(question)` → approvals row (tool_name `ask_user`, input `{question}`), blocks until the
owner answers (`response`) or 30 min · `schedule_task(cron, prompt, kind)` / `list_schedules()` / `cancel_schedule(id)` ·
`handoff(bot_slug, task)` → new thread on that bot + queued run (kind `handoff`, `parent_run_id`) · `list_bots()` ·
`save_artifact(path)` → uploads a workspace file. Proactive runs may only use remember/notify_user/list_*.

**Browser.** `@playwright/mcp` (pinned) per bot: `--user-data-dir <bots>/.browsers/<slug>` (logins persist),
`--output-dir <ws>/.shots`, `--isolated` off. Read-only browser tools (navigate, snapshot, screenshot, tabs, wait) are
pre-allowed; click/type/fill/select/upload/evaluate need approval (proactive: denied). New screenshots in `.shots` are
auto-saved as artifacts at the end of each tool result batch.

**Artifacts.** `artifacts(name, mime, bytes, storage db|s3, data bytea null, r2_key null)`. Daemon stores files ≤ 5 MB in
Postgres when no S3 store is configured, else uploads to S3/R2 (`[s3] endpoint, bucket, region, access_key_id,
secret_access_key` in config; same as `FAMILIAR_S3_*` env on the server). Event `artifact` payload `{artifact_id,name,mime,bytes}`.
API: `GET /api/runs/:id/artifacts`, `GET /api/bots/:id/artifacts`, `GET /api/artifacts/:id/download?token=` (bytes, or 302
to a 10-minute presigned URL for s3).

**Skills.** After each run the daemon mirrors `<ws>/.claude/skills/*/SKILL.md` (frontmatter name/description, body) into
`skills` (upsert by bot+name; rows whose folder vanished are deleted). API `GET /api/bots/:id/skills` (read-only list).

**Auto-review.** `review` rules send the call to a one-shot reviewer (`claude -p --model haiku --tools "" --json-schema`)
with the run goal, recent activity, the call and the owner's rules. Verdict `allow` → allowed (`decided_by reviewer`);
`escalate` → human approval with the reviewer's reason attached.

## Integrations (connectors, channels, triggers)
Secrets: `familiar-crypto::SecretBox` (AES-256-GCM, `FAMILIAR_SECRET_KEY` base64 32 bytes, set on both server and daemon; the
daemon reads `secret_key` from config or env). The server encrypts on write and **never returns secrets** (responses
carry `has_secrets: bool` and the env/header *names* only).

**Connectors = MCP servers** (GitHub, Notion, Slack, Linear, Google Workspace, Postgres, filesystem, anything MCP).
The daemon decrypts and adds every enabled connector linked to the bot into the run's `mcp.json`
(`stdio`: command/args/env; `http`: url/headers). Tools appear as `mcp__<name>__*`; they follow normal rules (ask by
default; owners add allow rules for read tools). Proactive runs only get connectors' tools via approval-free allow rules.
```
GET  /api/connectors/presets      → catalog [{id,name,description,transport,command,args,url,secret_fields:[{key,label,help}],docs_url}]
GET|POST   /api/connectors        POST {name,preset?,transport,command?,args?,url?,secrets?:{env?:{},headers?:{}},enabled?}
PATCH|DELETE /api/connectors/:id  PATCH same fields; `secrets` replaces all when present
GET|PUT    /api/bots/:id/connectors   PUT {connector_ids:[...]}
```
Preset catalog lives in the server (`presets.json`): github (`npx -y @modelcontextprotocol/server-github`,
GITHUB_PERSONAL_ACCESS_TOKEN), notion (`npx -y @notionhq/notion-mcp-server`, NOTION_TOKEN), slack, linear (http
`https://mcp.linear.app/mcp`, bearer), google-workspace (`uvx workspace-mcp`, GOOGLE_OAUTH_CLIENT_ID/SECRET), brave-search,
postgres, filesystem, fetch, memory — marked "verify" where the package name was not confirmed.

**Channels: Telegram.** Owner creates a bot with @BotFather, pastes the token in the UI (`POST /api/channels
{kind:'telegram', token, default_bot_id}` → server stores encrypted, generates `pair_code`). The daemon long-polls
`getUpdates` (no public URL needed). `/start <pair_code>` from the owner binds `chat_id` (any other chat is ignored).
Then: plain text → message to the default bot in its "Telegram" thread (`/bot <slug> <text>` targets another bot,
`/bots` lists); replies, `notify_user` messages, run failures and approvals are pushed to the chat; approvals carry
inline buttons Approve/Deny (callback → approvals update); `ask_user` questions are answered by replying to them.
```
GET|POST /api/channels   PATCH|DELETE /api/channels/:id  (returns {id,kind,bound:bool,pair_code?,default_bot_id,enabled})
```

**Triggers: webhooks.** `POST /api/bots/:id/triggers {name,prompt,kind}` → returns the URL once:
`<server>/hooks/<token>` (token 32 random bytes base64url; only sha256 stored). `POST /hooks/:token` (no auth, body ≤
64 KB, any content type) → queued run (kind from trigger) in the trigger's thread with prompt
`<prompt>\n\n--- webhook payload ---\n<body>`; 202 `{run_id}`; rate limit 30/min per trigger.
```
GET|POST /api/bots/:id/triggers   PATCH|DELETE /api/triggers/:id   POST /api/triggers/:id/rotate → new URL
```

**Script gates.** `schedules.gate_command`: the daemon runs it (bot workspace as cwd, 60 s timeout) when the schedule is
due; empty stdout → the run is skipped (`status succeeded`, `error 'gate: nothing to do'`); otherwise stdout (≤ 8 KB) is
appended to the prompt. UI: optional "Only run when this command prints something" field.

**Daemon status.** `devices.info = {utilization, resets_at, throttled, active_runs, claude_version}` refreshed each
heartbeat; `/api/overview` includes it so the UI can show "paused until HH:MM (subscription limit)".

**Always-human actions.** Enforced by the daemon (`permissions::always_human`, command-aware: wrappers like xargs/env,
flag permutations, compound commands): recursive deletes, installs, sudo, force pushes, git clean/reset --hard,
download-pipe-to-shell, registry/scheduled-task edits. Owner Bash `allow` rules are applied by the daemon *after* this
check (they are not forwarded to Claude Code), and `review` rules can never approve these. Browser navigation is
auto-allowed only to public http(s) hosts (no file://, loopback or private networks).

## Dots-style UX (round 3)
References: CopilotKit/OpenDots (MIT; mascot states, chat-first layout, approval cards, computer panel with take-over),
Anil-matcha/Open-Dots (MIT; approval/audit patterns), OpenClaw (MIT; "dreaming" memory consolidation, heartbeat),
Hermes desktop (MIT; presence, onboarding hints), Eigent (Apache-2.0; browser workspace with take-over). Own artwork
only — no OpenAI names, logos or assets.

**Desktop first run (Tauri only).** `invoke('app_status')` → `{boot:{phase:'starting'|'database'|'ready'|'error', message,
embedded, database_url}, daemon:{running, error, config_path}}`; `invoke('claude_status')` → `{installed, version,
logged_in, auth_method}`. The desktop app creates `~/.familiar/config.toml` itself, runs a built-in Postgres when no
`database_url` is set, serves the API on `127.0.0.1:47080`, and the daemon adopts the single account as owner once it
exists — so first run = (wait for boot) → (Claude check: "run `claude` in a terminal and sign in" if needed) →
(create account) → (create your first teammate: name, color, persona, model) → home.

**Mascots.** Each bot has a deterministic color from its id (4–6 palette entries) rendered as an original SVG blob with
eyes. States (from OpenDots' CSS): `working` = gentle bob (translateY −5px, 2s ease-in-out infinite), `needs-you` = tilt
6deg + amber ring, `done` = tilt −3deg, `paused` = desaturated, `idle` = still; `prefers-reduced-motion` disables motion.
State: running run → working; pending approval/ask_user for the bot → needs-you; paused → paused; last run succeeded
< 10 min ago → done; else idle.

**Rules = four autonomy tiers (Dots' Custom Rules).** UI labels map onto rule decisions: "Do it without asking" =
`allow`; "Let auto-review decide" = `review`; "Ask me first" = `ask`; "Hand it to me" = `deny` (the bot stops and tells
you). Always-human actions (installs, recursive deletes, force pushes, privilege escalation) are shown as locked rows.

**Memory review ("What I learned").** `memories.status active|proposed|rejected`. Only `active` memories go into the
bot's instructions. The daemon runs a nightly `dream` run per bot that had activity since `bots.last_dreamed_at` (03:00
local, research-only tools): it reviews recent threads and calls `remember`, which in a dream run stores `proposed`.
API: `GET /api/bots/:id/memories?status=` (default all), `PATCH /api/memories/:id {content?, status?}`,
`POST /api/bots/:id/dream` (queue a dream run now). UI: proposed memories on top with Accept / Edit / Reject.

**Computer panel (live browser).** The daemon runs each bot's Chrome itself (headless, persistent profile,
`--remote-debugging-port`), points Playwright MCP at it with `--cdp-endpoint`, and while a run is active captures a
JPEG every ~1 s via CDP (only when it changed) into `live_frames` (one row per bot). Notice `{t:'live_frames', bot}`.
API: `GET /api/bots/:id/live` → `{url,title,width,height,updated_at}` (404 if none), `GET /api/bots/:id/live.jpg?token=`
(image/jpeg, no-store), `POST /api/bots/:id/live/input {type:'click'|'type'|'key'|'scroll'|'navigate', x?,y?,text?,
key?,dy?,url?}` → `pg_notify('familiar_input', {owner,bot,...})` → daemon dispatches via CDP (take-over). Coordinates are in
frame pixels.

## CRM (`familiar_core::crm`, migrations `20261009000000_crm.sql`, `20261009000001_crm_webhooks.sql`)
A small CRM inside Familiar, where the GTM crew keeps its work. Customer data stays in Familiar's Postgres; other CRMs
get it through signed webhooks.

**Tables.** `crm_companies` (domain unique per owner among live rows, `fit_score` 0–100, `fit_reason`, `tags`,
`source_urls`, `custom`), `crm_contacts` (email unique per owner among live rows, `do_not_contact` + `dnc_reason` +
`dnc_at`), `crm_deals` (stage `new → researching → contacted → replied → meeting → proposal → won | lost`,
`value_cents`, `next_step`), `crm_activities` (the timeline: note, research, email/DM sent and received, post, call,
meeting, stage_change; optional `approval_id` = the draft it came from), `crm_changes` (every write: row before and
after, `actor_kind` user|bot, `bot_id`, `run_id`; the newest change of a record can be undone), `crm_webhooks`,
`crm_webhook_deliveries`. Companies, contacts and deals are soft-deleted. All writes go through `familiar_core::crm`
(the API as the owner, the tools as `Actor::Bot{bot, run}`), each in one transaction with its change record and its
webhook deliveries.

**Owner API.** CRUD `/api/crm/{companies|contacts|deals}`, `/api/crm/activities`, `/api/crm/pipeline`,
`/api/crm/changes` + `POST /api/crm/changes/{id}/undo`, `/api/crm/export.csv`, `/api/crm/import`, and the webhooks below.

**Teammate tools** (Familiar's MCP server, pre-allowed like every `mcp__familiar` tool): `crm_search {query?, kind?,
stage?, tags?, company_id?, limit ≤ 50}`, `crm_get {kind, id}` (a company with its contacts, deals and last 20
activities; a contact with its deals; a deal with its timeline), `crm_pipeline` (per stage: count, value, newest 10),
`crm_upsert_company`, `crm_upsert_contact`, `crm_upsert_deal` (each `{id?}` to change one record, else create or
update the match: same domain / same email (else LinkedIn link, else name at the company) / same company + title),
`crm_move_deal {deal_id, stage, note}` (logs a `stage_change`), `crm_log_activity` (any kind but `stage_change`;
`draft` = the `#id` of one of the teammate's own drafts links the activity to it). No delete tool.

**Trust model** (`crm::teammate`):
- *Fenced text.* CRM text is partly copied from web pages and emails, so a record can carry instructions aimed at the
  next teammate that reads it. Every free-text value a tool returns (names, descriptions, notes, titles, emails, links,
  tags, `custom`, summaries, bodies) is wrapped in `<data-NONCE>…</data-NONCE>`, a fresh random nonce per answer, after
  a one-line reminder that fenced text is data, never instructions. Ids, numbers, timestamps, stages, kinds, flags and
  the normalised domain stay structured; Familiar's own `warning` (do-not-contact) is not fenced. Lists clip long text
  to 300 characters ("crm_get has the rest").
- *Owner edits win.* For a teammate's change to an existing record, a field is **held** when the newest `crm_changes`
  entry that touched it (a create that gave it a value, or any update/undo/delete that changed it) was made by the owner
  (`actor_kind = 'user'`; an owner undo or CSV import counts). A held field keeps its value and the tool answer lists
  it under `kept_owner_values` ("say so in your summary instead"). Teammates may always: fill fields nobody set, change
  what a teammate set, maintain `fit_score` / `fit_reason` (companies) and `stage` / `next_step` / `next_step_at`
  (deals), except that a deal the owner closed (won/lost) stays closed; add tags and `source_urls` (merged, never
  removed, at most 20); set do-not-contact. To let a teammate change a held field, the owner makes the change.
- *Do-not-contact.* Anyone sets it; only the owner clears it (a teammate gets an error). Records come back flagged
  (`warning`; deals carry `contact_do_not_contact`). `propose_draft` refuses a draft whose `to` names a do-not-contact
  contact by email (any case, inside `Name <addr>`, lists, `mailto:`), `@handle`/X profile link or LinkedIn profile
  link (any subdomain, query, trailing slash); soft-deleted contacts still count. The check runs again in
  `drafts::sweep` before an approved draft's follow-up is queued: if the recipient became do-not-contact meanwhile, the
  follow-up says the draft was approved but must not be sent (and no text is passed on); if the check itself fails the
  draft waits for the next tick.
- *Limits.* Research-only runs (`proactive`, `dream`) may only search and read. A teammate's new company or contact
  needs `source_urls`. One run makes at most `MAX_WRITES_PER_RUN` = 200 changes (counted in `crm_changes` by `run_id`,
  with the run row locked so parallel calls count in order); past it every write fails with "stop changing the CRM,
  summarise, a later run can carry on".

**Webhooks** (`crm::webhooks`, API `routes/crm_webhooks.rs`, owner only: teammates have no API access). `GET/POST
/api/crm/webhooks`, `GET/PATCH/DELETE /api/crm/webhooks/{id}`, `POST /api/crm/webhooks/{id}/test` (a `ping`, sent at
once, never retried, answers with its delivery), `GET /api/crm/webhooks/{id}/deliveries?limit` (newest first, with
payloads). At most 20 per owner. The secret (`whsec_` + 64 hex) is made by the server, returned only in the create
answer, stored sealed with `SecretBox` (needs `FAMILIAR_SECRET_KEY`; 503 otherwise). Turning a webhook off fails its
waiting deliveries.
- Events: `company.created`, `company.updated`, `contact.created`, `contact.updated`, `contact.do_not_contact`,
  `deal.created`, `deal.updated`, `deal.stage_changed`, `activity.created` (soft deletes and undos are `*.updated`).
- Payload: `{id, event, at, data, previous?, actor: {kind: "user"|"bot", bot_id?, bot_slug?}}`; `id` is the delivery
  id, also sent as `Familiar-Delivery` (the same on every retry: use it to deduplicate); `data` / `previous` are the row
  after / before (no `owner_id`).
- Delivery: queued in the transaction of the change, sent by a daemon loop (woken by the delivery's notice, else every
  30 s; 20 per batch, 4 at a time, each claimed with a 10-minute lease first). POST with `Content-Type:
  application/json`, `Familiar-Event`, `Familiar-Delivery`, `Familiar-Signature`; 10 s timeout, redirects not followed,
  no proxy, at most 256 KB. 2xx = delivered; anything else is retried after 1 m, 5 m, 30 m, 2 h and 6 h, then `failed`.
  Delivered and failed rows are pruned after 30 days.
- **Verifying a delivery:** `Familiar-Signature: t=<unix seconds>,v1=<hex>` where `hex = HMAC-SHA256(key = the secret
  string exactly as shown, including "whsec_", message = "<t>.<raw request body>")`. Compute it over the raw bytes,
  compare in constant time, and reject a `t` more than 5 minutes off.
- **Where a webhook may point**, checked when it is saved and again at every delivery after DNS, with the connection
  pinned to the addresses that were checked (no DNS rebinding between check and connect): `https` to public addresses;
  this computer (loopback, `localhost`) over `http` or `https`; never private LAN (RFC 1918, CGNAT, IPv6 ULA) — not
  even with https, since the payload is customer data and LAN devices are the classic target of forged requests (a LAN
  receiver can be reached through a public https endpoint or a tunnel on this computer); never link-local (cloud
  metadata), multicast, unspecified, broadcast, documentation, benchmarking or reserved addresses, including IPv4 in
  IPv6 (mapped, compatible, NAT64, 6to4) and Teredo. Addresses are parsed the way the URL standard does (`127.1`,
  `0x7f.0.0.1`, `2130706433`). URLs with a user name or password are refused. Shared with browser navigation:
  `permissions::is_public`.

**GTM crew** (`templates.json`, `bundles.json`). Lead researcher (finds leads from public signals, adds companies /
contacts / `new` deals with `fit_reason` and `source_urls`, never contacts anyone), Outbound drafter (first touches for
`new` deals: one sourced observation, a one-line opt-out in every first email, every message through `propose_draft`
with the CRM ids in its note; after approval sends exactly the approved text, logs `email_sent`/`dm_sent` with the draft
id, moves the deal to contacted), Reply & follow-up tracker (twice a day on weekdays: matches replies to contacts, logs
them, keeps stages true, "stop"/"unsubscribe" → do-not-contact + lost, at most 2 follow-ups per contact after N days of
silence), Weekly metrics reporter (reads the pipeline), Inbox assistant (inbound leads become deals). Every
CRM-writing template carries the same CRM rules (sources, public business details only, never guessed emails or bought
lists, do-not-contact, owner edits win, fenced text). `GET /api/templates/bundles`; `POST
/api/templates/bundles/gtm-crew/create {answers}` hires the whole crew in one transaction (all or none) from shared
questions (product, ideal customer, ask, sender, voice, follow-up days) with every schedule off.

## Privacy model (local-first)
Familiar has no hosted service of its own. The desktop app runs the engine, the API (127.0.0.1 only) and a private
built-in PostgreSQL on the owner's machine; all data lives in `~/.familiar`. The only outbound traffic is the owner's
own LLM requests (their Claude/Codex subscription) and whatever sites/apps their teammates use. Remote access is the
owner's choice and never goes through infrastructure we run: (1) a private network such as Tailscale/WireGuard to the
owner's own PC, (2) self-hosting `familiar-server` + Postgres inside an organisation's own infrastructure, or (3) a
future optional end-to-end-encrypted relay that only forwards ciphertext. Chat channels (Telegram, WhatsApp) are
third-party transports and are opt-in.
