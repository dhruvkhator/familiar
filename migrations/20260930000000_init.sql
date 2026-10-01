-- zed: initial schema. Plain PostgreSQL (13+), no provider-specific features. See docs/ARCHITECTURE.md.

create extension if not exists pgcrypto;

create table public.users (
  id uuid primary key default gen_random_uuid(),
  email text not null unique,
  password_hash text not null,
  created_at timestamptz not null default now()
);

create table public.sessions (
  token_hash text primary key, -- sha256 hex of the bearer token
  user_id uuid not null references public.users(id) on delete cascade,
  created_at timestamptz not null default now(),
  expires_at timestamptz not null
);

-- ---------------------------------------------------------------- tables
create table public.bots (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  slug text not null,
  name text not null,
  persona text not null default '',
  model text not null default 'sonnet',
  paused boolean not null default false,
  created_at timestamptz not null default now(),
  unique (owner_id, slug)
);

create table public.threads (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  bot_id uuid not null references public.bots(id) on delete cascade,
  title text not null default 'New thread',
  claude_session_id uuid,
  schedule_id uuid, -- FK added after schedules exists (circular)
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);

create table public.runs (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  bot_id uuid not null references public.bots(id) on delete cascade,
  thread_id uuid not null references public.threads(id) on delete cascade,
  kind text not null default 'chat' check (kind in ('chat','scheduled','proactive','handoff')),
  prompt text not null,
  status text not null default 'queued'
    check (status in ('queued','running','waiting_approval','succeeded','failed','cancelled')),
  error text,
  cost_usd numeric,
  usage jsonb,
  parent_run_id uuid references public.runs(id) on delete set null,
  started_at timestamptz,
  finished_at timestamptz,
  created_at timestamptz not null default now()
);

create table public.messages (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  thread_id uuid not null references public.threads(id) on delete cascade,
  role text not null check (role in ('user','assistant','system')),
  content text not null,
  run_id uuid references public.runs(id) on delete set null, -- assistant messages only
  created_at timestamptz not null default now()
);

create table public.events (
  id bigserial primary key,
  owner_id uuid not null references public.users(id) on delete cascade,
  run_id uuid not null references public.runs(id) on delete cascade,
  seq int not null,
  kind text not null
    check (kind in ('status','text','thinking','tool_call','tool_result','approval','artifact','error','result','rate_limit')),
  payload jsonb not null default '{}'::jsonb,
  created_at timestamptz not null default now(),
  unique (run_id, seq)
);

create table public.approvals (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  run_id uuid not null references public.runs(id) on delete cascade,
  bot_id uuid not null references public.bots(id) on delete cascade,
  tool_use_id text,
  tool_name text not null,
  input jsonb,
  reason text,
  status text not null default 'pending' check (status in ('pending','approved','denied','expired')),
  decided_by text check (decided_by in ('user','rule','reviewer')),
  response text,
  decided_at timestamptz,
  created_at timestamptz not null default now()
);

create table public.rules (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  bot_id uuid references public.bots(id) on delete cascade, -- null = all bots
  pattern text not null,
  decision text not null check (decision in ('allow','deny','ask','review')),
  note text,
  created_at timestamptz not null default now()
);

create table public.schedules (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  bot_id uuid not null references public.bots(id) on delete cascade,
  thread_id uuid references public.threads(id) on delete cascade,
  cron text not null,
  prompt text not null,
  kind text not null default 'scheduled' check (kind in ('scheduled','proactive')),
  enabled boolean not null default true,
  last_run_at timestamptz,
  next_run_at timestamptz
);

-- threads.schedule_id: deferred so the BEFORE INSERT trigger on schedules can create the thread first
alter table public.threads
  add constraint threads_schedule_id_fkey foreign key (schedule_id)
  references public.schedules(id) on delete set null deferrable initially deferred;

create table public.memories (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  bot_id uuid not null references public.bots(id) on delete cascade,
  content text not null,
  source text not null default 'user' check (source in ('user','bot')),
  created_at timestamptz not null default now()
);

create table public.skills (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  bot_id uuid not null references public.bots(id) on delete cascade,
  name text not null,
  description text,
  body text not null default '',
  updated_at timestamptz not null default now()
);

create table public.artifacts (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  run_id uuid references public.runs(id) on delete cascade,
  bot_id uuid not null references public.bots(id) on delete cascade,
  r2_key text not null,
  mime text,
  bytes bigint,
  signed_url text,
  signed_until timestamptz,
  created_at timestamptz not null default now()
);

create table public.devices (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  name text not null,
  version text,
  last_seen_at timestamptz not null default now()
);

