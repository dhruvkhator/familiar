-- A conversation that read the owner's mail or calendar (familiar_core::google::tools) is marked: from then on
-- anything in it that reaches the internet asks the owner (runner::decide_tool), since an email may have planted
-- instructions to carry the mail out. Persisted, so a resumed session keeps it; a handoff passes it on.
alter table public.threads add column read_mail_at timestamptz;
