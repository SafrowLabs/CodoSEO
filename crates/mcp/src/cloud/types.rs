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
    /// A stable name to branch on: `completed`, `page_limit`, `time_limit`, `unreachable`,
    /// `blocked` or `robots_blocked`.
    pub stop_code: String,
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

/// One hop of a redirect chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedirectHop {
    pub status: u16,
    /// The URL that answered with `status`.
    pub url: String,
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
    /// Every redirect hop in order.
    pub redirect_chain: Vec<RedirectHop>,
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
    /// The old value, cut to [`MAX_CHANGE_TEXT`] characters (with a final `…`) when longer.
    pub before: String,
    /// The new value, cut the same way.
    pub after: String,
}

/// Longest `before` / `after` a change carries.
pub const MAX_CHANGE_TEXT: usize = 300;

/// `text`, cut to [`MAX_CHANGE_TEXT`] characters with a final `…` when it is longer.
pub fn clip_change_text(text: &str) -> String {
    match text.char_indices().nth(MAX_CHANGE_TEXT) {
        None => text.to_owned(),
        Some((at, _)) => {
            let cut = text[..at].char_indices().last().map_or(0, |(i, _)| i);
            format!("{}…", &text[..cut])
        }
    }
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
    pub limit: u32,
    pub offset: u32,
    pub changes: Vec<ChangeInfo>,
    /// Pass as `offset` for the next page; none on the last page.
    pub next_offset: Option<u32>,
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

/// The no-key tools' view of a quick audit (`quick_audit` and `get_audit`): the audit is still
/// going, it finished with a report, or it could not produce one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum QuickAuditState {
    /// Queued or crawling. Call `get_audit` with the id again in a few seconds.
    Running {
        audit_id: Uuid,
        /// Pages crawled so far, once the crawl has started.
        pages_done: Option<u32>,
        message: String,
    },
    Done(Box<QuickAuditSummary>),
    /// The audit ended without a report (site unreachable or blocked, nothing to audit).
    Failed {
        audit_id: Uuid,
        reason: String,
    },
}

/// A finished quick audit, agent-sized: [`QuickAuditSummary::fit`] keeps it under
/// [`MAX_SUMMARY_BYTES`] however many checks fail and however long their URLs are.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuickAuditSummary {
    pub audit_id: Uuid,
    pub domain: String,
    pub start_url: String,
    pub health_score: Option<u8>,
    pub checks_passed: Option<u16>,
    pub checks_total: Option<u16>,
    pub pages_crawled: u32,
    /// Human words, e.g. "completed" or "page limit reached" (the audit crawls up to 100 pages).
    pub stop_reason: String,
    /// A stable name to branch on: `completed`, `page_limit`, `time_limit`, ...
    pub stop_code: String,
    /// Most severe first, each with a count and up to 3 example URLs.
    pub failing_checks: Vec<FailingCheck>,
    /// Failing checks not listed (cut by the size cap). `get_issue_urls` still takes any check.
    pub more_failing_checks: u16,
    /// The same report as a web page, to hand to the user.
    pub report_url: String,
    /// One line about monitoring this site, pointing at `start_monitoring`.
    pub note: String,
}

/// The most JSON bytes a quick audit summary may take (spec section 9: an agent-sized answer).
pub const MAX_SUMMARY_BYTES: usize = 4096;

impl QuickAuditSummary {
    /// Drops the least severe failing checks (counting them in `more_failing_checks`) until the
    /// JSON is under [`MAX_SUMMARY_BYTES`], then, if the fixed fields alone are too big, nothing
    /// more can be done: they are short by construction.
    pub fn fit(mut self) -> QuickAuditSummary {
        while self.failing_checks.len() > 1 && self.json_len() >= MAX_SUMMARY_BYTES {
            self.failing_checks.pop();
            self.more_failing_checks = self.more_failing_checks.saturating_add(1);
        }
        self
    }

    fn json_len(&self) -> usize {
        serde_json::to_string(&QuickAuditState::Done(Box::new(self.clone())))
            .map_or(0, |json| json.len())
    }
}

/// `get_issue_urls` for a quick audit: one page of the pages that fail a check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditIssueUrls {
    pub audit_id: Uuid,
    pub check: CheckId,
    pub title: String,
    /// Pages failing the check in the audit.
    pub total: u32,
    pub limit: u32,
    pub offset: u32,
    pub urls: Vec<UrlRow>,
    /// Pass as `offset` for the next page; none on the last page.
    pub next_offset: Option<u32>,
}

