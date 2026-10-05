//! Notification channels: where an account's alerts go (its email address, Slack, Discord,
//! signed webhooks). The target (address, URL, secret) is stored encrypted with the channel key
//! and decrypted only to deliver or to show the host.

use codoseo_notify::{ChannelKey, ChannelKind, ChannelTarget, CryptoError, TargetError};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

/// How much of an error message `last_error` keeps.
const ERROR_MAX: usize = 500;

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error("the stored target could not be read: {0}")]
    Decrypt(#[from] CryptoError),
    #[error("the stored target is damaged: {0}")]
    Target(#[from] TargetError),
    #[error("unknown channel kind {0:?}")]
    UnknownKind(String),
}

/// A channel as the settings page shows it. `target` is display-safe: the address for email, the
/// host only for URL channels, never a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelSummary {
    pub id: Uuid,
    pub kind: ChannelKind,
    pub name: Option<String>,
    pub target: String,
    pub enabled: bool,
    pub muted: bool,
    pub is_default: bool,
    pub last_error: Option<String>,
    pub last_failure_at: Option<OffsetDateTime>,
    pub consecutive_failures: i16,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOutcome {
    Deleted,
    NotFound,
    /// The default email channel is only ever muted.
    DefaultChannel,
}

/// What the delivery job needs to know about a channel before sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelState {
    pub account_id: Uuid,
    pub kind: ChannelKind,
    pub enabled: bool,
    pub muted: bool,
    /// The account's own email channel.
    pub is_default: bool,
}

#[derive(FromRow)]
struct Row {
    id: Uuid,
    kind: String,
    name: Option<String>,
    target_encrypted: Vec<u8>,
    enabled: bool,
    muted: bool,
    is_default: bool,
    last_error: Option<String>,
    last_failure_at: Option<OffsetDateTime>,
    consecutive_failures: i16,
    created_at: OffsetDateTime,
}

fn decrypt_target(
    key: &ChannelKey,
    kind: ChannelKind,
    bytes: &[u8],
) -> Result<ChannelTarget, ChannelError> {
    let plain = key.decrypt(bytes)?;
    let json: serde_json::Value = serde_json::from_slice(&plain)
        .map_err(|e| TargetError::Invalid(format!("not JSON: {e}")))?;
    Ok(ChannelTarget::from_json(kind, &json)?)
}

fn kind_of(slug: &str) -> Result<ChannelKind, ChannelError> {
    ChannelKind::parse(slug).ok_or_else(|| ChannelError::UnknownKind(slug.to_owned()))
}

fn short_error(error: &str) -> String {
    error.chars().take(ERROR_MAX).collect()
}

/// Saves a channel. `is_default` marks the account's own-address email channel (at most one per
/// account; a second is an error). Validate URL targets with
/// [`codoseo_notify::GuardedHttp::validate_target`] first.
pub async fn create(
    pool: &PgPool,
    key: &ChannelKey,
    account_id: Uuid,
    target: &ChannelTarget,
    name: Option<&str>,
    is_default: bool,
) -> Result<Uuid, ChannelError> {
    let sealed = key.encrypt(target.to_json().to_string().as_bytes());
    let id = sqlx::query_scalar(
        "INSERT INTO alert_channels (account_id, kind, target_encrypted, name, is_default) \
         VALUES ($1, $2::alert_channel_kind, $3, $4, $5) RETURNING id",
    )
    .bind(account_id)
    .bind(target.kind().as_str())
    .bind(sealed)
    .bind(name)
    .bind(is_default)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// The account's channels, default first. A target that can't be decrypted (wrong `SECRET_KEY`)
/// still lists, with a placeholder instead of the target.
pub async fn list_for_account(
    pool: &PgPool,
    key: &ChannelKey,
    account_id: Uuid,
) -> Result<Vec<ChannelSummary>, ChannelError> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, kind::text AS kind, name, target_encrypted, enabled, muted, is_default, \
                last_error, last_failure_at, consecutive_failures, created_at \
         FROM alert_channels WHERE account_id = $1 \
         ORDER BY is_default DESC, created_at, id",
    )
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|r| {
            let kind = kind_of(&r.kind)?;
            let target = decrypt_target(key, kind, &r.target_encrypted)
                .map_or_else(|_| "(unreadable)".to_owned(), |t| t.display());
            Ok(ChannelSummary {
                id: r.id,
                kind,
                name: r.name,
                target,
                enabled: r.enabled,
                muted: r.muted,
                is_default: r.is_default,
                last_error: r.last_error,
                last_failure_at: r.last_failure_at,
                consecutive_failures: r.consecutive_failures,
                created_at: r.created_at,
            })
        })
        .collect()
}

