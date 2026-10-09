# Plan: everything in the desktop app

The desktop app is the product. Nothing may need the web app. These features exist only in `apps/web` today; build
them natively, in this order, reusing the existing client methods (`crates/familiar-client`) and the native patterns
(`crm_webhooks.rs` for a settings list with secrets shown once, `schedules.rs` for a full page, `bot_settings.rs` for a
teammate's Settings tab). Owner: Opus 5.5. Review: Fable 5.1.

| # | Feature | Web source | Client methods |
| --- | --- | --- | --- |
| 1 | **Integrations page** (sidebar or Settings): connector catalog (presets), install with secret fields, custom MCP server (stdio command + args + env, or http URL + headers), enable/disable, edit, delete; secrets write-only (never shown again, never logged) | `pages/Integrations.tsx` | `connector_presets`, `connectors`, `create_connector`, `update_connector`, `delete_connector` |
| 2 | **Telegram** on the same page: bot token (write-only), pairing code + `/start <code>` instructions, status, unlink | `pages/Integrations.tsx` (Telegram part) | `channels`, `create_channel`, `update_channel`, `delete_channel` |
| 3 | **A teammate's connectors** in its Settings tab: which connectors it may use (checkboxes), with a link to Integrations to add one | `pages/BotConnectors.tsx` | `bot_connectors`, `set_bot_connectors` |
| 4 | **Triggers** in a teammate's Settings: webhook URLs that wake it (create with a prompt template, copy URL, rotate token, enable/disable, delete), with a plain explanation of what a POST does | `pages/Triggers.tsx` | `triggers`, `create_trigger`, `update_trigger`, `rotate_trigger`, `delete_trigger` |
| 5 | **Skills** tab or section on the teammate page: list of skills it wrote (name, description, when used), read-only view of the SKILL.md | `pages/Skills.tsx` | `skills` |
| 6 | **Files** on the teammate page: artifacts it produced (per run/thread), open or save (system save dialog) | `pages/Files.tsx` | `bot_artifacts`, `run_artifacts`, `download_artifact` |
| 7 | **Rules page**: every allow / review / ask / deny rule, global and per teammate, add/edit/delete with plain-language explanations; the per-teammate "Allowed without asking" list stays | `pages/Rules.tsx` | `rules`, `create_rule`, `delete_rule` |
| 8 | Remove every "use the web app" pointer from the native app (CRM crew checklist → native Integrations; anything else found) | — | — |

Untrusted text (skill bodies, file names, connector names from presets/teammates) renders as plain text, http(s)-only
links, hidden characters handled like `approval.rs`. Secrets: write-only fields, `Zeroizing`, cleared when the page is
hidden. Live updates through the stream where the table has a notify trigger; no blanket redraws (keep the speed
round's targeted `Updated(Part)` model). Bench (`--bench-shot`) gets fake data for every new screen.