-- ---------------------------------------------------------------- indexes
create index runs_owner_status_created_idx on public.runs (owner_id, status, created_at);
create index runs_thread_created_idx on public.runs (thread_id, created_at);
create index runs_bot_idx on public.runs (bot_id);
create index events_run_seq_idx on public.events (run_id, seq);
create index events_created_idx on public.events (created_at);
create index messages_thread_created_idx on public.messages (thread_id, created_at);
create index approvals_owner_status_idx on public.approvals (owner_id, status);
create index approvals_run_idx on public.approvals (run_id);
create index schedules_enabled_next_idx on public.schedules (enabled, next_run_at);
create index threads_bot_idx on public.threads (bot_id);


-- ---------------------------------------------------------------- triggers
-- user message -> queued chat run, bump thread
create function public.zed_on_user_message() returns trigger
language plpgsql as $$
declare v_bot uuid;
begin
  update public.threads set updated_at = now() where id = NEW.thread_id returning bot_id into v_bot;
  if NEW.role = 'user' and v_bot is not null then
    insert into public.runs (owner_id, bot_id, thread_id, kind, prompt, status)
    values (NEW.owner_id, v_bot, NEW.thread_id, 'chat', NEW.content, 'queued');
  end if;
  return NEW;
end $$;

create trigger messages_after_insert_user
  after insert on public.messages
  for each row execute function public.zed_on_user_message();

-- wake-up signal for the daemon (LISTEN zed)
create function public.zed_notify() returns trigger
language plpgsql as $$
declare r jsonb := to_jsonb(case when TG_OP = 'DELETE' then OLD else NEW end);
begin
  perform pg_notify('zed', json_build_object('t', TG_TABLE_NAME, 'id', r->>'id', 'op', TG_OP,
    'owner', r->>'owner_id', 'run', r->>'run_id', 'bot', r->>'bot_id')::text);
  return null;
end $$;
create trigger runs_notify after insert or update on public.runs
  for each row execute function public.zed_notify();
create trigger approvals_notify after insert or update on public.approvals
  for each row execute function public.zed_notify();
create trigger messages_notify after insert on public.messages
  for each row execute function public.zed_notify();
create trigger schedules_notify after insert or update on public.schedules
  for each row execute function public.zed_notify();

-- schedule without a thread -> create one
create function public.zed_schedule_thread() returns trigger
language plpgsql as $$
declare v_thread uuid;
begin
  if NEW.thread_id is null then
    insert into public.threads (owner_id, bot_id, title, schedule_id)
    values (NEW.owner_id, NEW.bot_id, 'Schedule: ' || left(NEW.prompt, 40), NEW.id)
    returning id into v_thread;
    NEW.thread_id := v_thread;
  end if;
  return NEW;
end $$;

create trigger schedules_before_insert_thread
  before insert on public.schedules
  for each row execute function public.zed_schedule_thread();

-- ---------------------------------------------------------------- cron
create function public.zed_enqueue_due_schedules() returns integer
language plpgsql as $$
declare s record; n integer := 0;
begin
  for s in
    select sc.* from public.schedules sc
    join public.bots b on b.id = sc.bot_id
    where sc.enabled and sc.next_run_at is not null and sc.next_run_at <= now() and not b.paused
    for update of sc skip locked
  loop
    insert into public.runs (owner_id, bot_id, thread_id, kind, prompt, status)
    values (s.owner_id, s.bot_id, s.thread_id, s.kind, s.prompt, 'queued');
    update public.schedules set last_run_at = now(), next_run_at = null where id = s.id;
    n := n + 1;
  end loop;
  return n;
end $$;




-- ---------------------------------------------------------------- view
create view public.bot_status as
select
  b.id,
  case
    when exists (select 1 from public.runs r where r.bot_id = b.id and r.status in ('running','waiting_approval')) then 'running'
    when b.paused then 'paused'
    else 'idle'
  end as status,
  (select max(r.created_at) from public.runs r where r.bot_id = b.id) as last_run_at
from public.bots b;

-- ---------------------------------------------------------------- schedule edits
-- A changed cron (or re-enabling) clears next_run_at; the daemon recomputes it from the new expression.
create or replace function public.zed_schedule_reset_next()
returns trigger language plpgsql as $$
begin
  if new.cron is distinct from old.cron or (new.enabled and not old.enabled) then
    new.next_run_at := null;
  end if;
  return new;
end $$;

create trigger schedules_before_update_reset_next
  before update on public.schedules
  for each row execute function public.zed_schedule_reset_next();

-- API server forwards these to the UI's live stream.
create trigger events_notify after insert on public.events
  for each row execute function public.zed_notify();
create trigger threads_notify after insert or update or delete on public.threads
  for each row execute function public.zed_notify();
create trigger bots_notify after insert or update on public.bots
  for each row execute function public.zed_notify();
create trigger memories_notify after insert on public.memories
  for each row execute function public.zed_notify();
