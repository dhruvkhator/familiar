-- One approval per outgoing message (familiar_core::drafts, "Sending an approved draft").
-- `send_granted` (on a draft): its follow-up run was given the approved text to post or send, so that run may send it
--   once without asking again (only exactly as approved; see familiar_core::drafts::send_matches).
-- `draft_id` (on a tool call's approval): the approved draft this call sent, decided by Familiar ("sent as approved").
--   At most one per draft: the pre-approval is used up by its first send.
alter table public.approvals add column send_granted boolean not null default false;
alter table public.approvals add column draft_id uuid references public.approvals(id) on delete cascade;
create unique index approvals_one_send_per_draft on public.approvals (draft_id) where draft_id is not null;
