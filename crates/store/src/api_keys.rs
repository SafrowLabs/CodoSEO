//! API keys for the REST API and the cloud MCP server, and the daily call quota they share.
//!
//! The web crate generates the key and hashes it (SHA-256, like sessions); only the hash and a
//! short display prefix reach this module. A key is live until it is revoked.

use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::accounts::{ACCOUNT_COLUMNS, Account, parse_plan};

/// How many live keys one account may hold.
pub const MAX_LIVE_KEYS: i64 = 20;

/// Usage rows older than this many days are deleted by the daily cleanup.
pub const USAGE_RETENTION_DAYS: i64 = 35;

/// A key as the settings screen lists it. Never carries the key itself.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct ApiKey {
    pub id: Uuid,
    pub name: String,
    /// The first characters of the key, for telling keys apart.
    pub prefix: String,
    pub created_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateKeyOutcome {
    Created(ApiKey),
    /// The account already holds `max_live` live keys.
    LimitReached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charge {
    /// One call counted: `used` calls so far today, of `limit` (`None` = unlimited).
    Ok { used: i64, limit: Option<u32> },
    /// The day's allowance is spent; nothing was counted.
    OverQuota { limit: u32 },
}

/// Saves a key (its hash and display prefix, never the key). The cap is checked under a lock on
/// the account's row, so concurrent creates can't all pass the count.
pub async fn create(
    pool: &PgPool,
    account_id: Uuid,
    name: &str,
    key_hash: &[u8],
    prefix: &str,
    max_live: i64,
) -> Result<CreateKeyOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT 1 FROM accounts WHERE id = $1 FOR NO KEY UPDATE")
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM api_keys WHERE account_id = $1 AND revoked_at IS NULL",
    )
    .bind(account_id)
    .fetch_one(&mut *tx)
    .await?;
    if live >= max_live {
        return Ok(CreateKeyOutcome::LimitReached);
    }
    let key: ApiKey = sqlx::query_as(
        "INSERT INTO api_keys (account_id, name, key_hash, prefix) VALUES ($1, $2, $3, $4) \
         RETURNING id, name, prefix, created_at, last_used_at",
    )
    .bind(account_id)
    .bind(name)
    .bind(key_hash)
    .bind(prefix)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(CreateKeyOutcome::Created(key))
}

/// The account's live keys, newest first.
pub async fn list_for_account(pool: &PgPool, account_id: Uuid) -> Result<Vec<ApiKey>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, name, prefix, created_at, last_used_at FROM api_keys \
         WHERE account_id = $1 AND revoked_at IS NULL ORDER BY created_at DESC, id",
    )
    .bind(account_id)
    .fetch_all(pool)
    .await
}

/// Revokes one of the account's live keys. False when there is no such live key (unknown, already
/// revoked, or another account's).
pub async fn revoke(pool: &PgPool, account_id: Uuid, key_id: Uuid) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE api_keys SET revoked_at = now() \
         WHERE id = $1 AND account_id = $2 AND revoked_at IS NULL",
    )
    .bind(key_id)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// The key and account behind a live key's hash. Refreshes `last_used_at` at most once a minute,
/// so a busy agent doesn't write on every call.
pub async fn authenticate(
    pool: &PgPool,
    key_hash: &[u8],
) -> Result<Option<(Uuid, Account)>, sqlx::Error> {
    #[derive(FromRow)]
    struct Row {
        key_id: Uuid,
        stale: bool,
        id: Uuid,
        email: String,
        plan: String,
        is_owner: bool,
        github_id: Option<String>,
        created_at: OffsetDateTime,
    }
    let row: Option<Row> = sqlx::query_as(&format!(
        "SELECT k.id AS key_id, \
                (k.last_used_at IS NULL OR k.last_used_at < now() - interval '1 minute') AS stale, \
                {ACCOUNT_COLUMNS} \
         FROM api_keys k JOIN accounts a ON a.id = k.account_id \
         WHERE k.key_hash = $1 AND k.revoked_at IS NULL"
    ))
    .bind(key_hash)
    .fetch_optional(pool)
    .await?;
    let Some(r) = row else { return Ok(None) };
    if r.stale {
        sqlx::query("UPDATE api_keys SET last_used_at = now() WHERE id = $1")
            .bind(r.key_id)
            .execute(pool)
            .await?;
    }
    Ok(Some((
        r.key_id,
        Account {
            id: r.id,
            email: r.email,
            plan: parse_plan(&r.plan),
            is_owner: r.is_owner,
            github_id: r.github_id,
            created_at: r.created_at,
        },
    )))
}

/// Counts one API call against today's (UTC) allowance. One atomic upsert that only increments
/// below `limit`, so concurrent calls succeed exactly as often as there is allowance left and a
/// refusal consumes nothing. `None` is unlimited: it still counts, for the settings screen.
pub async fn charge(
    pool: &PgPool,
    account_id: Uuid,
    limit: Option<u32>,
) -> Result<Charge, sqlx::Error> {
    // The first call of a day inserts a row of 1, which the upsert's WHERE can't refuse.
    if limit == Some(0) {
        return Ok(Charge::OverQuota { limit: 0 });
    }
    let used: Option<i32> = sqlx::query_scalar(
        "INSERT INTO api_usage (account_id, day, calls) \
         VALUES ($1, (now() AT TIME ZONE 'utc')::date, 1) \
         ON CONFLICT (account_id, day) DO UPDATE SET calls = api_usage.calls + 1 \
           WHERE $2::int IS NULL OR api_usage.calls < $2 \
         RETURNING calls",
    )
    .bind(account_id)
    .bind(limit.map(|l| i32::try_from(l).unwrap_or(i32::MAX)))
    .fetch_optional(pool)
    .await?;
    Ok(match (used, limit) {
        (Some(used), limit) => Charge::Ok {
            used: i64::from(used),
            limit,
        },
        (None, limit) => Charge::OverQuota {
            limit: limit.unwrap_or(0),
        },
    })
}

/// Calls counted today (UTC day).
pub async fn usage_today(pool: &PgPool, account_id: Uuid) -> Result<i64, sqlx::Error> {
    let calls: Option<i32> = sqlx::query_scalar(
        "SELECT calls FROM api_usage \
         WHERE account_id = $1 AND day = (now() AT TIME ZONE 'utc')::date",
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    Ok(calls.map_or(0, i64::from))
}

/// Deletes usage rows older than [`USAGE_RETENTION_DAYS`] days. Returns how many.
pub async fn delete_old_usage(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let done =
        sqlx::query("DELETE FROM api_usage WHERE day < (now() AT TIME ZONE 'utc')::date - $1::int")
            .bind(USAGE_RETENTION_DAYS as i32)
            .execute(pool)
            .await?;
    Ok(done.rows_affected())
}
