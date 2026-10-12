-- GEO Phase B2: content visible without JavaScript, and llms.txt.
--
-- Two more change kinds: a page whose content is missing from the server-rendered HTML (what AI
-- crawlers read, since they do not run JavaScript) and an llms.txt the site adopted that no longer
-- serves. As in 0011 and 0012, sqlx runs the migration in one transaction and `ADD VALUE` is
-- allowed there because nothing here uses the new values before the commit.
ALTER TYPE change_kind ADD VALUE 'ai_content_missing';
ALTER TYPE change_kind ADD VALUE 'ai_llms_txt_unavailable';

COMMENT ON COLUMN ai_incidents.kind IS
  'robots_unavailable | bots_blocked | bots_not_blocked | answers_restricted | fetch_blocked | '
  'content_missing | llms_txt_unavailable';
COMMENT ON COLUMN ai_incidents.subject IS
  'The purpose slug for bot findings, the directive slug for answer restrictions, the cause slug '
  '(challenge_cloudflare, blocked_403, diverted, ...) for fetch_blocked, and empty otherwise '
  '(content_missing and llms_txt_unavailable have no subject).';
