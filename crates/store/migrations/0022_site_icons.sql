-- Each site's icon as its newest finished crawl found it: a small raster image (at most 64 KiB,
-- typed by its own bytes), served from our own origin so the app never loads from the audited
-- site. A crawl that finds none removes it, and the app shows the site's initial instead.
CREATE TABLE site_icons (
  site_id UUID PRIMARY KEY REFERENCES sites(id) ON DELETE CASCADE,
  content_type TEXT NOT NULL,
  bytes BYTEA NOT NULL CHECK (octet_length(bytes) <= 65536),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
