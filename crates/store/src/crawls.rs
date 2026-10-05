//! The read side of the `crawls` table for the web app: the latest finished crawl, the crawl
//! in flight, history, and the manual-crawl count behind the plan allowance; plus the checked
//! insert behind Run crawl.

use codoseo_core::output::{Progress, StopReason};
use codoseo_core::plan::ManualAllowance;
use codoseo_core::report::CrawlSummary;
use serde::Deserialize;
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::crawl_queue::CrawlTrigger;

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "crawl_status", rename_all = "snake_case")]
pub enum CrawlStatus {
    Queued,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, FromRow)]
pub struct Crawl {
    pub id: Uuid,
    pub site_id: Uuid,
    /// 1 for the site's first crawl, counting every crawl ever queued for it.
    pub number: i64,
    pub status: CrawlStatus,
    pub trigger: CrawlTrigger,
    pub queued_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    pub health_score: Option<i16>,
    pub checks_passed: Option<i16>,
    pub checks_total: Option<i16>,
    pub progress: Option<serde_json::Value>,
    pub summary: Option<serde_json::Value>,
    pub failure_reason: Option<String>,
}

/// `crawls.summary` as `finalize` writes it.
#[derive(Debug, Clone, Deserialize)]
pub struct StoredSummary {
    pub stop_reason: StopReason,
    pub report_summary: CrawlSummary,
    /// Failing checks as `(slug, affected pages)`; slugs this version doesn't know are kept
    /// as-is so callers can skip them.
    pub counts: Vec<(String, u32)>,
}

impl Crawl {
    pub fn progress(&self) -> Option<Progress> {
        self.progress
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    pub fn summary(&self) -> Option<StoredSummary> {
        self.summary
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Wall-clock duration of a finished (or failed) crawl.
    pub fn duration(&self) -> Option<time::Duration> {
        Some(self.finished_at? - self.started_at?)
    }

    pub fn is_active(&self) -> bool {
        matches!(self.status, CrawlStatus::Queued | CrawlStatus::Running)
    }
}

const SELECT: &str = "SELECT * FROM ( \
       SELECT id, site_id, row_number() OVER (PARTITION BY site_id ORDER BY created_at, id) AS number, \
              status, trigger, queued_at, started_at, finished_at, health_score, checks_passed, \
              checks_total, progress, summary, failure_reason, created_at \
       FROM crawls WHERE site_id = $1) c";

/// The most recent finished crawl.
pub async fn latest_done(pool: &PgPool, site_id: Uuid) -> Result<Option<Crawl>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{SELECT} WHERE status = 'done' ORDER BY finished_at DESC, number DESC LIMIT 1"
    ))
    .bind(site_id)
    .fetch_optional(pool)
    .await
}

/// For each of the account's sites with a finished crawl: `(site, health score, finished at)` of
/// the latest one, chosen like [`latest_done`] (`finished_at DESC`, then `number DESC`; `number`
/// is the crawl's place by `created_at, id`, so the ties break the same way).
pub async fn latest_done_for_account(
    pool: &PgPool,
    account_id: Uuid,
) -> Result<Vec<(Uuid, Option<i16>, Option<OffsetDateTime>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT DISTINCT ON (c.site_id) c.site_id, c.health_score, c.finished_at \
         FROM crawls c JOIN sites s ON s.id = c.site_id \
         WHERE s.account_id = $1 AND c.status = 'done' \
         ORDER BY c.site_id, c.finished_at DESC, c.created_at DESC, c.id DESC",
    )
    .bind(account_id)
    .fetch_all(pool)
    .await
}

/// The finished crawl before `crawl_id` (what the changes screen compares against).
pub async fn previous_done(
    pool: &PgPool,
    site_id: Uuid,
    crawl_id: Uuid,
) -> Result<Option<Crawl>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{SELECT} WHERE status = 'done' \
           AND finished_at < (SELECT finished_at FROM crawls WHERE id = $2) \
         ORDER BY finished_at DESC LIMIT 1"
    ))
    .bind(site_id)
    .bind(crawl_id)
    .fetch_optional(pool)
    .await
}

/// The queued or running crawl, if any (running wins over queued).
pub async fn active(pool: &PgPool, site_id: Uuid) -> Result<Option<Crawl>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{SELECT} WHERE status IN ('queued', 'running') \
         ORDER BY (status = 'running') DESC, created_at DESC LIMIT 1"
    ))
    .bind(site_id)
    .fetch_optional(pool)
    .await
}

