-- enums
CREATE TYPE plan AS ENUM ('free', 'pro', 'agency', 'self_hosted');
CREATE TYPE crawl_trigger AS ENUM ('schedule', 'manual', 'first', 'quick');
CREATE TYPE crawl_status AS ENUM ('queued', 'running', 'done', 'failed');
CREATE TYPE indexability AS ENUM ('indexable', 'noindex', 'canonicalised', 'redirected', 'client_error', 'server_error', 'blocked_by_robots');
CREATE TYPE severity AS ENUM ('critical', 'warning', 'notice');
CREATE TYPE change_kind AS ENUM ('new_url', 'removed_url', 'status_changed', 'became_noindex', 'title_changed', 'title_removed', 'canonical_changed', 'redirect_chain_grew', 'robots_txt_changed', 'sitemap_shrank', 'error_spike', 'site_moved');
CREATE TYPE job_kind AS ENUM ('send_alert', 'send_digest', 'send_email', 'cleanup');
CREATE TYPE job_status AS ENUM ('queued', 'running', 'done', 'failed');
CREATE TYPE login_token_purpose AS ENUM ('magic_link', 'start_monitoring');
CREATE TYPE alert_channel_kind AS ENUM ('email', 'slack', 'discord', 'webhook');
CREATE TYPE alert_mode AS ENUM ('instant', 'digest');

