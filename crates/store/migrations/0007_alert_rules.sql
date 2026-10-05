-- M7 alert rules: one rule per (site, change kind, channel), and the lookup the alert planner
-- uses to read a crawl's changes.
--
-- `change_kind` stays nullable for old rows ("any kind"); new rows must name a kind.
CREATE UNIQUE INDEX alert_rules_site_kind_channel_idx
  ON alert_rules (site_id, change_kind, channel_id);

ALTER TABLE alert_rules
  ADD CONSTRAINT alert_rules_kind_named CHECK (change_kind IS NOT NULL) NOT VALID;

CREATE INDEX IF NOT EXISTS changes_crawl_id_idx ON changes (crawl_id);
