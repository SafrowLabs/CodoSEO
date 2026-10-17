-- Each finished crawl's whole internal link graph, for the site graph views: every page's URL
-- hash, crawl-tree parent and link score, and every link between crawled pages with its anchor
-- text, in one gzipped binary body (`codoseo_store::graph`). `pages` and `edges` count what the
-- body holds, so a view can tell how big a graph is without reading it. Kept for the same 2
-- crawls as `pages`: `finalize` drops the older ones.
CREATE TABLE crawl_graphs (
  crawl_id UUID PRIMARY KEY REFERENCES crawls(id) ON DELETE CASCADE,
  pages INTEGER NOT NULL,
  edges INTEGER NOT NULL,
  body BYTEA NOT NULL
);
