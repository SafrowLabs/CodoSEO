-- The GEO score of a crawl and the Overall score that blends it with the SEO health score
-- (`codoseo_geo::score`), with the GEO check counts the audit page shows ("3 of 11 checks pass").
-- Written by `finalize` with the crawl's AI access report, and again by an intent change for the
-- newest finished crawl. NULL for crawls without an AI access evaluation (no-signup audits) and for
-- every crawl finished before this migration: nothing is backfilled. Nullable columns without a
-- default, so adding them rewrites nothing on a large table.
ALTER TABLE crawls
  ADD COLUMN geo_score SMALLINT,
  ADD COLUMN overall_score SMALLINT,
  ADD COLUMN geo_checks_passed SMALLINT,
  ADD COLUMN geo_checks_applicable SMALLINT;
