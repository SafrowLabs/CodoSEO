-- The pages of a running crawl as the live views list them: one row per recorded page with what
-- its own checks found and which AI products can use it (`LiveRow`, as JSON). The worker adds
-- them on its throttled heartbeat. A site keeps only its newest crawl's rows: `finalize` drops
-- the others, and a retried crawl starts its own over.
CREATE TABLE crawl_live_rows (
  crawl_id UUID NOT NULL REFERENCES crawls(id) ON DELETE CASCADE,
  n INTEGER NOT NULL,
  row JSONB NOT NULL,
  PRIMARY KEY (crawl_id, n)
);
