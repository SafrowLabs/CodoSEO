-- New change kinds: a page other pages link to that stopped answering (`broken_link`, instant by
-- default: existing sites get the rule the next time an alert is planned for them, like every
-- default), a reworded meta description or H1, and a page newly listed in the sitemap (all three
-- wait for the digest).
--
-- As in 0020, sqlx runs the migration in one transaction and `ADD VALUE` is allowed there
-- because nothing here uses the new values before the commit.
ALTER TYPE change_kind ADD VALUE 'broken_link';
ALTER TYPE change_kind ADD VALUE 'description_changed';
ALTER TYPE change_kind ADD VALUE 'h1_changed';
ALTER TYPE change_kind ADD VALUE 'added_to_sitemap';
