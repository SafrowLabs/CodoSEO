//! What the Monday digest says about one site: the latest finished crawl, the crawl a week
//! earlier to compare it with, and the changes of the last seven days.

use std::collections::HashMap;

use serde::Deserialize;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::reports::{ChangeRow, from_slug};

/// How many example changes the digest lists per site.
pub const TOP_CHANGES: i64 = 5;

/// A finished crawl as the digest reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrawlStats {
    pub crawl_id: Uuid,
    pub finished_at: OffsetDateTime,
    pub score: i16,
    pub checks_passed: i16,
    pub checks_total: i16,
    /// Failing checks as `(slug, affected pages)`, in the order the crawl stored them.
    pub counts: Vec<(String, u32)>,
}

/// The week's changes, counted by severity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeverityCounts {
    pub critical: u32,
    pub warning: u32,
    pub notice: u32,
}

impl SeverityCounts {
    pub fn total(&self) -> u32 {
        self.critical + self.warning + self.notice
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteWeek {
    pub site_id: Uuid,
    pub domain: String,
    pub latest: CrawlStats,
    /// The crawl to compare with; `None` in the site's first week.
    pub baseline: Option<CrawlStats>,
    /// Checks failing in `latest` that were not failing in the baseline, with this week's page
    /// count. Empty without a baseline.
    pub new_issues: Vec<(String, u32)>,
    /// Checks failing in the baseline that pass now, with the baseline's page count.
    pub resolved_issues: Vec<(String, u32)>,
    /// Every change of the last seven days, whether or not it was also sent instantly.
    pub changes: SeverityCounts,
    /// The [`TOP_CHANGES`] most severe of them.
    pub top_changes: Vec<ChangeRow>,
}

impl SiteWeek {
    /// The health score against the baseline; `None` without one.
    pub fn score_delta(&self) -> Option<i32> {
        self.baseline
            .as_ref()
            .map(|b| i32::from(self.latest.score) - i32::from(b.score))
    }
}

type CrawlRow = (
    Uuid,
    OffsetDateTime,
    Option<i16>,
    Option<i16>,
    Option<i16>,
    Option<serde_json::Value>,
);

const CRAWL_COLUMNS: &str = "id, finished_at, health_score, checks_passed, checks_total, summary FROM crawls \
     WHERE site_id = $1 AND status = 'done' AND finished_at IS NOT NULL \
       AND health_score IS NOT NULL AND checks_passed IS NOT NULL AND checks_total IS NOT NULL";

#[derive(Deserialize)]
struct CountsOnly {
    counts: Vec<(String, u32)>,
}

/// Reads a crawl row. `None` when the row is incomplete or its `summary` can't be read: a
/// missing or malformed summary must not pass for "no failing checks", which would make every
/// issue look new or resolved.
fn stats(row: CrawlRow) -> Option<CrawlStats> {
    let (crawl_id, finished_at, score, passed, total, summary) = row;
    let counts = serde_json::from_value::<CountsOnly>(summary?).ok()?.counts;
    Some(CrawlStats {
        crawl_id,
        finished_at,
        score: score?,
        checks_passed: passed?,
        checks_total: total?,
        counts: counts.into_iter().filter(|(_, n)| *n > 0).collect(),
    })
}

/// How long before the latest crawl the baseline must have finished. Weekly crawls drift by
/// minutes either way, so this is a day short of a week.
const BASELINE_MIN_GAP: Duration = Duration::days(6);

/// The site's week ending `now`, or `None` when it has no finished crawl yet.
///
/// The baseline is the latest finished crawl at least six days before the newest one, or,
/// when there is none, the oldest finished crawl inside the last seven days (other than the
/// newest). The change summary covers all of the last seven days' changes whatever their
/// `alerted_at`: that column records routing to an instant channel, not a delivery.
pub async fn site_week(
    pool: &PgPool,
    site_id: Uuid,
    now: OffsetDateTime,
) -> Result<Option<SiteWeek>, sqlx::Error> {
    let week_ago = now - Duration::days(7);

    let latest: Option<CrawlRow> = sqlx::query_as(&format!(
        "SELECT {CRAWL_COLUMNS} AND finished_at <= $2 ORDER BY finished_at DESC, created_at DESC LIMIT 1"
    ))
    .bind(site_id)
    .bind(now)
    .fetch_optional(pool)
    .await?;
    let Some(latest_row) = latest else {
        return Ok(None);
    };
    let latest_id = latest_row.0;
    let Some(latest) = stats(latest_row) else {
        tracing::warn!(site = %site_id, crawl = %latest_id, "digest: skipping a site whose latest crawl has no readable summary");
        return Ok(None);
    };

    let six_days_before: Option<CrawlRow> = sqlx::query_as(&format!(
        "SELECT {CRAWL_COLUMNS} AND id <> $2 AND finished_at <= $3 \
         ORDER BY finished_at DESC, created_at DESC LIMIT 1"
    ))
    .bind(site_id)
    .bind(latest.crawl_id)
    .bind(latest.finished_at - BASELINE_MIN_GAP)
    .fetch_optional(pool)
    .await?;
    let baseline = match six_days_before {
        Some(row) => Some(row),
        None => {
            sqlx::query_as(&format!(
                "SELECT {CRAWL_COLUMNS} AND id <> $2 AND finished_at < $3 AND finished_at > $4 \
                 ORDER BY finished_at ASC, created_at ASC LIMIT 1"
            ))
            .bind(site_id)
            .bind(latest.crawl_id)
            .bind(latest.finished_at)
            .bind(week_ago)
            .fetch_optional(pool)
            .await?
        }
    }
    .and_then(stats);

    let (new_issues, resolved_issues) = match &baseline {
        Some(base) => compare(&base.counts, &latest.counts),
        None => (Vec::new(), Vec::new()),
    };

    let domain: String = sqlx::query_scalar("SELECT domain FROM sites WHERE id = $1")
        .bind(site_id)
        .fetch_one(pool)
        .await?;

    let by_severity: Vec<(String, i64)> = sqlx::query_as(
        "SELECT severity::text, count(*) FROM changes \
         WHERE site_id = $1 AND created_at > $2 AND created_at <= $3 GROUP BY severity",
    )
    .bind(site_id)
    .bind(week_ago)
    .bind(now)
    .fetch_all(pool)
    .await?;
    let mut changes = SeverityCounts::default();
    for (severity, n) in by_severity {
        let n = u32::try_from(n).unwrap_or(u32::MAX);
        match severity.as_str() {
            "critical" => changes.critical = n,
            "warning" => changes.warning = n,
            "notice" => changes.notice = n,
            _ => {}
        }
    }

    let rows: Vec<(String, String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT kind::text, severity::text, url, before_value, after_value FROM changes \
         WHERE site_id = $1 AND created_at > $2 AND created_at <= $3 \
         ORDER BY changes.severity, id LIMIT $4",
    )
    .bind(site_id)
    .bind(week_ago)
    .bind(now)
    .bind(TOP_CHANGES)
    .fetch_all(pool)
    .await?;
    let top_changes = rows
        .into_iter()
        .filter_map(|(kind, severity, url, before, after)| {
            Some(ChangeRow {
                kind: from_slug(&kind)?,
                severity: from_slug(&severity)?,
                url,
                before,
                after,
            })
        })
        .collect();

    Ok(Some(SiteWeek {
        site_id,
        domain,
        latest,
        baseline,
        new_issues,
        resolved_issues,
        changes,
        top_changes,
    }))
}

/// Failing checks as `(slug, pages)`.
type Issues = Vec<(String, u32)>;

/// `(new, resolved)` from two sets of failing-check counts.
fn compare(before: &[(String, u32)], now: &[(String, u32)]) -> (Issues, Issues) {
    let before_map: HashMap<&str, u32> = before.iter().map(|(s, n)| (s.as_str(), *n)).collect();
    let now_map: HashMap<&str, u32> = now.iter().map(|(s, n)| (s.as_str(), *n)).collect();
    let new = now
        .iter()
        .filter(|(s, _)| !before_map.contains_key(s.as_str()))
        .cloned()
        .collect();
    let resolved = before
        .iter()
        .filter(|(s, _)| !now_map.contains_key(s.as_str()))
        .cloned()
        .collect();
    (new, resolved)
}

/// The account fields the digest needs.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DigestAccount {
    pub email: String,
    /// An IANA zone name; the caller treats anything it can't parse as UTC.
    pub timezone: String,
    pub paused: bool,
}

pub async fn account(pool: &PgPool, id: Uuid) -> Result<Option<DigestAccount>, sqlx::Error> {
    sqlx::query_as("SELECT email, timezone, paused FROM accounts WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}
