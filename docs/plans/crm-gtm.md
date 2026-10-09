# Plan: built-in CRM + the GTM crew

Goal: an auto-GTM crew of teammates (find leads → draft outreach → you approve → send → track replies → report) that
keeps its work in a CRM inside Familiar. Customer data stays in Familiar's local Postgres; outside CRMs (HubSpot via
Zapier, Attio, a home-made one) receive it through signed webhooks.

Branch `crm` in the worktree `C:\personal\familiar-crm`. Phases run in order; each ends with green tests and a commit.

| Phase | Who | What |
| --- | --- | --- |
| 1 | Sonnet 5.5 | Migration, `familiar_core::crm` data layer, CRUD/pipeline/changes/undo/CSV API, client, tests |
| 2 | Opus 5.5 | Teammate CRM tools + trust model, webhooks (API, signing, delivery, address safety), GTM templates + crew bundle |
| 3 | Fable 5.1 | Review of the whole branch; Opus fixes |
| 4 | Opus 5.5 | Native app: CRM page, pipeline board, record panel, hire-the-crew flow, webhook settings |
| 5 | Fable 5.1 | Review of phase 4; fixes; merge into `familiar-v0` |

## Phase 1 — data layer and API (Sonnet: routine, fully specified)

### Migration `migrations/20261009000000_crm.sql`
Follow `20261007000001_bot_folders.sql` for style (owner_id references users, `familiar_notify` triggers, checks).

- `crm_companies`: `id uuid pk default gen_random_uuid()`, `owner_id` (users, cascade), `name text not null`
  (1–200), `domain text` (normalised, ≤ 253), `website text` (≤ 500), `industry`, `size`, `location` (each ≤ 200),
  `description` (≤ 4000), `fit_score int` (0–100, null ok), `fit_reason` (≤ 2000), `tags text[] not null default '{}'`,
  `source_urls text[] not null default '{}'`, `custom jsonb not null default '{}'`, `created_by_bot uuid` (bots, set
  null), `created_at`/`updated_at timestamptz not null default now()`, `deleted_at timestamptz`.
  Unique index `(owner_id, domain) where domain is not null and deleted_at is null`.
- `crm_contacts`: same base columns; `company_id uuid` (crm_companies, set null), `name` (1–200), `title` (≤ 200),
  `email` (lowercased, ≤ 320), `linkedin_url`, `x_handle` (≤ 100), `notes` (≤ 4000), `tags`, `source_urls`, `custom`,
  `do_not_contact bool not null default false`, `dnc_reason text`, `dnc_at timestamptz`.
  Unique index `(owner_id, email) where email is not null and deleted_at is null`.
- `crm_deals`: `company_id uuid not null` (crm_companies, cascade), `contact_id uuid` (crm_contacts, set null),
  `title` (1–200), `stage text not null default 'new'` check in
  (`new`,`researching`,`contacted`,`replied`,`meeting`,`proposal`,`won`,`lost`), `stage_changed_at`, `value_cents bigint`
  (≥ 0), `currency char(3) not null default 'USD'`, `next_step` (≤ 500), `next_step_at timestamptz`, `created_by_bot`,
  timestamps, `deleted_at`.
- `crm_activities`: `company_id`, `contact_id`, `deal_id` (each nullable, set null; check at least one is set),
  `kind` check in (`note`,`research`,`email_sent`,`email_received`,`dm_sent`,`dm_received`,`post`,`call`,`meeting`,
  `stage_change`), `summary text not null` (1–500), `body` (≤ 20000), `url` (≤ 1000), `approval_id` (approvals, set null),
  `bot_id` (bots, set null), `actor_kind text not null` check in (`bot`,`user`), `occurred_at`, `created_at`.
- `crm_changes`: `entity` check in (`company`,`contact`,`deal`,`activity`), `entity_id uuid not null`, `op` check in
  (`create`,`update`,`delete`,`undo`), `before jsonb`, `after jsonb`, `actor_kind`, `bot_id`, `run_id` (runs, set null),
  `at timestamptz not null default now()`, `undone_at timestamptz`. Index `(owner_id, entity, entity_id, at desc)`.
- `crm_webhooks`: `url text not null` (≤ 1000), `secret_enc text not null`, `events text[] not null`,
  `enabled bool not null default true`, `created_at`. (Phase 2 fills in the behaviour; phase 1 only creates the table.)
- `crm_webhook_deliveries`: `webhook_id` (cascade), `event text`, `payload jsonb`, `status` check in
  (`pending`,`delivered`,`failed`), `attempts int not null default 0`, `next_attempt_at timestamptz not null default now()`,
  `last_error text`, `created_at`, `delivered_at`. Index `(status, next_attempt_at)`.
- `familiar_notify` triggers on companies, contacts, deals, activities.

