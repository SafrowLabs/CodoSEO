//! The read side of the `crawls` table for the web app: the latest finished crawl, the crawl
//! in flight, history, and the manual-crawl count behind the plan allowance.

use codoseo_core::output::{Progress, StopReason};
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
