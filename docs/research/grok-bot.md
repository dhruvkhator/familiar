# Grok Bot teardown (research date 2026-10-02)

Legend: [V] verified from primary (docs.x.ai/grok-bot/*, cursor.com/help/grok-bot/plans, x.ai design post via fetched summary); [S] secondary (reviews, press, forum); [?] unverified/inferred.
Note: WebFetch summaries are model-condensed, so copy strings are paraphrase-grade, not guaranteed verbatim. x.ai/news blocked (403); docs pages /memory, /connectors, /security-and-privacy 404 (they do not exist as such; memory/connectors are covered in other pages). Could not fetch App Store listing directly; MacRumors/aiweekly report developer is Anysphere [S].

Base URLs: https://docs.x.ai/grok-bot (+ /get-started /bots /computer-and-apps /chat-and-collaboration /skills-routines-and-automations /security /teams-and-enterprises /mobile /use-cases /settings-and-notifications), https://cursor.com/help/grok-bot/plans

## Timeline [S: keywordseverywhere.com/news/grok-updates/grok-bot/]
08-11 beta (SuperGrok Heavy, Cursor Ultra, Teams Premium); 08-21 + SuperGrok Plus etc; 08-26 base SuperGrok ($30) and Cursor Pro; 08-29 X connector (free API credits for paying subs; search posts/timelines/mentions); 09-03 Enterprise opens (2 weeks free), waitlist ends; 09-03 "Designing Grok Bot" post. Product is built/hosted by Cursor (Anysphere); sign-in is Cursor auth; marketed under SpaceXAI/Grok.

## Core concepts [V: x.ai Designing Grok Bot, via secondary summaries vpsranking.com + search]
Five primitives: Bot (persistent agent: identity, memory, runtime, tools), Chat (interface to a Bot), Prompt (one-off, saved as Skill, or triggers Routine), Tools (APIs, connectors, shell, computer use), Artifacts (docs, designs, code, data). Tools and Skills are account-level; Memory and Routines belong to the Bot. Principle: "when you come back tomorrow, you are coming back to the same Bot." Roster-not-history sidebar. Design test: "Did this help someone delegate, or give them one more thing to manage?"

## 1. Bots
- Create [V /bots]: sidebar "New" or Cmd/Ctrl+N -> "New chat" -> "Create new Bot" (or type a name and choose `Create "name" Bot`) -> creates and opens bot named "New Bot" -> Bot menu / "Edit Profile": name, label(title), description, avatar -> start with a concrete task. Onboarding [V /get-started]: asks what tools you use, suggests starter teammates, or "Create your own" with short name + one primary job + operating guidelines.
- Identity [V design post]: name, avatar, title. Avatars = simple shapes + expressive eyes + accessories; avatar animates state: idle, thinking, working, waiting, blocked, done; hover shows preview of execution. Dynamic wallpapers shift through day.
- Roles: docs push narrow roles (Talent Scout, Expense Manager, Bug Reproduction; "General Helper" discouraged). Use-cases page [V]: Sales Outbound, Talent Scout, Paid Media, Expense Manager, Product Performance, Bug Reproduction, Account Health, Chief of Staff. Pattern: job description -> single safe task -> corrections -> save skill -> second test -> define failure cases -> approval-gated external actions.
- Count: ~50 Bots per account [V design post]. Groups 2-6 Bots.
- Per-bot settings [V settings page]: conversation details -> Agent settings: name, title, description, avatar, Notifications toggle. Boundaries are written as prose in the description ("Never send external messages without approval") [S eesel]. No model picker; fixed model set w/ auto failover; no hackable config [S Lenny/ChatPRD/Flavio]. Global default model + timezone in Settings -> General -> Agent [V].
- Ops [V]: pin; Hide (Hidden Bots at sidebar bottom); Duplicate ("<name> copy": profile, settings, skills, routines, avatar; NOT history, memory, attachments); Share as template (Public link or Team-only; recipient previews on x.ai and adds); Delete (removes profile, conversation, routines; does NOT remove shared-computer files/sessions [S]).

## 2. The computer
- [V /teams-and-enterprises] One dedicated cloud computer per USER, Firecracker microVM (own kernel/memory/devices); ALL of a user's Bots share it. Isolation is personality/workspace only. "Treat a login or file on the computer as available to every Bot." Docs say don't use separate Bots as security boundaries.
- [V /computer-and-apps] Shared: browser cookies/sessions, files, CLI credentials. Each Bot has its own screen -> parallel work; one Bot = one computer-use task at a time. Persistent `/workspace` for durable files; temp dirs/uncommitted state replaceable. Settings -> Updates: Update (new image, keeps files) / Reset (rebuild from last snapshot; recent changes may vanish).
- OS: Linux [S eesel]; Chrome, terminal, filesystem [S Lenny/ChatPRD].
- Watching: three levels [V]: Status (title-bar icon turns purple while computer active), Preview (pinned side panel: clicks, typing, navigation, status), Takeover (full-screen, take control, hand back). "Agent Computer" entry in conversation. Works with laptop off/app closed [V]. Mobile can monitor computer [V /mobile].
- Local computer is separate [V]: command execution on user's machine requires "Execution on Local Computer" = Ask every time / Always allow / Never allow (default per-command approval; docs recommend Never). Optional "Route traffic through your desktop" sends bot web traffic via your IP (helps with private nets and datacenter-IP blocking).
- Infra [V]: Cursor-hosted only, shared static egress IPs, daily encrypted backups, no on-prem. Some sites block datacenter IPs [S].
- Hosts: *.cursorvm.com needed through TLS proxies [V].

## 3. Signing in / connectors
- Browser logins [V]: when a login/2FA/CAPTCHA/payment verification appears, user opens Agent Computer, takes control, types credentials, hands back. Sessions persist so usually no re-login; short-timeout sites may need re-verification (pause bot, don't bypass). Secrets: masked "secure handoff"/secret request fields that keep values out of transcript [V use-cases/skills; S Flavio]. No password manager/passkey feature documented [?]. Per-agent Chrome profiles reset daily, crash [S cellcog].
- Marketplace [V]: sidebar Marketplace -> Add -> auth if prompted; `@` attaches connectors, `/` invokes skills. Connectors are account-wide, not per-bot. Connectors = Cursor plugins/MCP; team connector policy inherited ("Disabled by team admin").
- Priority order [S Flavio]: structured plugins > official APIs/CLIs > cloud browser > local computer. Known connectors [S]: Gmail, Google Docs/Sheets/Slides, Slack, X (posts/timeline/mentions), Stripe Link (single-use cards, approval shows merchant/price/breakdown), Finance (Plaid, read-only), GitHub, Zendesk/Shopify via browser. Connector tokens held on Cursor backend, not local [V].
- Multi-account connectors [S Lenny/ChatPRD, headline differentiator]: several accounts of same service (4 Gmail + 7 Slack workspaces demoed) attached to one bot. "Only agent platform that has shipped it."
- Identity [V]: bots act as the signed-in member; no independent identity.

## 4. Work model
- Chat-first, iMessage-like [S]; type, dictate (Cmd/Ctrl+D), voice chat, voice memo replies (play/pause/transcript). Send further messages while it works; user DMs take priority over background work; "Stop now" halts current work (completed actions not undone) [V].
- Timeline [V design]: prose + cards/widgets (draft email, task status, task board); routine creation, settings changes, bot-to-bot messages appear as events in transcript.
- Drafts [V]: external comms appear as editable draft with recipients -> Send email / Send message / Discard.
- Sidebar states [V]: "Needs attention" (question, approval, handoff) vs "Unread activity" (new result). Notifications per-bot toggle; suppressed when app focused; dock badge; mobile push "gradually rolling out". Errors above composer w/ copy request ID.
- Approvals [V /security]: Allow once / Always allow (saves matching rule) / Deny. Real boundary is the instruction itself. Auto-Review: independent model layer judging risky actions (shell, plugin calls, computer use, automation writes, delegation) -> approve / require approval / deny. Personal rules "Ask first" / "Allow automatically" in Settings -> General -> Auto-review, stored LOCALLY per desktop, not synced [V]; Ask first wins on conflict. Approval requests are separate from tool-execution binding [S].
- Audit: Enterprise-only Audit Logs, Action Recording (off by default, 90-day, scrubbed shell commands, tool calls, approval decisions, browser nav, file transfers), OTel export (cursor.surface=grok_bot), Conversation Content Export, Conversation Insights [V]. Non-enterprise: transcripts only [S].
- Routines [V /skills...]: owned by a Bot; schedule (time+tz) or event triggers (Slack, GitHub via Cursor integrations); max 50 routines/bot, last 20 run records each; manage via conversation details -> Routines: enable/pause, test run, edit, history, delete (no undo). Test runs do REAL work (no dry run). Create by asking the bot in chat. Mobile can pause/resume/view history; editing routines and Teach-a-task desktop-only [S].
- Delegation to Cursor Cloud Agents for coding (admin toggle) [V].

## 5. Collaboration
- Group chats [V]: 2-6 Bots, shared outcome; plain message -> auto-pick responder; `@Bot`, multiple mentions, `@everyone` (sparingly). Bot-to-bot handoffs async; bot-to-group messages text-only, images only via direct bot-to-bot. Each bot keeps own memory; share project context. Shared via direct messages, group chats, shared files in /workspace [V]. Complaints: multi-bot threads noisy/repeat themselves, burn quota [S].
- "Team" in org sense = Cursor Teams/Enterprise: Team Rules (scoped Cursor/Grok Bot/both, mandatory), Team Setup manifests (scripts on all computers) + Team Secrets (100 secrets, 32KB each, 96KB total), shared templates, Team Marketplace [V]. No Slack-style multi-human bot sharing documented [?]; each person has own computer/bots.

## 6. Learning
- "Teach a task" [V /skills]: in 1:1 bot chat with computer view active -> Teach a task -> describe goal -> do workflow once (up to 10 min recording of browser activity [S]) -> stop -> review generated skill DRAFT -> test on safe data -> schedule. Don't expose secrets; use secure handoff. Output needs decision rules, failure handling, approval gates added.
- Skill = reusable instructions; six elements: when to apply, inputs/access, steps, validation, output format, approval requirements. Account-level, shared by all bots, need connectors/logins. Invoked with `/`. Skill file format not published [?] (prose instruction doc; likely SKILL.md-like [?]).
- Recommended loop: manual task -> refine -> skill -> routine.

## 7. Memory
- [V /bots] retains stable working preferences, important facts, summaries of work; per Bot. Memory != database; shouldn't replace CRM/repo [S].
- Implementation [S Cursor forum 168066]: agent-side markdown on the bot's computer: `profile` + dated logs. No Settings UI to view/edit/delete; workaround: ask bot to dump memory or browse filesystem. Feature request open.
- Each Bot = one unbounded thread; full history reloaded each turn; no fresh session, no manual compaction, no context meter (staff-confirmed) [S cellcog]. Old preferences persist after policy change [S].

## 8. Surfaces
- Desktop: macOS (Apple silicon/Intel dmg), Windows (x64/Arm64), Linux (.deb/.rpm/AppImage) [V get-started; one secondary claimed no Linux, conflicts]. iOS 18+, Android 9+ [V /mobile; Android was "to follow" at launch]. No web client documented [?]. Mobile: messaging, dictation, voice chat, attachments, @mentions, threads, reactions, drafts per convo, monitor computer, routine pause/resume. Keyboard: Cmd/Ctrl+K jump, +Shift+F search bots, +N new, +D dictate, Ctrl+Tab cycle bots, Cmd/Ctrl+Shift+, toggle settings. Slack/Teams as a chat surface into bots: NOT documented (Slack is a connector/trigger) [?]. Auto-update; Settings -> Updates.

## 9. Pricing / limits / safety
- [V cursor.com/help/grok-bot/plans] No separate subscription. Included with Cursor Pro / Pro+ / Ultra (increasing weekly usage), Teams (per Teams allowance), Enterprise (admin-managed); or linked SuperGrok Heavy > Plus > base SuperGrok > X Premium+. SuperGrok Lite, SuperGrok Team/Enterprise ineligible. Weekly included grant resets weekly; on-demand overage billed via Cursor and counts toward on-demand monthly limit. Cursor plan and SuperGrok link don't stack (higher wins). Trial = usage credit + 7-day window. No published numeric allowances; no separate Grok Bot spend cap [V]. Prices: Ultra $200/mo, Teams Premium $120/seat [S digitalapplied], SuperGrok $30 [S].
- Burn [S]: six-agent business used 42% weekly on day one; ~100 completions + one 10-min script = ~5%.
- Requires non-legacy Privacy Mode (cloud storage) [V]. ISO 27001 + 42001 (Schellman) incl. Grok Bot [V]; SOC2 etc. claims absent per one review [S]. Deletion per DPA within 30 days.
- Enterprise controls [V]: SSO SAML (Okta/Entra/Google/OneLogin), SCIM, Manage Group Access, Network Controls (4 modes; allow-all default; changes apply ~60s), local egress toggle, enforce Auto-review, team auto-review rules (locked in member UI), group settings widen-only, admin terminate/recreate member computers, Admin API, public template sharing restriction.

## 10. Distinctive / complaints
Distinctive vs OpenAI Dots-like agents: multi-account connectors; takeover-based login (user types into the bot's browser); roster-of-named-bots UX with animated avatars; persistent per-user VM with shared sessions; Teach-a-task recording to skills; avatar state; group chats of up to 6 bots; Auto-review model layer; Cursor Cloud Agent delegation.
Complaints [S: cellcog.ai/blog/grok-bot-problems, eesel, Lenny/ChatPRD, Flavio]: shared-login blast radius (no per-bot isolation); stuck computer 08-20/21 stopped all bots; "Bot failed to respond" persistent; quota burn and opaque limits; metering inconsistencies; unbounded context/no compaction; no model choice/low hackability; approvals preventive only (no undo; Stop doesn't undo); no dry run for routines; no per-action audit for non-enterprise; no memory UI; personal auto-review rules not synced; datacenter IPs blocked on some sites; multi-bot chatter; Claire Vo still uses OpenClaws; confusing plan eligibility; docs lag.
