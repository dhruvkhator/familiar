-- Telegram long-poll position, so restarts and re-pairing never replay already-handled updates.
alter table public.channels add column update_offset bigint not null default 0;
