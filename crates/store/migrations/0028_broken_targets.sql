-- What the checks after a crawl found gone: links to other sites, images, scripts and
-- stylesheets that answered 404 or 410 (or any error on the site's own host), one row per page
-- that uses them. The page bits (`links_to_broken_external`, `broken_images`,
-- `broken_resources`) say which pages are affected; these rows say by what.
CREATE TABLE broken_targets (
    crawl_id      uuid     NOT NULL REFERENCES crawls (id) ON DELETE CASCADE,
    from_url_hash bigint   NOT NULL,
    -- `link`, `image`, `script` or `stylesheet`.
    kind          text     NOT NULL,
    -- What the address answered; 0 when the host does not exist or refused the connection.
    status        smallint NOT NULL,
    target_url    text     NOT NULL
);

CREATE INDEX broken_targets_page_idx ON broken_targets (crawl_id, from_url_hash);
