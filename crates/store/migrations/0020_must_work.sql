-- Site policy, SEO rules: a page the policy says must always work (a must-work path, a starred
-- page or the home page) that failed on a crawl. Instant by default (`DEFAULT_INSTANT`); existing
-- sites get the default rule the next time an alert is planned for them, like every default.
--
-- As in 0011-0013, sqlx runs the migration in one transaction and `ADD VALUE` is allowed there
-- because nothing here uses the new value before the commit.
ALTER TYPE change_kind ADD VALUE 'must_work_failed';
