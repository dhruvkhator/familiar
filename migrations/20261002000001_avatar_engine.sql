-- Customizable avatars (Dots-style "make it your own") and a per-bot engine: Claude Code or Codex CLI.
alter table public.bots add column avatar jsonb;
alter table public.bots add column engine text not null default 'claude' check (engine in ('claude', 'codex'));
-- Codex identifies conversations by its own thread id (a string), Claude by a session uuid.
alter table public.threads add column codex_thread_id text;
