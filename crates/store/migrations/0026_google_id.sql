-- "Continue with Google": the Google account's stable `sub` id, like `github_id`.
ALTER TABLE accounts ADD COLUMN google_id TEXT UNIQUE;
