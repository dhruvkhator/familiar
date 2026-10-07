# Familiar

An open-source, always-on AI teammate that lives on **your** computer — in the spirit of OpenAI Dots, xAI Grok Bot,
Hermes Agent and OpenClaw.

Each **teammate** has its own computer (a workspace folder with a terminal and a real browser you can watch and take
over), remembers what you teach it, works on schedules and webhooks without being asked, hands work to other
teammates, and stops for your approval before doing anything risky.

- **Brain:** your own subscription through the CLI already signed in on your machine — Claude Code (Claude) or Codex
  (ChatGPT), chosen per teammate, any model your plan includes. No API keys, no per-message billing.
- **Desktop app:** native, GPU-rendered (Rust + [GPUI](https://github.com/zeronsh/zui)), ~45 MB, in your tray.
- **Phone / browser:** a web app on the same engine, and chat channels such as Telegram.

> Formerly named *zed*. On first launch `~/.zed` is renamed to `~/.familiar`; old `ZED_*` env vars still work.

## Privacy: local-first, no cloud of ours
- **Everything stays on your machine.** Teammates, chats, memories, files and connector secrets live in `~/.familiar`
  in a private, built-in PostgreSQL. There is no Familiar account, no Familiar server, no telemetry.
- **What leaves your machine:** your own requests to Anthropic or OpenAI under your own subscription, and whatever
  websites and apps your teammates use for you. Nothing passes through us.
- **Secrets** (connector tokens, Telegram tokens) are encrypted at rest (AES-256-GCM) with a key that never leaves the
  machine. The local API listens on `127.0.0.1` only.
- **Reaching it from your phone** without anyone else's cloud:
  1. **Private network (recommended):** put your PC and phone on [Tailscale](https://tailscale.com) or WireGuard and
     open the web app over that encrypted link — nothing is exposed to the internet.
  2. **Self-hosted (teams/companies):** run `familiar-server` (one Docker image) and any Postgres inside your own
     infrastructure; data never leaves it.
  3. *Planned:* an optional end-to-end-encrypted relay that only forwards bytes it cannot read.
- **Desktop control** (optional, Windows, off for every teammate until you turn it on) uses
  [Windows-MCP](https://github.com/CursorTouch/Windows-MCP) (MIT). It isn't bundled: the first time a teammate gets it,
  Familiar installs the pinned version (0.8.7, from PyPI) once with your `uv` into `~/.familiar/tools/windows-mcp`
  (its own Python, cache and config there; never `uvx` at run time). Windows-MCP sends anonymous usage telemetry to
  PostHog by default; Familiar always runs it with telemetry off (`ANONYMIZED_TELEMETRY=false`, no PostHog key).
- **Chat channels** (Telegram, later WhatsApp) route messages through those companies' servers — fine for personal
  use; keep companies on options 1–2.

## What a teammate can do
| | |
|---|---|
| Chat | Threads with live streaming; a live card while it works, a clean chat when it's done |
| Computer | Shell + files in its own workspace and a persistent headless Chrome (logins survive) you can watch and take over |
| Memory | Remembers what you teach it; a nightly "dream" proposes new memories you accept or reject |
| Skills | Writes reusable procedures (`.claude/skills/*/SKILL.md`) |
| Schedules | Cron in local time, with an optional script gate that only wakes it when there's something to do |
| Proactive | Research-only runs that can look but never act |
| Webhooks | `POST /hooks/<token>` wakes a teammate with the payload |
| Teamwork | Hands tasks to other teammates and can wait for the result |
| Reach you | Asks questions, sends notifications (desktop, Telegram) |
| Integrations | Any MCP server as a connector (GitHub, Notion, Slack, Linear, Google Workspace, …) |

## Safety model
- Teammates run Claude Code in `--restricted` mode: they can't grant themselves permissions, file tools are confined to
  their workspace, and their instructions live outside it.
- Anything that writes, executes, clicks or uses a connector asks you first, unless you allow it. Rules come in four
  tiers — *do it without asking*, *let auto-review decide*, *ask me first*, *hand it to me*.
- Some actions always need you, whatever the rules: recursive deletes, installs, `sudo`, force pushes, running inline
  code, touching the browser's debugging port. Browser navigation is auto-allowed only to public websites.
- Proactive and dream runs are read-only. Approvals expire after 30 minutes.
- Drafts (posts, replies, emails, DMs) wait in your queue for up to 7 days without holding their teammate up; it hears
  your decision (approved with the exact final text, changes asked, or rejected) in a message Familiar writes, and the
  actual post or send still asks you.
- You can share folders on this PC with a teammate, read only (default) or read & write. Drives, your home folder,
  hidden settings folders (`.ssh`, `.aws`, `.config`…), app data, system folders, Familiar's own data and other
  teammates' workspaces are refused; links and junctions are resolved first and checked again before every run. Read
  only is enforced by Familiar (changes there are refused); on Codex, reading can't be limited to the shared folders.
- Desktop control: every step asks you, screenshots and screen reads included (they show your screen), in plain words
  with a small picture of the spot it would touch; no rule, auto-review or "Always allow" covers it. Windows-MCP's
  tools that run commands or touch files, processes, the registry, the clipboard, web pages or notifications are
  switched off, and starting programs by path is refused. One teammate at a time; while one uses the desktop the tray
  icon turns red and the tray's **Stop desktop control** ends it at once. It only works inside the Familiar app.

## Install
Download `Familiar-<version>-setup.exe` from [Releases](https://github.com/dhruvkhator/familiar/releases) (Windows;
macOS/Linux later), install (no admin rights needed), and open Familiar. The installer isn't code-signed yet, so Windows
SmartScreen may warn: choose *More info → Run anyway*, and compare the file with `SHA256SUMS.txt` if you like. First launch sets up the built-in database, checks your Claude Code / Codex
sign-in, and walks you through creating your first teammate. Requires [Claude Code](https://docs.claude.com/en/docs/claude-code)
and/or the [Codex CLI](https://www.npmjs.com/package/@openai/codex) signed in, plus Node 20+ for browser/connector tools.

## Build from source
- Native desktop app: `cd apps/native && cargo build --release` → `apps/native/target/release/familiar-native.exe`
  (Rust 1.99, pinned in `apps/native/rust-toolchain.toml`; the first build compiles GPUI and takes a while).
- Engine, API and tests: `cargo test --workspace --exclude familiar-desktop` (integration tests need
  `TEST_DATABASE_URL` pointing at any Postgres).
- Web app: `pnpm install && pnpm --filter web dev` (set `VITE_API_URL`).
- Headless server for self-hosting: `crates/familiar-server/Dockerfile` (`DATABASE_URL`, `FAMILIAR_SECRET_KEY`,
  `FAMILIAR_WEB_ORIGINS`), and `familiard` for a headless engine.

## Telegram
Create a bot with @BotFather, paste its token in **Integrations → Telegram**, then send `/start <code>` to it from a
private chat. Plain messages go to your default teammate; approvals arrive with Approve/Deny buttons.

## Layout
`apps/native` desktop app (GPUI) · `apps/web` web app · `crates/familiar-core` engine · `crates/familiar-server` API ·
`crates/familiar-host` desktop boot (built-in DB + API + engine) · `crates/familiar-client` typed API client ·
`crates/familiar-testkit` fake CLIs for tests · `migrations/` SQL · `apps/desktop` legacy Tauri app (being retired).
Design notes: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/native-app-plan.md](docs/native-app-plan.md).

Early and experimental. MIT licensed. Built for fun.
