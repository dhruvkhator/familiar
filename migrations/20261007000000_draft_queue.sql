-- Drafts no longer hold their teammate up. `propose_draft` queues the draft and returns at once; when the owner decides
-- (approve, approve with edits, ask for changes, reject), the daemon queues a `followup` run on the draft's thread with
-- a message it writes itself from this row. Undecided drafts expire after a week.
-- `followed_up_at`: when the daemon handled the decision (queued the follow-up, or expired the draft); null = not yet.
-- `followup_run_id`: the run that carried the decision to the teammate.
alter table public.approvals add column followed_up_at timestamptz;
alter table public.approvals add column followup_run_id uuid references public.runs(id) on delete set null;

-- Drafts decided before this change were answered inside their (blocking) run: nothing left to follow up.
update public.approvals set followed_up_at = coalesce(decided_at, now())
 where tool_name = 'propose_draft' and status <> 'pending';

-- Decided drafts the daemon has not handled yet.
create index approvals_draft_followup_idx on public.approvals (owner_id)
  where tool_name = 'propose_draft' and followed_up_at is null;

-- `followup`: a run the daemon queues to tell a teammate the owner's decision on its draft.
alter table public.runs drop constraint runs_kind_check;
alter table public.runs add constraint runs_kind_check
  check (kind in ('chat', 'scheduled', 'proactive', 'handoff', 'dream', 'followup'));
