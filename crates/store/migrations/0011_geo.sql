-- GEO Phase A: what answer engines are told, per page and per site.
--
-- `pages.ai_meta` holds the page's `AiMeta` (bot-named robots metas, the share of text under
-- `data-nosnippet`, the TDM metas) as JSON. NULL for pages with none of that markup, which is
-- nearly all of them, and for every crawl stored before this migration.
ALTER TABLE pages ADD COLUMN ai_meta JSONB;
