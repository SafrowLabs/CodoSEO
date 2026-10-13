//! Deploy hooks: a deploy pipeline asks for an immediate crawl of a site, which rechecks its AI
//! access too. Two doors lead here: `POST /api/v1/sites/{site}/deploy` with an API key, and a
//! per-site secret URL for platforms that cannot send headers (Vercel, Netlify). Only the
//! secret's SHA-256 hash is stored, one per site; making a new one replaces the old.
//!
//! A deploy crawl is a manual crawl (it counts against the plan's manual crawls), and the call is
//! idempotent: while the site has a crawl queued or running, or a deploy crawl was queued in the
//! last [`DEDUPE_WINDOW`], the call answers with that crawl instead of queueing another, so a
//! pipeline that fires twice (a retry, a monorepo with several projects) crawls once.

use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::accounts::{ACCOUNT_COLUMNS, Account, AccountRow};
use crate::crawl_queue::CrawlTrigger;
use crate::crawls::{CrawlStatus, ManualWindow};

/// A deploy call within this long of the last manual crawl's queueing gets that crawl back.
pub const DEDUPE_WINDOW: time::Duration = time::Duration::minutes(5);

/// What [`deploy_crawl`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployOutcome {
    /// A crawl was queued for this deploy.
    Queued { id: Uuid, number: i64 },
    /// An existing crawl answers it: one queued or running, or a manual crawl queued in the last
    /// [`DEDUPE_WINDOW`] (then `status` may already be `done`).
    Existing {
        id: Uuid,
        number: i64,
        status: CrawlStatus,
    },
    /// The plan's manual crawls are used up until `frees_at`.
    LimitReached { frees_at: OffsetDateTime },
}

/// Queues a crawl for a deploy unless one already answers it (see the module docs). The site row
/// is locked around the checks and the insert, as for the Run crawl button, so two deploy calls
/// at once queue one crawl.
pub async fn deploy_crawl(
    pool: &PgPool,
    site_id: Uuid,
    domain: &str,
    priority: i16,
    allowance: Option<ManualWindow>,
) -> Result<DeployOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM sites WHERE id = $1 FOR UPDATE")
        .bind(site_id)
        .fetch_one(&mut *tx)
        .await?;

    let now = OffsetDateTime::now_utc();
    // A crawl in flight first (running before queued), else the newest recent manual crawl.
    let existing: Option<(Uuid, CrawlStatus, OffsetDateTime)> = sqlx::query_as(
        "SELECT id, status, created_at FROM crawls WHERE site_id = $1 \
           AND (status IN ('queued', 'running') \
                OR (trigger = 'manual' AND created_at >= $2 AND status <> 'failed')) \
         ORDER BY (status = 'running') DESC, (status = 'queued') DESC, created_at DESC \
         LIMIT 1",
    )
    .bind(site_id)
    .bind(now - DEDUPE_WINDOW)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((id, status, _)) = existing {
        let number = number_of(&mut tx, site_id, id).await?;
        tx.commit().await?;
        return Ok(DeployOutcome::Existing { id, number, status });
    }

    if let Some(allowance) = allowance {
        let (used, oldest): (i64, Option<OffsetDateTime>) = sqlx::query_as(
            "SELECT count(*), min(created_at) FROM crawls \
             WHERE site_id = $1 AND trigger = 'manual' AND created_at >= $2",
        )
        .bind(site_id)
        .bind(now - allowance.window)
        .fetch_one(&mut *tx)
        .await?;
        if used >= allowance.max {
            let frees_at = oldest.unwrap_or(now) + allowance.window;
            return Ok(DeployOutcome::LimitReached { frees_at });
        }
    }

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, trigger, priority) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(site_id)
    .bind(domain)
    .bind(CrawlTrigger::Manual)
    .bind(priority)
    .fetch_one(&mut *tx)
    .await?;
    let number = number_of(&mut tx, site_id, id).await?;
    tx.commit().await?;
    Ok(DeployOutcome::Queued { id, number })
}

/// A crawl's place in its site's history (`#49`), as `crawls::enqueue_manual_checked` counts it.
async fn number_of(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    site_id: Uuid,
    id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM crawls WHERE site_id = $1 \
           AND (created_at, id) <= (SELECT created_at, id FROM crawls WHERE id = $2)",
    )
    .bind(site_id)
    .bind(id)
    .fetch_one(&mut **tx)
    .await
}

/// Stores the hash of a site's new deploy hook secret, replacing any earlier one.
pub async fn set(pool: &PgPool, site_id: Uuid, token_hash: &[u8]) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO deploy_hooks (site_id, token_hash) VALUES ($1, $2) \
         ON CONFLICT (site_id) DO UPDATE \
           SET token_hash = EXCLUDED.token_hash, created_at = now(), last_used_at = NULL",
    )
    .bind(site_id)
    .bind(token_hash)
    .execute(pool)
    .await?;
    Ok(())
}

/// Removes a site's deploy hook. True when there was one.
pub async fn delete(pool: &PgPool, site_id: Uuid) -> Result<bool, sqlx::Error> {
    let done = sqlx::query("DELETE FROM deploy_hooks WHERE site_id = $1")
        .bind(site_id)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// When a site's deploy hook was made and last used; `None` when it has none.
pub async fn status(
    pool: &PgPool,
    site_id: Uuid,
) -> Result<Option<(OffsetDateTime, Option<OffsetDateTime>)>, sqlx::Error> {
    sqlx::query_as("SELECT created_at, last_used_at FROM deploy_hooks WHERE site_id = $1")
        .bind(site_id)
        .fetch_optional(pool)
        .await
}

/// The site and its account behind a deploy hook secret's hash, marking the hook used. `None` for
/// an unknown secret.
pub async fn resolve(
    pool: &PgPool,
    token_hash: &[u8],
) -> Result<Option<(Uuid, String, Account)>, sqlx::Error> {
    #[derive(FromRow)]
    struct Row {
        site_id: Uuid,
        domain: String,
        #[sqlx(flatten)]
        account: AccountRow,
    }
    let row: Option<Row> = sqlx::query_as(&format!(
        "UPDATE deploy_hooks h SET last_used_at = now() \
         FROM sites s JOIN accounts a ON a.id = s.account_id \
         WHERE h.token_hash = $1 AND s.id = h.site_id \
         RETURNING s.id AS site_id, s.domain, {ACCOUNT_COLUMNS}"
    ))
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| (r.site_id, r.domain, r.account.into())))
}