/// One crawl of the site by ID.
pub async fn get(
    pool: &PgPool,
    site_id: Uuid,
    crawl_id: Uuid,
) -> Result<Option<Crawl>, sqlx::Error> {
    sqlx::query_as(&format!("{SELECT} WHERE id = $2"))
        .bind(site_id)
        .bind(crawl_id)
        .fetch_optional(pool)
        .await
}

/// Newest first.
pub async fn history(pool: &PgPool, site_id: Uuid, limit: i64) -> Result<Vec<Crawl>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{SELECT} ORDER BY created_at DESC, id DESC LIMIT $2"
    ))
    .bind(site_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Manual crawls queued for the site since `since` (for the plan's manual allowance).
pub async fn manual_count_since(
    pool: &PgPool,
    site_id: Uuid,
    since: OffsetDateTime,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM crawls WHERE site_id = $1 AND trigger = 'manual' AND created_at >= $2",
    )
    .bind(site_id)
    .bind(since)
    .fetch_one(pool)
    .await
}

/// The oldest manual crawl since `since`: the allowance frees up one window after it.
pub async fn oldest_manual_since(
    pool: &PgPool,
    site_id: Uuid,
    since: OffsetDateTime,
) -> Result<Option<OffsetDateTime>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT min(created_at) FROM crawls \
         WHERE site_id = $1 AND trigger = 'manual' AND created_at >= $2",
    )
    .bind(site_id)
    .bind(since)
    .fetch_one(pool)
    .await
}

/// A plan's manual-crawl allowance for one site, as a sliding window: at most `max` manual
/// crawls in any `window` (1 a week on Free, 1 a day on Pro).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManualWindow {
    pub max: i64,
    pub window: time::Duration,
}

impl ManualWindow {
    /// The window for a plan's allowance, or `None` when it is unlimited.
    pub fn for_allowance(allowance: ManualAllowance) -> Option<ManualWindow> {
        match allowance {
            ManualAllowance::PerWeek(n) => Some(ManualWindow {
                max: n.into(),
                window: time::Duration::weeks(1),
            }),
            ManualAllowance::PerDay(n) => Some(ManualWindow {
                max: n.into(),
                window: time::Duration::days(1),
            }),
            ManualAllowance::Unlimited => None,
        }
    }
}

/// What [`enqueue_manual_checked`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManualOutcome {
    /// The crawl was queued. `number` is its place in the site's history (`#49`).
    Queued { id: Uuid, number: i64 },
    /// The site already has a crawl queued or running (running wins), so nothing was queued.
    Busy(CrawlStatus),
    /// The allowance is used up. `frees_at` is when the oldest manual crawl in the window
    /// drops out of it.
    LimitReached { frees_at: OffsetDateTime },
}

/// Queues a manual crawl unless the site already has one queued or running, or `allowance`
/// is used up (`None` means unlimited; only `trigger = 'manual'` crawls count against it).
///
/// The site row is locked (`FOR UPDATE`) around the checks and the insert, so two Run crawl
/// clicks at once queue one crawl: the second waits for the first to commit, then sees it.
pub async fn enqueue_manual_checked(
    pool: &PgPool,
    site_id: Uuid,
    domain: &str,
    priority: i16,
    allowance: Option<ManualWindow>,
) -> Result<ManualOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM sites WHERE id = $1 FOR UPDATE")
        .bind(site_id)
        .fetch_one(&mut *tx)
        .await?;

    let busy: Option<CrawlStatus> = sqlx::query_scalar(
        "SELECT status FROM crawls WHERE site_id = $1 AND status IN ('queued', 'running') \
         ORDER BY (status = 'running') DESC LIMIT 1",
    )
    .bind(site_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(status) = busy {
        return Ok(ManualOutcome::Busy(status));
    }

    if let Some(allowance) = allowance {
        let now = OffsetDateTime::now_utc();
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
            return Ok(ManualOutcome::LimitReached { frees_at });
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
    // The same ordering `number` uses in `SELECT` above.
    let number: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM crawls WHERE site_id = $1 \
           AND (created_at, id) <= (SELECT created_at, id FROM crawls WHERE id = $2)",
    )
    .bind(site_id)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ManualOutcome::Queued { id, number })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_windows_per_plan() {
        assert_eq!(
            ManualWindow::for_allowance(ManualAllowance::PerWeek(1)),
            Some(ManualWindow {
                max: 1,
                window: time::Duration::days(7)
            })
        );
        assert_eq!(
            ManualWindow::for_allowance(ManualAllowance::PerDay(3)),
            Some(ManualWindow {
                max: 3,
                window: time::Duration::hours(24)
            })
        );
        assert_eq!(
            ManualWindow::for_allowance(ManualAllowance::Unlimited),
            None
        );
    }
}
