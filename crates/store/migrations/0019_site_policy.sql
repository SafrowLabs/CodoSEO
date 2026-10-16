-- Site policy: how the owner means the site to work for search engines and AI bots
-- (`codoseo_policy::SitePolicy` as JSON).
--
-- The AI part takes over `sites.ai_intent` and `sites.ai_probes`: every site's policy starts as
-- its intent plus the probes switch. Those two columns stay for one release as a write-through
-- mirror (every policy save writes them too), so a rollback to the previous release reads the
-- same intent. NULL reads as the defaults (as the mirror columns do).
ALTER TABLE sites ADD COLUMN policy JSONB;

UPDATE sites SET policy = jsonb_build_object(
  'version', 1,
  'seo', '{}'::jsonb,
  'ai', CASE WHEN jsonb_typeof(ai_intent) = 'object' THEN ai_intent ELSE '{}'::jsonb END
        || jsonb_build_object('probes', ai_probes)
);

-- Every change of a site's policy: who made it and where, what changed, and the scores of the
-- newest finished crawl before and after (a save re-evaluates it), so a score jump can be traced
-- to a policy edit. Trimmed with the plan's history window.
CREATE TABLE policy_history (
  id BIGSERIAL PRIMARY KEY,
  site_id UUID NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
  account_id UUID REFERENCES accounts(id) ON DELETE SET NULL,
  via TEXT NOT NULL CHECK (via IN ('web', 'api', 'mcp', 'import', 'migration', 'mark_intended')),
  api_key_id UUID REFERENCES api_keys(id) ON DELETE SET NULL,
  before JSONB,
  after JSONB NOT NULL,
  -- The changed keys: `ai.purposes.training: default → block`.
  summary TEXT NOT NULL,
  -- The crawl the save re-evaluated (the newest finished one), if any.
  crawl_id UUID REFERENCES crawls(id) ON DELETE SET NULL,
  health_before SMALLINT,
  health_after SMALLINT,
  geo_before SMALLINT,
  geo_after SMALLINT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX policy_history_site_idx ON policy_history (site_id, created_at DESC);

-- Where each customised policy came from.
INSERT INTO policy_history (site_id, account_id, via, before, after, summary)
SELECT id, account_id, 'migration', NULL, policy, 'Moved from the AI access intent'
FROM sites
WHERE ai_intent <> '{}'::jsonb OR NOT ai_probes;
