-- When a pending approval stops waiting (30 minutes for a tool call or question, a day for a draft). The daemon
-- expires it then; until its sweep does, the API already treats it as expired, so a late click can't approve it or
-- add an "Always allow" rule.
alter table public.approvals add column expires_at timestamptz;
