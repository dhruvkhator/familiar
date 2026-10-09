-- A teammate run's CRM write budget counts its changes on every write (familiar_core::crm::budget).
create index crm_changes_run on public.crm_changes (run_id) where run_id is not null;
