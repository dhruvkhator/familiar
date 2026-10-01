# Familiar

> Formerly named *zed*. On first launch `~/.zed` is renamed to `~/.familiar`; old `ZED_*` env vars still work when the `FAMILIAR_*` one is unset.

An open-source, self-hosted, always-on AI teammate in the spirit of OpenAI Dots, xAI Grok Bot, Hermes Agent and OpenClaw.

Each **bot** has its own computer (a workspace folder with a terminal and a real browser), remembers what you teach it,
works on schedules and webhooks without being asked, hands work to other bots, and stops for your approval before doing
anything risky. Talk to it from the desktop app, the web app on your phone, or Telegram.

- **Brain:** your own subscription through the CLI already logged in on your machine: Claude Code (Claude) or Codex
  (ChatGPT), chosen per bot. No API keys. Codex needs a recent CLI: `npm i -g @openai/codex@latest`.
- **Body:** a Rust daemon (`familiar-core`) inside a Tauri tray app, or headless as `familiard`.
- **Backend:** a Rust API (`familiar-server`) on any host, with **any PostgreSQL** for storage (Supabase, Neon, RDS, local).
  No vendor SDKs; the database is just Postgres.
- **UI:** a static React app (Vercel or anywhere), also bundled in the desktop app.

## What a bot can do
| | |
|---|---|
| Chat | Threads with live token streaming, full activity log of every tool call |
| Computer | Shell + files in its own workspace, a persistent Chromium profile (logins survive) via Playwright MCP |
| Memory | Owner-taught and bot-saved memories (`remember`), injected into every run |
| Skills | Writes `.claude/skills/*/SKILL.md` procedures it can reuse; mirrored to the UI |
| Schedules | Cron (local time), optional **script gate** that only wakes the bot when a command prints something |
| Proactive | Research-only runs that can look but never act |
| Webhooks | `POST /hooks/<token>` wakes a bot with the payload (GitHub, Zapier, anything) |
| Teamwork | `handoff` a task to another bot, optionally waiting for its result |
| Reach you | `notify_user`, `ask_user` (blocks for your answer), desktop notifications, Telegram |
| Integrations | Any MCP server as a connector (GitHub, Notion, Slack, Linear, Google Workspace, …), secrets encrypted |
| Files | Screenshots and `save_artifact` files, stored in Postgres or any S3/R2 bucket |

## Safety model
- Bots run Claude Code in `--restricted` mode: settings files and skill `allowed-tools` are ignored (a bot can't grant
  itself permissions), file tools are confined to its workspace, and its instructions live outside it.
- Anything that writes, executes, clicks or uses a connector asks you first (web, desktop or Telegram), unless you add an
  `allow` rule. `review` rules let a Haiku reviewer approve low-risk calls and escalate the rest.
- Some actions always need a human regardless of rules: recursive deletes, installs, `sudo`, force pushes.
- Proactive runs are read-only. Approvals expire after 30 minutes.
- Bots still run as your user on your machine. Be careful with broad `allow` rules like `Bash(*)`.

## Desktop app (the easy way)
Build it with `pnpm install && pnpm --filter web build && cargo build --release -p familiar-desktop`, then run
`target/release/familiar-desktop`. On first launch it creates `~/.familiar/config.toml`, installs and runs its own private Postgres
(no Docker), serves the API on 127.0.0.1:47080, checks that Claude Code and/or Codex are signed in, and walks you through
creating your account and your first teammate (name, avatar, what it should do). It lives in the tray; closing the
window keeps your teammates working. Each bot gets a headless Chrome you can watch and take over from its Computer panel.

## Run it with scripts (Windows, Docker Postgres)
`.\scripts\start-local.ps1` starts Postgres (Docker, port 47432) and the desktop app, which runs the API
(127.0.0.1:47080) and the daemon in one process. `-Web` adds the browser UI on http://localhost:47173; `-Headless`
runs `familiar-server` + `familiard` without a window. `.\scripts\stop-local.ps1` stops it (`-All` also stops Postgres).
Settings: `~/.familiar/local.env` (database) and `~/.familiar/config.toml` (app, `local_api_port = 47080`). Logs: `~/.familiar/logs`.

## Quick start (manual, any OS)
1. Install and log in to [Claude Code](https://docs.claude.com/en/docs/claude-code), plus Node 20+ (for MCP servers).
2. Start Postgres (any provider). Generate a shared secret: `openssl rand -base64 32`.
3. Run the API: `DATABASE_URL=… FAMILIAR_SECRET_KEY=… cargo run -p familiar-server` (migrations run automatically).
4. Run the UI: `echo VITE_API_URL=http://localhost:8080 > apps/web/.env.local && pnpm install && pnpm dev`,
   open it and create the owner account. Copy your user id from `/api/me`.
5. Configure the daemon in `~/.familiar/config.toml` and run `cargo run -p familiard` (or the desktop app in `apps/desktop`):
   ```toml
   database_url = "postgres://…"        # on Supabase: the session pooler, port 5432
   owner_id     = "<your user id>"
   server_url   = "http://localhost:8080"
   secret_key   = "<same as FAMILIAR_SECRET_KEY>"
   # [s3]  endpoint = "https://<account>.r2.cloudflarestorage.com"  bucket = "Familiar"  access_key_id = "…"  secret_access_key = "…"
   ```

## Deploy (free tiers)
- **Postgres:** Supabase or Neon free project. Use a session-mode connection (LISTEN/NOTIFY).
- **API:** Render free web service from `render.yaml` (Docker). Set `DATABASE_URL`, `FAMILIAR_SECRET_KEY`,
  `FAMILIAR_WEB_ORIGINS` (your Vercel URL), `FAMILIAR_PUBLIC_URL` (for webhook URLs). The daemon pings it so it stays awake while
  your PC is on.
- **Web:** Vercel, root `apps/web`, env `VITE_API_URL=<Render URL>`.
- **Files (optional):** Cloudflare R2 bucket; set `FAMILIAR_S3_*` on the server and `[s3]` in the daemon config.

## Telegram
Create a bot with @BotFather, paste its token in **Integrations → Telegram**, then send `/start <code>` to it.
Plain messages go to your default bot (`/bot <slug> <message>` picks another, `/bots` lists them). Approvals arrive with
Approve/Deny buttons, `ask_user` questions are answered by replying, and failures of unattended runs are reported.

## Layout
`crates/familiar-core` daemon · `crates/familiard` headless binary · `crates/familiar-server` API · `crates/familiar-crypto` secrets ·
`apps/desktop` Tauri app · `apps/web` UI · `migrations/` SQL. Design: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

Early and experimental. Built for fun.
