-- GEO Phase A: what answer engines are told, per page and per site.
--
-- `pages.ai_meta` holds the page's `AiMeta` (bot-named robots metas, the share of text under
-- `data-nosnippet`, the TDM metas) as JSON. NULL for pages with none of that markup, which is
-- nearly all of them, and for every crawl stored before this migration.
ALTER TABLE pages ADD COLUMN ai_meta JSONB;

-- GEO Phase A: AI access incidents.
--
-- Five change kinds for what a crawl finds about AI crawlers and AI answers. sqlx runs a
-- migration in one transaction; `ADD VALUE` is allowed inside one (Postgres 12+) as long as the
-- new value is not used before the commit, and nothing here uses them.
ALTER TYPE change_kind ADD VALUE 'ai_bot_blocked';
ALTER TYPE change_kind ADD VALUE 'ai_answers_restricted';
ALTER TYPE change_kind ADD VALUE 'ai_block_not_applied';
ALTER TYPE change_kind ADD VALUE 'ai_issue_resolved';
ALTER TYPE change_kind ADD VALUE 'ai_preferences_changed';

-- What the owner wants from each kind of AI bot (`codoseo_geo::Intent` as JSON). '{}' is the
-- defaults: search and user-triggered fetches wanted, everything else no preference.
ALTER TABLE sites ADD COLUMN ai_intent JSONB NOT NULL DEFAULT '{}';

-- The intent-independent facts of one crawl (`codoseo_geo::report::AccessReport` as JSON). A
-- failed crawl that saw a failing robots.txt has one too; the newest two per site are kept.
CREATE TABLE ai_reports (
  crawl_id UUID PRIMARY KEY REFERENCES crawls(id) ON DELETE CASCADE,
  site_id UUID NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
  report JSONB NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX ai_reports_site_idx ON ai_reports (site_id, created_at DESC);

-- One row per problem, kept from the crawl that first saw it to the one that no longer does
-- (or until the owner's intent makes it moot). `subject` is the purpose slug for bot findings,
-- the directive slug for answer restrictions, and empty otherwise.
CREATE TABLE ai_incidents (
  id BIGSERIAL PRIMARY KEY,
  site_id UUID NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
  kind TEXT NOT NULL, -- 'robots_unavailable' | 'bots_blocked' | 'bots_not_blocked' | 'answers_restricted'
  subject TEXT NOT NULL DEFAULT '',
  severity severity NOT NULL,
  title TEXT NOT NULL,
  summary TEXT NOT NULL,
  evidence JSONB NOT NULL,
  opened_crawl_id UUID,
  last_seen_crawl_id UUID,
  resolved_crawl_id UUID,
  opened_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  resolved_at TIMESTAMPTZ,
  resolution TEXT, -- 'fixed' (a crawl no longer finds it) | 'intent' (the owner's intent changed)
  quiet BOOLEAN NOT NULL DEFAULT FALSE -- opened without an alert: the first report of a site, or an intent change
);
-- At most one open incident per problem.
CREATE UNIQUE INDEX ai_incidents_open_idx ON ai_incidents (site_id, kind, subject) WHERE resolved_at IS NULL;
CREATE INDEX ai_incidents_site_idx ON ai_incidents (site_id, resolved_at);
