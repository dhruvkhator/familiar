-- Rename zed -> Familiar: SQL functions, NOTIFY channel, reserved connector name.
-- Earlier migrations are untouched (sqlx checksums); triggers follow the renamed functions automatically.

alter function public.zed_notify() rename to familiar_notify;
alter function public.zed_frame_notify() rename to familiar_frame_notify;
alter function public.zed_on_user_message() rename to familiar_on_user_message;
alter function public.zed_schedule_thread() rename to familiar_schedule_thread;
alter function public.zed_schedule_reset_next() rename to familiar_schedule_reset_next;
alter function public.zed_enqueue_due_schedules() rename to familiar_enqueue_due_schedules;

-- NOTIFY channel 'zed' -> 'familiar' (bodies otherwise unchanged)
create or replace function public.familiar_notify() returns trigger
language plpgsql as $$
declare r jsonb := to_jsonb(case when TG_OP = 'DELETE' then OLD else NEW end);
begin
  perform pg_notify('familiar', json_build_object('t', TG_TABLE_NAME, 'id', r->>'id', 'op', TG_OP,
    'owner', r->>'owner_id', 'run', r->>'run_id', 'bot', r->>'bot_id')::text);
  return null;
end $$;

create or replace function public.familiar_frame_notify() returns trigger language plpgsql as $$
begin
  perform pg_notify('familiar', json_build_object('t', 'live_frames', 'id', NEW.bot_id, 'op', TG_OP,
    'owner', NEW.owner_id, 'bot', NEW.bot_id)::text);
  return null;
end $$;

-- The bot's own MCP server is now 'familiar', so that name is reserved instead of 'zed'.
update public.connectors set name = name || '_custom' where name = 'familiar';
alter table public.connectors drop constraint connectors_name_check;
alter table public.connectors add constraint connectors_name_check
  check (name ~ '^[a-z0-9][a-z0-9_-]{0,31}$' and name not in ('familiar', 'browser'));
