-- M5: the web app. Login deduplication, the self-hosted owner, instance settings, and an index
-- for the manual-crawl allowance check.

-- The deduplication key for an email address (lowercased, `+tag` dropped, Gmail dots removed).
-- `email` keeps the address as typed, for sending. Every account the web app creates sets it;
-- it stays nullable so rows written by older code paths (and tests) remain valid.
ALTER TABLE accounts ADD COLUMN email_canonical TEXT;
UPDATE accounts SET email_canonical = lower(email) WHERE email_canonical IS NULL;
ALTER TABLE accounts ADD CONSTRAINT accounts_email_canonical_key UNIQUE (email_canonical);

-- Self-hosted: the first account becomes the owner.
ALTER TABLE accounts ADD COLUMN is_owner BOOLEAN NOT NULL DEFAULT FALSE;

-- Instance-wide settings the owner can change (`signups_open`).
CREATE TABLE instance_settings (
  key TEXT PRIMARY KEY,
  value JSONB NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX crawls_site_manual_idx ON crawls(site_id, created_at) WHERE trigger = 'manual';
CREATE INDEX sessions_account_id_idx ON sessions(account_id);
