//! Types that cross the MCP boundary: audit identity and state, and the small
//! report shapes the tools return. `PageRecord` and `Change` are core's own types,
//! reused as-is.

use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use codoseo_core::check::{CheckId, Severity};
use codoseo_core::output::Progress;

/// A running or finished audit's identity. The simple (no-dash) form of a v4 UUID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AuditId(pub String);

impl AuditId {
    pub fn new() -> AuditId {
        AuditId(Uuid::new_v4().simple().to_string())
    }
}

impl Default for AuditId {
    fn default() -> AuditId {
        AuditId::new()
    }
}

impl std::fmt::Display for AuditId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "reason")]
pub enum AuditStatus {
    Running,
    Done,
    Failed(String),
}

/// What starting an audit hands back right away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditHandle {
    pub id: AuditId,
    /// Flattened, so the wire shape is `"status": "running"` rather than nesting
    /// `AuditStatus`'s own tag under a second `status` key.
    #[serde(flatten)]
    pub status: AuditStatus,
}

/// What `get_audit` (and the post-wait check inside `audit_site`) returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditState {
    pub id: AuditId,
    #[serde(flatten)]
    pub status: AuditStatus,
    /// Set while `status` is `Running`.
    pub progress: Option<Progress>,
    /// Set once `status` is `Done`.
    pub summary: Option<AuditSummary>,
}

/// `failing_checks` is capped at this many entries (most severe first), so the summary
/// stays agent-sized even for a site that fails every check. Checks beyond the cap are
/// counted in `more_failing_checks`, not listed.
pub const MAX_FAILING_CHECKS: usize = 15;

/// Agent-sized: score, counts and up to 3 example URLs per failing check. Never the
/// full page list. Must stay well under 4 KB even for a large crawl with every check
/// failing (see `tests::summary_for_a_bad_500_page_crawl_stays_small`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditSummary {
    pub audit_id: AuditId,
    pub start_url: Url,
    pub health_score: u8,
    pub checks_passed: u16,
    pub checks_total: u16,
    pub pages_crawled: u32,
    /// Human words, e.g. "completed" or "page limit reached".
    pub stop_reason: String,
    /// At most [`MAX_FAILING_CHECKS`] entries, most severe first.
    pub failing_checks: Vec<FailingCheck>,
    /// Failing checks beyond the [`MAX_FAILING_CHECKS`] cap, not otherwise listed.
    pub more_failing_checks: u16,
}

