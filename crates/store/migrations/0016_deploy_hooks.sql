-- Deploy hooks: a per-site secret URL a deploy platform calls to queue a crawl (and so an AI access
-- recheck) of the site. Only the secret's SHA-256 is kept; a site has at most one, and making a new
-- one replaces it. Deleting the site deletes its hook.
CREATE TABLE deploy_hooks (
  site_id UUID PRIMARY KEY REFERENCES sites(id) ON DELETE CASCADE,
  token_hash BYTEA NOT NULL UNIQUE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_used_at TIMESTAMPTZ
);
