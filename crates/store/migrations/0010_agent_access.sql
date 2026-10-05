-- M8 cloud agent access: named API keys for the REST API and the cloud MCP server, and the
-- per-account daily call counter their quota reads.
--
-- `api_keys` exists since 0001 and nothing wrote to it yet. The key itself is only ever stored
-- as a SHA-256 hash; `prefix` is the first 12 characters of the key (`cdo_Ab3dE5gH`), kept so the
-- settings screen can tell a person's keys apart.
ALTER TABLE api_keys ADD COLUMN prefix TEXT NOT NULL DEFAULT '';
ALTER TABLE api_keys ALTER COLUMN prefix DROP DEFAULT;

-- An account's live keys, newest first: the settings list and the 20-key cap. Revoked keys drop
-- out of the index and out of the list.
CREATE INDEX api_keys_account_idx ON api_keys (account_id, created_at DESC)
  WHERE revoked_at IS NULL;

-- API calls per account per UTC day. One row a day, bumped by a single upsert that only
-- increments below the plan's limit (`api_keys::charge`). Rows older than 35 days are deleted
-- by the daily cleanup.
CREATE TABLE api_usage (
  account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  day DATE NOT NULL,
  calls INT NOT NULL,
  PRIMARY KEY (account_id, day)
);