### `crates/familiar-core/src/crm.rs` (the one place CRM writes happen; server routes and phase-2 tools both call it)
- `pub enum Actor { User, Bot { bot: Uuid, run: Option<Uuid> } }`.
- Normalisers (pure, unit-tested): `domain("https://www.Acme.com/pricing") == Some("acme.com")`, rejects non-host
  input; `email(" Sam@Acme.COM ") == Some("sam@acme.com")`, rejects anything without exactly one `@` and a dot in the
  domain; `http_url` accepts only `http`/`https`; tags ≤ 20 × ≤ 40 chars, trimmed, deduped; `source_urls` ≤ 20, each
  `http_url`.
- Inputs: `CompanyInput`, `ContactInput`, `DealInput`, `ActivityInput` (all fields `Option` for patch semantics).
- `upsert_company(db, actor, input) -> (Value, bool)`: match by normalised domain, else by exact lower(name) when no
  domain; create or update; returns the row (as JSON without owner_id) and whether it was created.
  `upsert_contact`: match by email, else linkedin_url, else (lower(name), company_id). `upsert_deal`, `patch_*`,
  `soft_delete_*`, `log_activity`, `move_deal(deal, stage, note)` (sets `stage_changed_at`, logs a `stage_change`
  activity). Every write, in one transaction: the row change + a `crm_changes` record (`before`/`after` = the row
  JSON) + a call to `enqueue_webhooks(tx, owner, event, payload)` which in phase 1 is a no-op stub with a doc comment
  (phase 2 implements it).
- `do_not_contact`: settable to true by anyone; setting it back to false only by `Actor::User`
  (a bot gets an error). Sets `dnc_at`.
- `undo(db, change_id)`: only the newest change of that entity can be undone (else `Conflict`); create → soft delete,
  update → restore `before`, delete → clear `deleted_at`; records an `undo` change; marks `undone_at`.
- Queries: `search(owner, kind, q, filters, sort, limit ≤ 200, offset)`; `pipeline(owner)` = per stage
  `{stage, count, value_cents}` plus the deals (id, title, company name, contact name, next_step, next_step_at);
  `changes(owner, filters)`.

### API (`crates/familiar-server/src/routes/crm.rs`, registered in `lib.rs`; copy `schedules.rs` conventions)
- `GET/POST /api/crm/companies`, `GET/PATCH/DELETE /api/crm/companies/{id}` — same for `contacts`, `deals`.
  List params: `q`, `tag`, `stage` (deals), `company_id`, `dnc` (contacts), `sort` (`updated`|`name`|`fit`), `limit`,
  `offset`. Rows include small joins: a contact's `company_name`, a deal's `company_name` + `contact_name`.
- `GET /api/crm/activities?company_id|contact_id|deal_id`, `POST /api/crm/activities`.
- `GET /api/crm/pipeline`.
- `GET /api/crm/changes?entity&entity_id&bot_id&limit`, `POST /api/crm/changes/{id}/undo`.
- `GET /api/crm/export.csv?kind=companies|contacts|deals` (UTF-8, header row, RFC 4180 quoting; also neutralise
  cells starting with `=`, `+`, `-`, `@` by prefixing `'` against spreadsheet formula injection).
- `POST /api/crm/import?kind=…&dry_run=true|false` with the CSV as the body: ≤ 5 MB, ≤ 5000 rows; header names map to
  fields; dedupe as the upserts do; returns `{created, updated, skipped, errors:[{row, message}]}`; dry run changes
  nothing.
- All owner-scoped (`where owner_id = $user`), soft-deleted rows hidden, request bodies size-limited, errors as
  `ApiError::bad` with a clear message.

### Client (`crates/familiar-client`)
Types `CrmCompany`, `CrmContact`, `CrmDeal`, `CrmActivity`, `CrmChange`, `PipelineStage`, `CrmImportResult`, inputs
`NewCompany`/`CompanyPatch` (etc.); methods `crm_companies(params)`, `crm_company(id)`, `create_crm_company`,
`update_crm_company`, `delete_crm_company` (same for contacts and deals), `crm_activities`, `log_crm_activity`,
`crm_pipeline`, `crm_changes`, `undo_crm_change`, `crm_export_csv(kind) -> String`, `crm_import(kind, csv, dry_run)`.
Follow the existing type tests (`tests/types.rs`) and add a live test like the existing one.

### Tests (phase 1 acceptance)
- Unit: every normaliser and validator, including bad input.
- API: CRUD per entity; dedupe (same domain twice → one company; same email different case → one contact); soft delete
  hides from lists and frees the unique key; second owner sees nothing (copy `second_user_sees_nothing`); pipeline
  counts and value; change log on every write; undo for create/update/delete; undo of a non-newest change → 409; a
  bot actor cannot clear do_not_contact; CSV export round-trips through a dry-run import with zero creates; formula
  cells neutralised; import limits.