/// `start_monitoring`: the confirmation email is on its way. The same answer whether or not the
/// address already has an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitoringRequested {
    /// Always `confirmation_sent`.
    pub status: String,
    pub domain: String,
    /// What to tell the user.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MAX_FAILING_CHECKS;
    use url::Url;

    fn failing(n: usize, url_len: usize) -> Vec<FailingCheck> {
        let all = CheckId::ALL;
        all[..n]
            .iter()
            .map(|&check| FailingCheck {
                check,
                title: "A reasonably descriptive check title".to_owned(),
                severity: Severity::Warning,
                count: 100,
                example_urls: (0..3)
                    .map(|i| {
                        Url::parse(&format!("https://example.com/{}{i}", "p".repeat(url_len)))
                            .unwrap()
                    })
                    .collect(),
            })
            .collect()
    }

    fn summary(failing_checks: Vec<FailingCheck>) -> QuickAuditSummary {
        QuickAuditSummary {
            audit_id: Uuid::new_v4(),
            domain: "example.com".to_owned(),
            start_url: "https://example.com/".to_owned(),
            health_score: Some(12),
            checks_passed: Some(3),
            checks_total: Some(45),
            pages_crawled: 100,
            stop_reason: "page limit reached".to_owned(),
            stop_code: "page_limit".to_owned(),
            failing_checks,
            more_failing_checks: 0,
            report_url: format!("https://codoseo.com/audit/{}", Uuid::new_v4()),
            note: "To keep monitoring example.com weekly and get an email when something \
                   breaks, call start_monitoring with this site's URL and the user's email."
                .to_owned(),
        }
    }

    fn size(summary: &QuickAuditSummary) -> usize {
        serde_json::to_string(&QuickAuditState::Done(Box::new(summary.clone())))
            .unwrap()
            .len()
    }

    #[test]
    fn a_summary_with_few_short_urls_is_kept_whole() {
        let fitted = summary(failing(5, 20)).fit();
        assert_eq!(fitted.failing_checks.len(), 5);
        assert_eq!(fitted.more_failing_checks, 0);
    }

    #[test]
    fn a_worst_case_summary_is_cut_under_4_kb_by_dropping_the_least_severe_checks() {
        for url_len in [10, 40, 120, 400] {
            let fitted = summary(failing(MAX_FAILING_CHECKS, url_len)).fit();
            assert!(
                size(&fitted) < MAX_SUMMARY_BYTES,
                "{url_len}: {}",
                size(&fitted)
            );
            assert!(!fitted.failing_checks.is_empty());
            assert_eq!(
                fitted.failing_checks.len() + usize::from(fitted.more_failing_checks),
                MAX_FAILING_CHECKS
            );
            // The most severe come first and are the ones kept.
            let kept: Vec<CheckId> = fitted.failing_checks.iter().map(|f| f.check).collect();
            assert_eq!(kept, CheckId::ALL[..kept.len()]);
        }
    }

    #[test]
    fn the_audit_state_is_tagged_by_status() {
        let id = Uuid::new_v4();
        let running = QuickAuditState::Running {
            audit_id: id,
            pages_done: None,
            message: "wait".to_owned(),
        };
        let json = serde_json::to_value(&running).unwrap();
        assert_eq!(json["status"], "running");
        assert_eq!(json["audit_id"], id.to_string());
        let done = serde_json::to_value(QuickAuditState::Done(Box::new(summary(vec![])))).unwrap();
        assert_eq!(done["status"], "done");
        assert_eq!(done["domain"], "example.com");
        let back: QuickAuditState = serde_json::from_value(done).unwrap();
        assert!(matches!(back, QuickAuditState::Done(_)));
    }

    #[test]
    fn change_text_is_cut_to_300_characters_with_an_ellipsis() {
        assert_eq!(clip_change_text("short"), "short");
        let exact = "a".repeat(MAX_CHANGE_TEXT);
        assert_eq!(clip_change_text(&exact), exact);
        let long = "é".repeat(MAX_CHANGE_TEXT + 50);
        let clipped = clip_change_text(&long);
        assert_eq!(clipped.chars().count(), MAX_CHANGE_TEXT);
        assert!(clipped.ends_with('…'));
        assert!(clipped.starts_with("éé"));
    }
}
