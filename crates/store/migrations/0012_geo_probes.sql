-- GEO Phase B1: synthetic AI agent fetch probes.
--
-- After each monitored crawl a few important pages are fetched with the documented user agents
-- of the AI search and user-fetch bots and compared with CodoSEObot's own fetch of the same page.
-- The results live in `ai_reports.report` (`AccessReport.probes`), so no new table is needed.
--
-- One change kind for a site that blocks, challenges, diverts or degrades those fetches. As in
-- 0011, sqlx runs the migration in one transaction and `ADD VALUE` is allowed there because
-- nothing here uses the new value before the commit.
ALTER TYPE change_kind ADD VALUE 'ai_fetch_blocked';

-- Synthetic agent fetch probes after each monitored crawl; the owner can switch them off.
ALTER TABLE sites ADD COLUMN ai_probes BOOLEAN NOT NULL DEFAULT TRUE;

COMMENT ON COLUMN ai_incidents.kind IS
  'robots_unavailable | bots_blocked | bots_not_blocked | answers_restricted | fetch_blocked';
COMMENT ON COLUMN ai_incidents.subject IS
  'The purpose slug for bot findings, the directive slug for answer restrictions, the cause slug '
  '(challenge_cloudflare, blocked_403, diverted, ...) for fetch_blocked, and empty otherwise.';
