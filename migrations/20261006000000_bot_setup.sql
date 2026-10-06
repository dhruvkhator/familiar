-- Teammates hired from a template remember what the template set up, for the teammate page's "Set up" checklist:
-- {"template": id, "logins": [{"site", "url", "done"}], "schedules": [schedule ids], "dismissed": bool}.
alter table public.bots add column setup jsonb;
