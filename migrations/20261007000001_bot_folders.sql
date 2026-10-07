-- Folders on this PC the owner shares with a teammate (Cowork-style). `path` is the folder as Familiar resolved it when
-- it was added (links and junctions followed); the daemon checks it again at the start of every run. `mode`: read
-- (read only, the default) or write (read & write; every change still asks the owner).
create table public.bot_folders (
  id uuid primary key default gen_random_uuid(),
  owner_id uuid not null references public.users(id) on delete cascade,
  bot_id uuid not null references public.bots(id) on delete cascade,
  path text not null check (length(path) between 1 and 1000),
  mode text not null default 'read' check (mode in ('read', 'write')),
  created_at timestamptz not null default now(),
  unique (bot_id, path)
);
create trigger bot_folders_notify after insert or update or delete on public.bot_folders
  for each row execute function public.familiar_notify();