/// The decrypted target, to deliver to. `None` when the channel doesn't exist.
pub async fn get_target(
    pool: &PgPool,
    key: &ChannelKey,
    id: Uuid,
) -> Result<Option<ChannelTarget>, ChannelError> {
    let row: Option<(String, Vec<u8>)> =
        sqlx::query_as("SELECT kind::text, target_encrypted FROM alert_channels WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    row.map(|(kind, bytes)| decrypt_target(key, kind_of(&kind)?, &bytes))
        .transpose()
}

/// Deletes one of the account's channels; the default email channel is refused.
pub async fn delete(
    pool: &PgPool,
    account_id: Uuid,
    id: Uuid,
) -> Result<DeleteOutcome, sqlx::Error> {
    let is_default: Option<bool> = sqlx::query_scalar(
        "SELECT is_default FROM alert_channels WHERE id = $1 AND account_id = $2",
    )
    .bind(id)
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    match is_default {
        None => Ok(DeleteOutcome::NotFound),
        Some(true) => Ok(DeleteOutcome::DefaultChannel),
        Some(false) => {
            let n = sqlx::query(
                "DELETE FROM alert_channels WHERE id = $1 AND account_id = $2 AND NOT is_default",
            )
            .bind(id)
            .bind(account_id)
            .execute(pool)
            .await?
            .rows_affected();
            Ok(if n == 1 {
                DeleteOutcome::Deleted
            } else {
                DeleteOutcome::NotFound
            })
        }
    }
}

/// Mutes or unmutes one of the account's channels; false when it isn't theirs.
pub async fn set_muted(
    pool: &PgPool,
    account_id: Uuid,
    id: Uuid,
    muted: bool,
) -> Result<bool, sqlx::Error> {
    let n = sqlx::query("UPDATE alert_channels SET muted = $3 WHERE id = $1 AND account_id = $2")
        .bind(id)
        .bind(account_id)
        .bind(muted)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(n == 1)
}

/// One channel's owner, kind and switches; `None` when it doesn't exist (deleted since the
/// delivery job was planned).
pub async fn state(pool: &PgPool, id: Uuid) -> Result<Option<ChannelState>, ChannelError> {
    let row: Option<(Uuid, String, bool, bool, bool)> = sqlx::query_as(
        "SELECT account_id, kind::text, enabled, muted, is_default FROM alert_channels WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map(|(account_id, kind, enabled, muted, is_default)| {
        Ok(ChannelState {
            account_id,
            kind: kind_of(&kind)?,
            enabled,
            muted,
            is_default,
        })
    })
    .transpose()
}

/// The account's channels alerts may go to right now: enabled and not muted. Each with its kind.
pub async fn deliverable(
    pool: &PgPool,
    account_id: Uuid,
) -> Result<Vec<(Uuid, ChannelKind)>, ChannelError> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, kind::text FROM alert_channels \
         WHERE account_id = $1 AND enabled AND NOT muted ORDER BY is_default DESC, created_at, id",
    )
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(id, kind)| Ok((id, kind_of(&kind)?)))
        .collect()
}

/// The account's default email channel (its own address), created on first use. Safe to call
/// from several places at once: there is exactly one per account.
pub async fn ensure_default_email(
    pool: &PgPool,
    key: &ChannelKey,
    account_id: Uuid,
) -> Result<Uuid, ChannelError> {
    const FIND: &str = "SELECT id FROM alert_channels WHERE account_id = $1 AND is_default";
    if let Some(id) = sqlx::query_scalar(FIND)
        .bind(account_id)
        .fetch_optional(pool)
        .await?
    {
        return Ok(id);
    }
    let email: String = sqlx::query_scalar("SELECT email FROM accounts WHERE id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await?;
    let sealed = key.encrypt(
        ChannelTarget::Email { to: email }
            .to_json()
            .to_string()
            .as_bytes(),
    );
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO alert_channels (account_id, kind, target_encrypted, is_default) \
         VALUES ($1, 'email', $2, TRUE) \
         ON CONFLICT (account_id) WHERE is_default DO NOTHING RETURNING id",
    )
    .bind(account_id)
    .bind(sealed)
    .fetch_optional(pool)
    .await?;
    match inserted {
        Some(id) => Ok(id),
        // Another caller created it between the lookup and the insert.
        None => Ok(sqlx::query_scalar(FIND)
            .bind(account_id)
            .fetch_one(pool)
            .await?),
    }
}

/// A delivery succeeded: the failure streak and the last error are cleared.
pub async fn record_success(pool: &PgPool, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE alert_channels SET consecutive_failures = 0, last_error = NULL WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// A delivery failed: one more in the streak, with the error and when.
pub async fn record_failure(pool: &PgPool, id: Uuid, error: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE alert_channels SET consecutive_failures = LEAST(consecutive_failures + 1, 32000)::smallint, \
                last_error = $2, last_failure_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(short_error(error))
    .execute(pool)
    .await?;
    Ok(())
}

/// Switches a channel the account owns back on after it was turned off, and forgets the failures
/// and the last error. `false` when it isn't theirs.
pub async fn reenable(pool: &PgPool, account_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
    let n = sqlx::query(
        "UPDATE alert_channels SET enabled = TRUE, consecutive_failures = 0, last_error = NULL \
         WHERE id = $1 AND account_id = $2",
    )
    .bind(id)
    .bind(account_id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n == 1)
}

/// Switches the channel off after delivery kept failing; `last_error` says why. Returns whether
/// this call did it (`false` when it was already off), so the account is told once.
pub async fn disable(pool: &PgPool, id: Uuid, error: &str) -> Result<bool, sqlx::Error> {
    let n = sqlx::query(
        "UPDATE alert_channels SET enabled = FALSE, last_error = $2, last_failure_at = now() \
         WHERE id = $1 AND enabled",
    )
    .bind(id)
    .bind(short_error(error))
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_kind_is_an_error_not_email() {
        assert_eq!(kind_of("slack").unwrap(), ChannelKind::Slack);
        assert!(matches!(
            kind_of("pagerduty"),
            Err(ChannelError::UnknownKind(k)) if k == "pagerduty"
        ));
    }
}
