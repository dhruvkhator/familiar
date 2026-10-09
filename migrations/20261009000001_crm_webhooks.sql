-- CRM webhooks, phase 2: the daemon delivers queued deliveries (familiar_core::crm::webhooks). A new delivery wakes
-- it through the usual notice, and the app sees a delivery's status change live.
create trigger crm_webhook_deliveries_notify after insert or update on public.crm_webhook_deliveries
  for each row execute function public.familiar_notify();

-- The API lists a webhook's deliveries newest first, and an owner's webhooks oldest first.
create index crm_webhook_deliveries_webhook on public.crm_webhook_deliveries (webhook_id, created_at desc);
create index crm_webhooks_owner on public.crm_webhooks (owner_id, created_at);

alter table public.crm_webhooks add constraint crm_webhooks_events_check
  check (cardinality(events) between 1 and 9);
