-- Each crawl's sitemaps as a tree (a `SitemapTree`): every file discovery tried, in fetch order,
-- with its parent index, lastmod stats and how its URLs fared in the crawl, for the site graph.
-- NULL for crawls finished before it was kept.
ALTER TABLE site_files ADD COLUMN sitemap_tree JSONB;