/// Failing checks in the order summaries list them: most severe first, then the most affected
/// pages, then by check id so equal counts always come out the same way.
pub fn rank_failing(counts: impl IntoIterator<Item = (CheckId, u32)>) -> Vec<(CheckId, u32)> {
    let mut ranked: Vec<(CheckId, u32)> = counts.into_iter().collect();
    ranked.sort_by_key(|&(check, count)| {
        (
            codoseo_checks::def(check).severity,
            std::cmp::Reverse(count),
            check,
        )
    });
    ranked
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailingCheck {
    pub check: CheckId,
    pub title: String,
    pub severity: Severity,
    pub count: u32,
    pub example_urls: Vec<Url>,
}

/// One row of `get_issue_urls`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlRow {
    pub url: Url,
    pub status: u16,
    pub title: Option<String>,
    pub indexability: codoseo_core::page::Indexability,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RobotsReport {
    pub status: u16,
    pub path: String,
    pub allowed: bool,
    pub crawl_delay_secs: Option<f64>,
    pub sitemaps: Vec<String>,
}

/// What `check_ai_access` returns: who robots.txt lets in for one path, what the site declares
/// about use of its content, and whether the page itself can appear in each engine's AI answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiAccessReport {
    pub url: String,
    pub path: String,
    /// One line an agent can relay as it is.
    pub summary: String,
    pub robots: AiRobots,
    /// Every registry bot's verdict for the path.
    pub bots: Vec<codoseo_geo::report::BotVerdict>,
    /// Stated preferences in robots.txt. Declared, not enforced.
    pub declared: codoseo_geo::report::RobotsDeclared,
    /// Empty when the page was not a successful HTML response.
    pub engines: Vec<AiEngine>,
    /// The page's HTTP status, `None` when it could not be fetched.
    pub page_status: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRobots {
    pub status: u16,
    pub availability: codoseo_geo::robots::RobotsAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiEngine {
    pub id: codoseo_geo::eligibility::EngineId,
    pub name: String,
    pub effect: codoseo_geo::eligibility::Effect,
    pub causes: Vec<codoseo_geo::eligibility::Cause>,
    /// Why `effect` is always eligible, for engines with no documented page-level control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedirectReport {
    pub hops: Vec<(u16, Url)>,
    pub final_status: Option<u16>,
    pub final_url: Option<Url>,
    /// `redirect_loop` or `too_many_redirects` when the chain never settled.
    pub problem: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_core::check::CheckId;

    #[test]
    fn audit_state_round_trips_through_json() {
        let state = AuditState {
            id: AuditId("abc123".to_owned()),
            status: AuditStatus::Running,
            progress: Some(Progress {
                pages_done: 12,
                queued: 3,
                failures: 0,
                depth: 2,
                elapsed_ms: 1500,
            }),
            summary: None,
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: AuditState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, back);
    }

    #[test]
    fn audit_summary_round_trips_through_json() {
        let summary = AuditSummary {
            audit_id: AuditId::new(),
            start_url: Url::parse("https://example.com/").unwrap(),
            health_score: 83,
            checks_passed: 38,
            checks_total: 44,
            pages_crawled: 120,
            stop_reason: "completed".to_owned(),
            failing_checks: vec![FailingCheck {
                check: CheckId::TitleMissing,
                title: "Missing title".to_owned(),
                severity: Severity::Warning,
                count: 4,
                example_urls: vec![Url::parse("https://example.com/a").unwrap()],
            }],
            more_failing_checks: 0,
        };
        let json = serde_json::to_string(&summary).unwrap();
        let back: AuditSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(summary, back);
    }

    #[test]
    fn failing_checks_rank_by_severity_then_count_then_id() {
        let ranked = rank_failing([
            (CheckId::TitleMissing, 4),
            (CheckId::Http4xx, 1),
            (CheckId::H1Missing, 4),
            (CheckId::TitleTooLong, 9),
        ]);
        // Critical first, then warnings (equal counts by check id), and a less severe check
        // after them however many pages it affects.
        let order: Vec<CheckId> = ranked.iter().map(|r| r.0).collect();
        assert_eq!(
            order,
            [
                CheckId::Http4xx,
                CheckId::TitleMissing,
                CheckId::H1Missing,
                CheckId::TitleTooLong
            ]
        );
    }

    /// A synthetic worst case: 500 pages, every check failing, 3 example URLs each,
    /// capped at `MAX_FAILING_CHECKS` entries. No real crawl runs here; this is purely
    /// a size check on the summary shape.
    #[test]
    fn summary_for_a_bad_500_page_crawl_stays_small() {
        let all = CheckId::ALL;
        let shown = &all[..MAX_FAILING_CHECKS];
        let failing_checks = shown
            .iter()
            .map(|&check| FailingCheck {
                check,
                title: "A reasonably descriptive check title".to_owned(),
                severity: Severity::Warning,
                count: 500,
                example_urls: (0..3)
                    .map(|i| {
                        Url::parse(&format!("https://example.com/some/long/path/{i}")).unwrap()
                    })
                    .collect(),
            })
            .collect();
        let summary = AuditSummary {
            audit_id: AuditId::new(),
            start_url: Url::parse("https://example.com/").unwrap(),
            health_score: 0,
            checks_passed: 0,
            checks_total: all.len() as u16,
            pages_crawled: 500,
            stop_reason: "completed".to_owned(),
            failing_checks,
            more_failing_checks: (all.len() - MAX_FAILING_CHECKS) as u16,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(
            json.len() < 4096,
            "summary is {} bytes, expected under 4096",
            json.len()
        );
    }

    #[test]
    fn audit_id_is_unique_and_display_matches_inner_string() {
        let a = AuditId::new();
        let b = AuditId::new();
        assert_ne!(a, b);
        assert_eq!(a.to_string(), a.0);
    }
}
