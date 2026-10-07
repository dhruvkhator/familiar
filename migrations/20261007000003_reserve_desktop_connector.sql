-- `desktop` is Familiar's own MCP server name (Windows-MCP, desktop control), so connectors can't use it.
update public.connectors set name = name || '_custom' where name = 'desktop';
alter table public.connectors drop constraint connectors_name_check;
alter table public.connectors add constraint connectors_name_check
  check (name ~ '^[a-z0-9][a-z0-9_-]{0,31}$' and name not in ('familiar', 'browser', 'desktop'));
