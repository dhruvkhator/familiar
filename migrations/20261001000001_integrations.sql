-- External integrations: MCP connectors, chat channels, webhook triggers.
-- Secrets are AES-256-GCM encrypted with ZED_SECRET_KEY (shared by zed-server and the daemon), never stored plain.

create table public.connectors (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  name text not null,                       -- MCP server name the bot sees: tools are mcp__<name>__*
  preset text,                              -- catalog id it was created from (github, notion, ...), informational
  transport text not null check (transport in ('stdio', 'http')),
  command text,                             -- stdio
  args jsonb not null default '[]'::jsonb,  -- stdio
  url text,                                 -- http
  secrets_enc text,                         -- encrypted JSON {"env": {..}, "headers": {..}}
  enabled boolean not null default true,
  created_at timestamptz not null default now(),
  unique (owner_id, name),
  check (name ~ '^[a-z0-9][a-z0-9_-]{0,31}$' and name not in ('zed', 'browser'))
);

create table public.bot_connectors (
  bot_id uuid not null references public.bots(id) on delete cascade,
  connector_id uuid not null references public.connectors(id) on delete cascade,
  primary key (bot_id, connector_id)
);

create table public.channels (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  kind text not null check (kind in ('telegram')),
  config_enc text not null,                 -- encrypted JSON, telegram: {"token": "..."}
  chat_id text,                             -- bound by the daemon after the owner sends /start <pair_code>
  pair_code text,                           -- one-time code shown in the UI until bound
  default_bot_id uuid references public.bots(id) on delete set null,
  enabled boolean not null default true,
  created_at timestamptz not null default now(),
  unique (owner_id, kind)
);

create table public.triggers (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  bot_id uuid not null references public.bots(id) on delete cascade,
  thread_id uuid references public.threads(id) on delete set null,
  name text not null,
  token_hash text not null unique,          -- sha256 hex of the secret in the webhook URL
  prompt text not null,                     -- instructions; the request body is appended
  kind text not null default 'scheduled' check (kind in ('scheduled', 'proactive')),
  enabled boolean not null default true,
  last_fired_at timestamptz,
  created_at timestamptz not null default now()
);

-- Telegram threads, trigger threads: remember which surface a thread belongs to.
alter table public.threads add column source text not null default 'app'
  check (source in ('app', 'schedule', 'telegram', 'trigger', 'handoff'));

create trigger connectors_notify after insert or update or delete on public.connectors
  for each row execute function public.zed_notify();
create trigger channels_notify after insert or update or delete on public.channels
  for each row execute function public.zed_notify();

-- Script gate: a cheap shell command run before a scheduled run; empty output = skip (saves subscription quota),
-- otherwise its output is appended to the prompt.
alter table public.schedules add column gate_command text;

-- Daemon status for the UI (subscription utilization, throttle, version...).
alter table public.devices add column info jsonb not null default '{}'::jsonb;
