//! JSON shapes of the cloud API: what `GET /api/v1/...` and the keyed MCP tools return. Times are
//! RFC 3339 (UTC). Check ids are the stable slugs (`title_missing`).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use codoseo_core::change::ChangeKind;
use codoseo_core::check::{CheckId, Severity};
use codoseo_core::page::Indexability;

use crate::types::{FailingCheck, UrlRow};

/// One monitored site, as `list_sites` returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteInfo {
    pub id: Uuid,
    pub domain: String,
    pub start_url: String,
    pub monitoring_active: bool,
    /// `weekly`, `daily`, or none when the site isn't crawled on a schedule.
    pub schedule: Option<String>,
    /// Health score (0 to 100) of the latest finished crawl.
    pub health_score: Option<u8>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_crawled_at: Option<OffsetDateTime>,
}

/// A site's latest finished crawl.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrawlHealth {
    pub crawl_id: Uuid,
    /// 1 for the site's first crawl.
    pub number: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    pub health_score: Option<u8>,
    pub checks_passed: Option<u16>,
    pub checks_total: Option<u16>,
    pub pages_crawled: u32,
    /// Human words, e.g. "completed" or "page limit reached".
    pub stop_reason: String,
    /// Most severe first, at most 15, each with 3 example URLs.
    pub failing_checks: Vec<FailingCheck>,
    /// Failing checks beyond the 15 listed.
    pub more_failing_checks: u16,
}

/// A crawl that is queued or running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveCrawl {
    pub crawl_id: Uuid,
    pub number: i64,
    /// `queued` or `running`.
    pub status: String,
    pub pages_done: Option<u32>,
}

/// `get_site_health`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteHealth {
    pub id: Uuid,
    pub domain: String,
    pub start_url: String,
    pub monitoring_active: bool,
    pub schedule: Option<String>,
    /// When the scheduler crawls next; none when the site has no schedule or monitoring is off.
    #[serde(with = "time::serde::rfc3339::option")]
    pub next_crawl_at: Option<OffsetDateTime>,
    /// None until the first crawl has finished.
    pub latest_crawl: Option<CrawlHealth>,
    /// Set while a crawl is queued or running.
    pub active_crawl: Option<ActiveCrawl>,
    /// The site's audit screen in the web app.
    pub audit_url: String,
}

/// `get_issue_urls`: one page of the pages that fail a check in the latest finished crawl.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueUrlsPage {
    pub site_id: Uuid,
    pub check: CheckId,
    pub title: String,
    /// The crawl read; none when the site has no finished crawl yet (then `urls` is empty).
    pub crawl_number: Option<i64>,
    /// Pages failing the check in that crawl.
    pub total: u32,
    pub limit: u32,
    pub offset: u32,
    pub urls: Vec<UrlRow>,
    /// Pass as `offset` for the next page; none on the last page.
    pub next_offset: Option<u32>,
}

/// A page's issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageIssue {
    pub check: CheckId,
    pub title: String,
    pub severity: Severity,
}

/// `get_page`: what the latest finished crawl stored about one URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageInfo {
    pub site_id: Uuid,
    pub crawl_number: i64,
    pub url: String,
    pub status: u16,
    /// Every redirect hop in order: the status and the URL that returned it.
    pub redirect_chain: Vec<(u16, String)>,
    pub redirect_target: Option<String>,
    pub response_ms: Option<u32>,
    pub size_bytes: Option<u64>,
    pub content_type: Option<String>,
    pub depth: Option<u32>,
    pub in_sitemap: bool,
    pub indexability: Indexability,
    pub title: Option<String>,
    pub meta_description: Option<String>,
    pub meta_robots: Option<String>,
    pub x_robots_tag: Option<String>,
    pub canonical: Option<String>,
    pub h1: Vec<String>,
    pub h2: Vec<String>,
    pub word_count: Option<u32>,
    pub inlinks: u32,
    pub outlinks_internal: u32,
    pub outlinks_external: u32,
    /// The checks this page fails, most severe first.
    pub issues: Vec<PageIssue>,
}

/// One change between the latest finished crawl and the one before it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeInfo {
    pub kind: ChangeKind,
    pub severity: Severity,
    /// The page the change is about; none for site-wide changes (robots.txt, sitemap, spike).
    pub url: Option<String>,
    pub before: String,
    pub after: String,
}

/// `get_changes`: the latest finished crawl's changes, most severe first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangesPage {
    pub site_id: Uuid,
    /// The crawl read; none when the site has no finished crawl yet.
    pub crawl_number: Option<i64>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub crawl_finished_at: Option<OffsetDateTime>,
    /// Changes matching the severity filter, which may be more than `changes` holds.
    pub total: u32,
    pub changes: Vec<ChangeInfo>,
}

/// `run_crawl`: the crawl that was queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrawlQueued {
    pub site_id: Uuid,
    pub crawl_id: Uuid,
    pub number: i64,
    /// Always `queued`; poll `get_site_health` (`active_crawl`) to follow it.
    pub status: String,
}

/// `GET /api/v1/usage`: today's (UTC) API calls against the plan's allowance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub calls_today: u32,
    /// None on plans without a limit (self-hosted).
    pub limit: Option<u32>,
    pub remaining: Option<u32>,
    /// The next 00:00 UTC, when the count starts again.
    #[serde(with = "time::serde::rfc3339")]
    pub resets_at: OffsetDateTime,
}
