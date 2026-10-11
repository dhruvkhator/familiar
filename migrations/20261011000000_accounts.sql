-- Connected accounts (familiar_core::google): the owner's Gmail / Google Calendar, connected in Integrations →
-- Accounts with "Connect Google" (OAuth; `kind` google) or a Gmail app password (IMAP/SMTP; `kind`
-- gmail_app_password). One of each per owner.
-- `secret_enc`: the refresh token or the app password, sealed with SecretBox (FAMILIAR_SECRET_KEY); never returned by
--   the API, never logged. Access tokens are never stored (the daemon keeps them in memory).
-- `status`: ok | reconnect (Google refused the refresh token, or Gmail the app password: the owner connects again).
create table public.accounts (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  kind text not null check (kind in ('google', 'gmail_app_password')),
  email text not null check (char_length(email) between 3 and 320),
  scopes text[] not null default '{}',
  secret_enc text not null,
  status text not null default 'ok' check (status in ('ok', 'reconnect')),
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  unique (owner_id, kind)
);

-- Which teammates may use an account (the owner ticks them). Changing the links touches accounts.updated_at, so the
-- app hears about it through the accounts notice.
create table public.bot_accounts (
  bot_id uuid not null references public.bots(id) on delete cascade,
  account_id uuid not null references public.accounts(id) on delete cascade,
  primary key (bot_id, account_id)
);
create index bot_accounts_account on public.bot_accounts (account_id);

create trigger accounts_notify after insert or update or delete on public.accounts
  for each row execute function public.familiar_notify();
