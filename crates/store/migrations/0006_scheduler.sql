-- M7 scheduler: inactivity and digest bookkeeping on accounts, the resume-monitoring token
-- purpose, and the index the minute-by-minute "what is due" query reads.
--
-- sqlx runs a migration in one transaction. `ADD VALUE` is allowed inside one (Postgres 12+),
-- but the new label can't be used until it commits, so nothing below refers to
-- 'resume_monitoring'.
ALTER TABLE accounts
  ADD COLUMN keep_monitoring_sent_at TIMESTAMPTZ,
  ADD COLUMN last_digest_at TIMESTAMPTZ;

ALTER TYPE login_token_purpose ADD VALUE 'resume_monitoring';

CREATE INDEX sites_due_idx ON sites (next_crawl_at)
  WHERE monitoring_active AND account_id IS NOT NULL AND schedule IS NOT NULL;
