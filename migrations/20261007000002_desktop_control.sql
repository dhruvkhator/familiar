-- "Can use this PC's desktop": the teammate may use the mouse, keyboard and screen through Windows-MCP, one approved
-- step at a time. Off by default.
alter table public.bots add column desktop boolean not null default false;

-- A desktop step's small picture of the owner's screen around its target, for the approval card only (the teammate never
-- gets it). Kept only while the request waits: any decision drops it.
alter table public.approvals add column preview bytea;
create function public.familiar_drop_preview() returns trigger language plpgsql as $$
begin
  if NEW.status <> 'pending' then
    NEW.preview := null;
  end if;
  return NEW;
end $$;
create trigger approvals_drop_preview before update on public.approvals
  for each row execute function public.familiar_drop_preview();
