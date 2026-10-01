-- Artifacts can live in Postgres (small files, zero setup) or any S3-compatible store (R2).
alter table public.artifacts
  add column name text not null default '',
  add column storage text not null default 'db' check (storage in ('db', 's3')),
  add column data bytea,
  alter column r2_key drop not null;

-- The daemon mirrors <workspace>/.claude/skills/<name>/SKILL.md one row per skill.
alter table public.skills add constraint skills_bot_name_key unique (bot_id, name);

create trigger skills_notify after insert or update on public.skills
  for each row execute function public.zed_notify();
create trigger artifacts_notify after insert on public.artifacts
  for each row execute function public.zed_notify();
