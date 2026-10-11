# Plan: "Connect Google" (and friends) with one click, tokens on this PC

Users click **Connect Google**, approve on Google's own page, and teammates can read and send Gmail and use Calendar,
like Dots, Grok Bot or Manus. The difference: tokens never leave the owner's PC (encrypted in Familiar's database), and
no third-party token broker is involved. Owner: Opus 5.5. Review: Fable 5.1.

## How it works
- **One Familiar app registered with Google** (by the project owner, once): OAuth client of type *Desktop app*. Its
  client ID (and the non-confidential desktop "secret") ship in release builds through CI variables
  (`FAMILIAR_GOOGLE_CLIENT_ID` / `_SECRET`), overridable in `config.toml` for self-builders.
- **The sign-in:** installed-app flow with PKCE (S256) and a loopback redirect (`http://127.0.0.1:<random port>/`),
  opened in the owner's normal browser (never a teammate's browser, never an embedded view). `state` checked; the
  listener accepts one request and closes; timeout.
- **Scopes, least first:** Gmail read + send + compose (drafts) and Calendar events; requested per feature with
  incremental consent where possible. The consent screen shows exactly these.
- **Tokens:** refresh token stored with `SecretBox` (AES-256-GCM) in Familiar's database, linked to the owner; access
  tokens kept in memory and refreshed; revoke on Disconnect (Google's revoke endpoint) and delete.
- **Tools for teammates:** Familiar's own Gmail/Calendar tools (in the Familiar MCP server, so no third-party MCP code
  touches the token): search and read mail (bodies as untrusted, fenced data), list threads, create drafts, send
  (always through approvals: a send is a "send" action; the one-shot pre-approval for an approved draft applies with
  exact recipients/subject/body), list/create calendar events (create asks). Rate limits and size caps.
- **Which teammates may use it:** linked per teammate like connectors (the GTM crew gets it when the owner says so).
- **UI (desktop app):** Integrations → Accounts: "Connect Google" → browser opens → back in Familiar: "Connected as
  sam@acme.com", scopes listed, Disconnect. Teammate Settings: tick which teammates may use it.
- **Until Google verifies the app** (privacy policy page, security assessment for Gmail scopes) users see "Google
  hasn't verified this app" → Advanced → Continue; up to 100 users. Documented in the README.
- **Later, same pattern:** Slack, Notion, GitHub, HubSpot (their own OAuth apps; Notion and others also offer hosted
  MCP servers with OAuth).

## The owner's one-time Google setup (documented in docs/google-oauth-setup.md)
Google Cloud console → new project "Familiar" → enable Gmail API and Calendar API → OAuth consent screen (External,
app name Familiar, support email, scopes) → add test users (or publish as unverified) → Credentials → OAuth client ID →
Desktop app → copy client ID and secret into the repo's Actions variables (not secrets: desktop secrets aren't
confidential) and into config.toml for local builds.