CREATE TABLE accounts (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  email TEXT NOT NULL UNIQUE,
  github_id TEXT UNIQUE,
  plan plan NOT NULL DEFAULT 'free',
  plan_expires_at TIMESTAMPTZ,
  timezone TEXT NOT NULL DEFAULT 'UTC',
  last_login_at TIMESTAMPTZ,
  last_email_click_at TIMESTAMPTZ,
  paused BOOLEAN NOT NULL DEFAULT FALSE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE login_tokens (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  account_id UUID REFERENCES accounts(id) ON DELETE CASCADE,
  purpose login_token_purpose NOT NULL,
  token_hash BYTEA NOT NULL UNIQUE,
  payload JSONB,
  expires_at TIMESTAMPTZ NOT NULL,
  used_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE sessions (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  session_hash BYTEA NOT NULL UNIQUE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at TIMESTAMPTZ NOT NULL,
  last_seen_at TIMESTAMPTZ
);

CREATE TABLE api_keys (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  key_hash BYTEA NOT NULL UNIQUE,
  last_used_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  revoked_at TIMESTAMPTZ
);

CREATE TABLE sites (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  account_id UUID REFERENCES accounts(id) ON DELETE CASCADE,
  domain TEXT NOT NULL,
  start_url TEXT NOT NULL,
  schedule TEXT, -- 'weekly' | 'daily' | NULL, matches core::plan::Schedule
  scheduled_hour SMALLINT,
  next_crawl_at TIMESTAMPTZ,
  crawl_settings JSONB NOT NULL DEFAULT '{}',
  monitoring_active BOOLEAN NOT NULL DEFAULT TRUE,
  key_pages BIGINT[] NOT NULL DEFAULT '{}', -- url_hash values, cast via hash::to_db
  claim_token_hash BYTEA UNIQUE, -- set only for unclaimed no-signup audits
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX sites_account_id_idx ON sites(account_id);

CREATE TABLE crawls (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  site_id UUID NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
  domain TEXT NOT NULL, -- denormalised from sites.domain for the partial unique index below
  status crawl_status NOT NULL DEFAULT 'queued',
  trigger crawl_trigger NOT NULL,
  priority SMALLINT NOT NULL, -- base lane, spec section 10 table: 0 (quick) .. 5 (free scheduled)
  source TEXT, -- 'web' | 'agent', quick audits only
  requester_ip_hash BYTEA,
  queued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  started_at TIMESTAMPTZ,
  finished_at TIMESTAMPTZ,
  worker_id TEXT,
  heartbeat_at TIMESTAMPTZ,
  progress JSONB, -- codoseo_core::output::Progress
  health_score SMALLINT,
  checks_passed SMALLINT,
  checks_total SMALLINT,
  summary JSONB, -- codoseo_core::report::CrawlReport minus inlink_samples
  failure_reason TEXT,
  attempt SMALLINT NOT NULL DEFAULT 0,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX crawls_one_running_per_domain ON crawls(domain) WHERE status = 'running';
CREATE INDEX crawls_queue_idx ON crawls(priority, queued_at) WHERE status = 'queued';
CREATE INDEX crawls_site_id_idx ON crawls(site_id, finished_at DESC);

CREATE TABLE pages (
  id BIGSERIAL PRIMARY KEY,
  crawl_id UUID NOT NULL REFERENCES crawls(id) ON DELETE CASCADE,
  site_id UUID NOT NULL,
  url TEXT NOT NULL,
  url_hash BIGINT NOT NULL,
  status SMALLINT NOT NULL,
  redirect_chain JSONB NOT NULL DEFAULT '[]',
  response_ms INTEGER,
  size_bytes BIGINT,
  content_type TEXT,
  depth INTEGER,
  in_sitemap BOOLEAN NOT NULL DEFAULT FALSE,
  indexability indexability NOT NULL,
  title TEXT,
  meta_description TEXT,
  meta_robots TEXT,
  x_robots_tag TEXT,
  canonical TEXT,
  hreflang JSONB NOT NULL DEFAULT '[]',
  h1 JSONB NOT NULL DEFAULT '[]',
  h2 JSONB NOT NULL DEFAULT '[]',
  word_count INTEGER,
  content_hash BIGINT,
  images_missing_alt INTEGER,
  og JSONB,
  jsonld_status TEXT,
  mixed_content INTEGER,
  inlinks INTEGER NOT NULL DEFAULT 0,
  outlinks_internal INTEGER NOT NULL DEFAULT 0,
  outlinks_external INTEGER NOT NULL DEFAULT 0,
  issues BIGINT NOT NULL DEFAULT 0,
  key_hash BIGINT
);
CREATE INDEX pages_crawl_id_idx ON pages(crawl_id);
CREATE INDEX pages_site_url_hash_idx ON pages(site_id, url_hash);

CREATE TABLE inlinks (
  id BIGSERIAL PRIMARY KEY,
  crawl_id UUID NOT NULL REFERENCES crawls(id) ON DELETE CASCADE,
  target_url_hash BIGINT NOT NULL,
  from_url TEXT NOT NULL,
  anchor_text TEXT,
  nofollow BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE INDEX inlinks_crawl_target_idx ON inlinks(crawl_id, target_url_hash);

CREATE TABLE site_files (
  id BIGSERIAL PRIMARY KEY,
  crawl_id UUID NOT NULL REFERENCES crawls(id) ON DELETE CASCADE,
  site_id UUID NOT NULL,
  robots_status SMALLINT,
  robots_body TEXT,
  robots_hash BIGINT,
  sitemap_url_count INTEGER,
  sitemap_hash BIGINT
);
CREATE INDEX site_files_site_id_idx ON site_files(site_id, crawl_id);

CREATE TABLE changes (
  id BIGSERIAL PRIMARY KEY,
  crawl_id UUID NOT NULL REFERENCES crawls(id) ON DELETE CASCADE,
  site_id UUID NOT NULL,
  kind change_kind NOT NULL,
  severity severity NOT NULL,
  url TEXT,
  before_value TEXT NOT NULL,
  after_value TEXT NOT NULL,
  alerted_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX changes_site_id_idx ON changes(site_id, created_at DESC);

CREATE TABLE alert_channels (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  kind alert_channel_kind NOT NULL,
  target_encrypted BYTEA NOT NULL,
  enabled BOOLEAN NOT NULL DEFAULT TRUE,
  last_failure_at TIMESTAMPTZ,
  consecutive_failures SMALLINT NOT NULL DEFAULT 0,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE alert_rules (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  site_id UUID NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
  change_kind change_kind, -- NULL = any kind
  threshold JSONB,
  channel_id UUID NOT NULL REFERENCES alert_channels(id) ON DELETE CASCADE,
  mode alert_mode NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE jobs (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  kind job_kind NOT NULL,
  payload JSONB NOT NULL,
  status job_status NOT NULL DEFAULT 'queued',
  attempt SMALLINT NOT NULL DEFAULT 0,
  max_attempts SMALLINT NOT NULL DEFAULT 5,
  run_after TIMESTAMPTZ NOT NULL DEFAULT now(),
  claimed_by TEXT,
  claimed_at TIMESTAMPTZ,
  completed_at TIMESTAMPTZ,
  last_error TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX jobs_queue_idx ON jobs(run_after) WHERE status = 'queued';

CREATE TABLE events (
  id BIGSERIAL PRIMARY KEY,
  account_id UUID REFERENCES accounts(id) ON DELETE SET NULL,
  site_id UUID REFERENCES sites(id) ON DELETE SET NULL,
  kind TEXT NOT NULL,
  payload JSONB,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX events_kind_created_idx ON events(kind, created_at);
