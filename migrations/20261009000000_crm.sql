-- The built-in CRM: companies, contacts, deals, an activity timeline, a change log (for "who changed this" and undo)
-- and outgoing webhooks. All writes go through familiar_core::crm. Companies, contacts and deals are soft-deleted
-- (`deleted_at`) so a delete can be undone; the unique keys only cover live rows. Text columns carry the same length
-- limits the API checks, so a teammate's tool call can't get around them.
create table public.crm_companies (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  name text not null check (length(name) between 1 and 200),
  -- lowercase host without scheme, path or a leading www.
  domain text check (length(domain) between 1 and 253),
  website text check (length(website) <= 500),
  industry text check (length(industry) <= 200),
  size text check (length(size) <= 200),
  location text check (length(location) <= 200),
  description text check (length(description) <= 4000),
  fit_score int check (fit_score between 0 and 100),
  fit_reason text check (length(fit_reason) <= 2000),
  tags text[] not null default '{}',
  source_urls text[] not null default '{}',
  custom jsonb not null default '{}',
  created_by_bot uuid references public.bots(id) on delete set null,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  deleted_at timestamptz
);
create unique index crm_companies_domain on public.crm_companies (owner_id, domain)
  where domain is not null and deleted_at is null;
create index crm_companies_owner on public.crm_companies (owner_id, updated_at desc) where deleted_at is null;
create trigger crm_companies_notify after insert or update or delete on public.crm_companies
  for each row execute function public.familiar_notify();

create table public.crm_contacts (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  company_id uuid references public.crm_companies(id) on delete set null,
  name text not null check (length(name) between 1 and 200),
  title text check (length(title) <= 200),
  email text check (length(email) <= 320 and email = lower(email)),
  linkedin_url text check (length(linkedin_url) <= 500),
  x_handle text check (length(x_handle) <= 100),
  notes text check (length(notes) <= 4000),
  tags text[] not null default '{}',
  source_urls text[] not null default '{}',
  custom jsonb not null default '{}',
  -- A contact who asked not to be contacted: anyone can set it, only the owner can clear it.
  do_not_contact boolean not null default false,
  dnc_reason text check (length(dnc_reason) <= 1000),
  dnc_at timestamptz,
  created_by_bot uuid references public.bots(id) on delete set null,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  deleted_at timestamptz
);
create unique index crm_contacts_email on public.crm_contacts (owner_id, email)
  where email is not null and deleted_at is null;
create index crm_contacts_owner on public.crm_contacts (owner_id, updated_at desc) where deleted_at is null;
create index crm_contacts_company on public.crm_contacts (company_id);
create trigger crm_contacts_notify after insert or update or delete on public.crm_contacts
  for each row execute function public.familiar_notify();

create table public.crm_deals (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  company_id uuid not null references public.crm_companies(id) on delete cascade,
  contact_id uuid references public.crm_contacts(id) on delete set null,
  title text not null check (length(title) between 1 and 200),
  stage text not null default 'new'
    check (stage in ('new', 'researching', 'contacted', 'replied', 'meeting', 'proposal', 'won', 'lost')),
  stage_changed_at timestamptz not null default now(),
  value_cents bigint check (value_cents >= 0),
  currency char(3) not null default 'USD',
  next_step text check (length(next_step) <= 500),
  next_step_at timestamptz,
  created_by_bot uuid references public.bots(id) on delete set null,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  deleted_at timestamptz
);
create index crm_deals_owner on public.crm_deals (owner_id, stage) where deleted_at is null;
create index crm_deals_company on public.crm_deals (company_id);
create trigger crm_deals_notify after insert or update or delete on public.crm_deals
  for each row execute function public.familiar_notify();

-- What happened with a company, contact or deal. `actor_kind`: who wrote it (a teammate, or the owner).
create table public.crm_activities (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  company_id uuid references public.crm_companies(id) on delete set null,
  contact_id uuid references public.crm_contacts(id) on delete set null,
  deal_id uuid references public.crm_deals(id) on delete set null,
  kind text not null check (kind in ('note', 'research', 'email_sent', 'email_received', 'dm_sent', 'dm_received',
                                     'post', 'call', 'meeting', 'stage_change')),
  summary text not null check (length(summary) between 1 and 500),
  body text check (length(body) <= 20000),
  url text check (length(url) <= 1000),
  approval_id uuid references public.approvals(id) on delete set null,
  bot_id uuid references public.bots(id) on delete set null,
  actor_kind text not null check (actor_kind in ('bot', 'user')),
  occurred_at timestamptz not null default now(),
  created_at timestamptz not null default now(),
  check (company_id is not null or contact_id is not null or deal_id is not null)
);
create index crm_activities_company on public.crm_activities (company_id, occurred_at desc);
create index crm_activities_contact on public.crm_activities (contact_id, occurred_at desc);
create index crm_activities_deal on public.crm_activities (deal_id, occurred_at desc);
create index crm_activities_owner on public.crm_activities (owner_id, occurred_at desc);
create trigger crm_activities_notify after insert or update or delete on public.crm_activities
  for each row execute function public.familiar_notify();

-- Every write: the row before and after (as JSON), by whom. Only the newest change of a record can be undone.
-- `at` is the clock time (not the transaction start) so changes of one record always sort in the order they happened.
create table public.crm_changes (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  entity text not null check (entity in ('company', 'contact', 'deal', 'activity')),
  entity_id uuid not null,
  op text not null check (op in ('create', 'update', 'delete', 'undo')),
  before jsonb,
  after jsonb,
  actor_kind text not null check (actor_kind in ('bot', 'user')),
  bot_id uuid references public.bots(id) on delete set null,
  run_id uuid references public.runs(id) on delete set null,
  at timestamptz not null default clock_timestamp(),
  undone_at timestamptz
);
create index crm_changes_entity on public.crm_changes (owner_id, entity, entity_id, at desc);

-- Outgoing webhooks (the API and delivery come later; the table is here so the data layer can queue deliveries).
create table public.crm_webhooks (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  url text not null check (length(url) <= 1000),
  secret_enc text not null,
  events text[] not null,
  enabled boolean not null default true,
  created_at timestamptz not null default now()
);

create table public.crm_webhook_deliveries (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  webhook_id uuid not null references public.crm_webhooks(id) on delete cascade,
  event text not null,
  payload jsonb not null,
  status text not null default 'pending' check (status in ('pending', 'delivered', 'failed')),
  attempts int not null default 0,
  next_attempt_at timestamptz not null default now(),
  last_error text,
  created_at timestamptz not null default now(),
  delivered_at timestamptz
);
create index crm_webhook_deliveries_due on public.crm_webhook_deliveries (status, next_attempt_at);
