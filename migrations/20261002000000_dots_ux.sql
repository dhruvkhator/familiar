-- Dots-style UX: memory review ("what I learned"), nightly dreaming runs, live view of each bot's browser.

-- Memories can be proposed (by the nightly dream run) and wait for the owner to accept, edit or reject them.
alter table public.memories add column status text not null default 'active'
  check (status in ('active', 'proposed', 'rejected'));
alter table public.memories add column updated_at timestamptz not null default now();
create trigger memories_update_notify after update or delete on public.memories
  for each row execute function public.zed_notify();

-- `dream`: the nightly, research-only run that reviews recent work and proposes memories.
alter table public.runs drop constraint runs_kind_check;
alter table public.runs add constraint runs_kind_check
  check (kind in ('chat', 'scheduled', 'proactive', 'handoff', 'dream'));
alter table public.bots add column last_dreamed_at timestamptz;

-- Latest frame of each bot's browser (one row per bot, overwritten ~1/s while the browser is open).
create table public.live_frames (
  bot_id uuid primary key references public.bots(id) on delete cascade,
  owner_id uuid not null references public.users(id) on delete cascade,
  jpeg bytea not null,
  url text,
  title text,
  width int,
  height int,
  updated_at timestamptz not null default now()
);
-- Frames notify without the image (the UI fetches it); input from "take over" goes daemon-wards on `zed_input`.
create function public.zed_frame_notify() returns trigger language plpgsql as $$
begin
  perform pg_notify('zed', json_build_object('t', 'live_frames', 'id', NEW.bot_id, 'op', TG_OP,
    'owner', NEW.owner_id, 'bot', NEW.bot_id)::text);
  return null;
end $$;
create trigger live_frames_notify after insert or update on public.live_frames
  for each row execute function public.zed_frame_notify();
