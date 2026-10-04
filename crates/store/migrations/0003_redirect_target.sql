-- Where a redirecting URL finally lands. `redirect_chain` records each hop's status and the URL
-- that answered it, but not the destination, which the explorer, the CSV export and the change
-- detector all need.
ALTER TABLE pages ADD COLUMN redirect_target TEXT;
