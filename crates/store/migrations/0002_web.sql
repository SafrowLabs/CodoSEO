-- M5: the web app. Login deduplication, the self-hosted owner, instance settings, and an index
-- for the manual-crawl allowance check.

-- The deduplication key for an email address (lowercased, `+tag` dropped, Gmail dots removed).
-- `email` keeps the address as typed, for sending. Every account the web app creates sets it;
-- it stays nullable so rows written by older code paths (and tests) remain valid.
ALTER TABLE accounts ADD COLUMN email_canonical TEXT;
ALTER TABLE accounts ADD CONSTRAINT accounts_email_canonical_key UNIQUE (email_canonical);

-- Backfill with the same rules as the web crate's `auth::email::canonical`. When several
-- existing accounts share a key, only the oldest gets it; sign-in finds the others by email.
WITH parts AS (
  SELECT id, created_at,
         split_part(split_part(lower(email), '@', 1), '+', 1) AS local,
         split_part(lower(email), '@', 2) AS domain
  FROM accounts
), keyed AS (
  SELECT id, created_at,
         CASE WHEN domain IN ('gmail.com', 'googlemail.com')
              THEN replace(local, '.', '') || '@gmail.com'
              ELSE local || '@' || domain END AS canonical
  FROM parts
), ranked AS (
  SELECT id, canonical,
         row_number() OVER (PARTITION BY canonical ORDER BY created_at, id) AS n
  FROM keyed
)
UPDATE accounts a SET email_canonical = r.canonical
FROM ranked r
WHERE a.id = r.id AND r.n = 1;

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
