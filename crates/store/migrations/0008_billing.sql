-- M7 billing (Dodo Payments): the ids that tie an account to its customer and subscription,
-- the timestamp of the last webhook applied (older events are ignored), and a record of every
-- webhook delivery so a replay changes nothing.
ALTER TABLE accounts
  ADD COLUMN dodo_customer_id TEXT,
  ADD COLUMN dodo_subscription_id TEXT,
  ADD COLUMN billing_updated_at TIMESTAMPTZ;

CREATE INDEX accounts_dodo_subscription_idx ON accounts (dodo_subscription_id)
  WHERE dodo_subscription_id IS NOT NULL;
CREATE INDEX accounts_dodo_customer_idx ON accounts (dodo_customer_id)
  WHERE dodo_customer_id IS NOT NULL;

-- `event_id` is the `webhook-id` header: Dodo resends the same id on every retry.
CREATE TABLE billing_events (
  event_id TEXT PRIMARY KEY,
  type TEXT NOT NULL,
  received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  payload JSONB NOT NULL
);