- Integration tests need Postgres: never the user's real DB (built-in Postgres on 127.0.0.1:47432, database `zed`)
  or their PostgreSQL 17 on 5432. Make a throwaway cluster from `C:\Users\Admin\.familiar\pg\18.6.0\bin` (initdb into a
  temp dir, start with PowerShell `Start-Process -WindowStyle Hidden`, `--test-threads=4`, then stop and delete it).

## Phase 2 — tools, trust, webhooks, crew (Opus: judgment)

### Teammate tools (in `mcp.rs`, Familiar's own server, so pre-allowed like `remember`)
`crm_search {query, kind?, stage?, tags?, limit≤50}`, `crm_get {kind, id}`, `crm_upsert_company`,
`crm_upsert_contact`, `crm_upsert_deal`, `crm_move_deal {deal_id, stage, note}`, `crm_log_activity`. All call
`familiar_core::crm` with `Actor::Bot`. No delete tool. New records must carry `source_urls`. Research-only runs may
search and read but not write. Size limits as in phase 1.

Trust model (decide and document in ARCHITECTURE.md):
- CRM text is partly copied from web pages, so a record can carry instructions aimed at the next teammate that reads
  it. Records returned by `crm_search`/`crm_get` wrap their free-text fields in nonce-tagged untrusted-data fences
  (the pattern in `reviewer.rs`) with a one-line reminder that fenced text is data.
- `do_not_contact` contacts come back flagged; `propose_draft` refuses a draft whose `to` matches a do-not-contact
  email or handle in the CRM (check at proposal time and again when the follow-up run is created).
- Bulk-write guard: at most N CRM writes per run (e.g. 200) to stop a runaway loop; beyond it the tool fails with a
  clear message.

### Webhooks
- API (owner only; teammates never): CRUD `/api/crm/webhooks`, `POST /api/crm/webhooks/{id}/test`,
  `GET /api/crm/webhooks/{id}/deliveries`. Secret generated server-side, shown once, stored with `SecretBox`.
- Events: `company.created`, `company.updated`, `contact.created`, `contact.updated`, `contact.do_not_contact`,
  `deal.created`, `deal.updated`, `deal.stage_changed`, `activity.created`. Payload
  `{id, event, at, data, previous?, actor:{kind, bot_slug?}}`.
- `enqueue_webhooks` writes deliveries in the same transaction as the change; the daemon's tick delivers them: POST,
  10 s timeout, no redirects, ≤ 256 KB, headers `Familiar-Event`, `Familiar-Delivery`,
  `Familiar-Signature: t=<unix>,v1=<hex HMAC-SHA256 of "<t>.<body>">`; retries at 1 m, 5 m, 30 m, 2 h, 6 h, then
  `failed`.
- Address rules, checked when saved and again (after DNS) at every delivery: `https` to any public address; `http` only
  to loopback; never link-local / cloud-metadata / multicast / unspecified; decide (and document) whether private LAN
  addresses are allowed with https.

### GTM crew (templates.json + the template test)
- Lead researcher → writes companies, contacts and `new` deals with `fit_score`, `fit_reason` (a real reason to
  contact) and `source_urls`; only public business contact info; never buys lists or guesses personal emails.
- Outbound drafter → takes `new` deals (skipping do-not-contact), proposes drafts with CRM ids in the note; after
  approval the follow-up run sends, logs `email_sent`/`dm_sent`, moves the deal to `contacted`. First emails carry a
  one-line opt-out; a "stop/unsubscribe" reply → do-not-contact.
- New "Reply & follow-up tracker" → twice a day matches inbox replies to CRM contacts, logs `email_received`, moves
  deals (`replied`/`meeting`/`lost`), drafts at most 2 follow-ups after N days of silence.
- Weekly metrics reporter and Inbox assistant → read/write the CRM (pipeline summary; inbound leads become deals).
- Bundle: `GET /api/templates/bundles`, `POST /api/templates/bundles/gtm-crew/create {answers}` hires the whole crew in
  one transaction from shared questions (product, ideal customer, offer/ask, sender, voice, follow-up days); every
  schedule off.

## Phase 4 — native app (Opus)
CRM in the sidebar. Tabs Companies · Contacts · Pipeline · Activity. Table with search, filters and sort; record panel
(fields editable inline, contacts, deals, activity timeline linking to the approval/draft, do-not-contact toggle,
"Changed by <teammate> · Undo"); pipeline board by stage with counts and value (move via a stage menu); CSV import
(dry-run preview first) and export; empty state with "Hire the GTM crew"; webhook settings (add URL + events, secret
shown once, Test, recent deliveries). Live updates through the existing stream.

## Out of scope for this round
Sending email from Familiar's own address (later: teammate identity), LinkedIn/X connectors, sequences with A/B tests,
two-way sync with a specific CRM.
