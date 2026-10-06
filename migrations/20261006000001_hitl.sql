-- Human in the loop: drafts the owner approves with edits, tool inputs the owner edits before approving, and
-- "Always allow this" straight from an approval card.
-- `editable`: the input fields the owner may change before approving (the daemon decides, per engine and tool).
-- `allow_rule`: the narrowest owner rule "Always allow this" adds (null = not offered: always-human actions, drafts,
--   questions, browser navigation).
-- `edited_input`: what the owner approved when they changed it; `input` keeps what the teammate proposed.
alter table public.approvals add column editable text[] not null default '{}';
alter table public.approvals add column allow_rule text;
alter table public.approvals add column edited_input jsonb;

-- `revise`: "Ask for changes" on a draft; the teammate revises it and proposes it again.
alter table public.approvals drop constraint approvals_status_check;
alter table public.approvals add constraint approvals_status_check
  check (status in ('pending', 'approved', 'denied', 'expired', 'revise'));
