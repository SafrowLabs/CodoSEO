-- M7 notification channels: a display name, the last delivery error, a per-channel mute, and
-- the default email channel (one per account, targeting the account's own address).
ALTER TABLE alert_channels
  ADD COLUMN name TEXT,
  ADD COLUMN last_error TEXT,
  ADD COLUMN muted BOOLEAN NOT NULL DEFAULT FALSE,
  ADD COLUMN is_default BOOLEAN NOT NULL DEFAULT FALSE;

CREATE UNIQUE INDEX alert_channels_default_idx ON alert_channels (account_id) WHERE is_default;
CREATE INDEX alert_channels_account_idx ON alert_channels (account_id, created_at);
