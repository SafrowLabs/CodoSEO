-- Running totals for the public "pages crawled" counter, so the landing page never counts pages.
-- `finalize` bumps the one row in the same transaction that marks a crawl done, monitored crawls
-- and no-signup audits alike; per-page rows are trimmed after two crawls, so they could not be
-- counted anyway.
CREATE TABLE crawl_stats (
  id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
  pages_crawled BIGINT NOT NULL DEFAULT 0,
  crawls BIGINT NOT NULL DEFAULT 0,
  -- Distinct websites (by domain) ever crawled. Kept, not shown publicly yet.
  websites BIGINT NOT NULL DEFAULT 0,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Every website ever crawled, so `websites` counts each domain once. Only the domain's SHA-256 is
-- kept: unclaimed audits and deleted sites take their domain with them, the count stays.
CREATE TABLE crawled_websites (
  domain_hash BYTEA PRIMARY KEY,
  first_crawled_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Start from what is still in the database: every finished crawl's page count from its stored
-- summary, and the domains of those crawls. Crawls already removed by the retention cleanup
-- (unclaimed audits after 7 days, history past a plan's window) are not counted.
INSERT INTO crawled_websites (domain_hash, first_crawled_at)
SELECT sha256(convert_to(domain, 'UTF8')), min(coalesce(finished_at, created_at))
FROM crawls WHERE status = 'done'
GROUP BY domain;

INSERT INTO crawl_stats (pages_crawled, crawls, websites)
SELECT coalesce(sum((summary -> 'report_summary' ->> 'pages')::bigint), 0),
       count(*),
       (SELECT count(*) FROM crawled_websites)
FROM crawls WHERE status = 'done';
