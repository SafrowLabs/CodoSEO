//! `/demo`: the site audit and its AI access part for a sample site, public and read-only, on the
//! cloud only. Everything comes from a fixture compiled into the binary: no crawl runs, nothing is
//! read from or written to the database, and no account is involved. The
//! fixture, `crates/web/demo/fixture.json`, is `codoseo crawl --format json` of a small static
//! shop served on 127.0.0.1 (a robots.txt blocking PerplexityBot and GPTBot, a `nosnippet` page,
//! an app-shell page, broken links, missing descriptions), with the address replaced by
//! `https://www.example.com` and the `not_https` count that only the local HTTP server caused
//! dropped: its `summary` is the stored crawl summary and `access_report` the AI access report. The
//! page reuses the audit screen's view code and template with its actions turned off, and adds the
//! fix for each AI access finding, as the AI access screen would give it.

use std::collections::HashMap;

use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use codoseo_checks::{BASE_CHECKS, health_score};
use codoseo_core::change::ChangeKind;
use codoseo_core::check::{CheckId, Severity};
use codoseo_geo::Intent;
use codoseo_geo::findings::{Finding, findings};
use codoseo_geo::remedy::{FindingRemedy, finding_remedies};
use codoseo_geo::report::AccessReport;
use codoseo_store::crawl_queue::CrawlTrigger;
use codoseo_store::crawls::{Crawl, CrawlStatus};
use codoseo_store::geo::Incident;
use codoseo_store::reports::{ChangeCounts, ResponseBuckets};
use serde::Deserialize;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::config::{Mode, UmamiConfig};
use crate::error::AppError;
use crate::render::html;
use crate::routes::audit::{AiData, AuditBody, DoneInput, done_view};
use crate::state::AppState;

/// The sample site's domain: a reserved example domain, so it is never a real site.
pub const DEMO_DOMAIN: &str = "www.example.com";
const DEMO_ORIGIN: &str = "https://www.example.com/";
const FIXTURE: &str = include_str!("../../demo/fixture.json");
/// Health scores of the twelve crawls before the fixture's, oldest first: the trend chart's line.
const EARLIER_SCORES: [i16; 12] = [58, 61, 60, 64, 66, 65, 69, 71, 70, 73, 74, 76];

pub fn routes() -> Router<AppState> {
    Router::new().route("/demo", get(page))
}

#[derive(Deserialize)]
struct Fixture {
    summary: serde_json::Value,
    access_report: AccessReport,
}

/// One AI access finding with its fix, for the list under the audit.
pub struct DemoFix {
    pub anchor: String,
    pub finding: Finding,
    pub remedy: FindingRemedy,
}

impl DemoFix {
    pub fn severity(&self) -> &'static str {
        match self.finding.severity {
            Severity::Critical => "critical",
            Severity::Warning => "warning",
            Severity::Notice => "notice",
        }
    }
}

#[derive(Template)]
#[template(path = "demo/index.html")]
pub struct DemoPage {
    pub base: String,
    pub body: AuditBody,
    pub fixes: Vec<DemoFix>,
    pub umami: Option<UmamiConfig>,
}

async fn page(State(state): State<AppState>) -> Result<Response, AppError> {
    if state.config.mode != Mode::Cloud {
        return Err(AppError::NotFound);
    }
    let mut page = demo(OffsetDateTime::now_utc())?;
    page.base = state.config.origin();
    page.umami = state.config.umami.clone();
    Ok(html(&page)?.into_response())
}

/// A finished crawl of the sample site, `hours_ago` hours ago.
fn crawl(
    number: i64,
    now: OffsetDateTime,
    hours_ago: i64,
    score: i16,
    summary: serde_json::Value,
) -> Crawl {
    let finished = now - Duration::hours(hours_ago);
    let counts = summary["counts"].as_array().map_or(0, Vec::len);
    Crawl {
        id: Uuid::from_u128(0xdec0_0000 + number as u128),
        site_id: Uuid::nil(),
        number,
        status: CrawlStatus::Done,
        trigger: CrawlTrigger::Schedule,
        queued_at: finished - Duration::seconds(50),
        started_at: Some(finished - Duration::seconds(41)),
        finished_at: Some(finished),
        health_score: Some(score),
        checks_passed: Some((BASE_CHECKS - counts) as i16),
        checks_total: Some(BASE_CHECKS as i16),
        // The demo has no AI access fixture to score.
        geo_score: None,
        geo_checks_passed: None,
        geo_checks_applicable: None,
        overall_score: None,
        progress: None,
        summary: Some(summary),
        failure_reason: None,
    }
}

/// The page, built from the fixture as of `now`. Pure: no database, no network.
pub fn demo(now: OffsetDateTime) -> Result<DemoPage, AppError> {
    let fixture: Fixture = serde_json::from_str(FIXTURE).map_err(AppError::internal)?;
    let summary = fixture.summary;
    let counts: Vec<(CheckId, u32)> = summary["counts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some((
                CheckId::from_slug(c.get(0)?.as_str()?)?,
                u32::try_from(c.get(1)?.as_u64()?).ok()?,
            ))
        })
        .collect();
    let pages = summary["report_summary"]["pages"].as_u64().unwrap_or(0) as u32;
    let score = i16::from(health_score(&counts, pages));

    // Weekly crawls: the twelve before this one had one more broken page each few weeks.
    let mut recent: Vec<Crawl> = EARLIER_SCORES
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut older = summary.clone();
            if let Some(list) = older["counts"].as_array_mut() {
                for c in list.iter_mut().filter(|c| c[0] == "http_4xx") {
                    c[1] = serde_json::json!(c[1].as_u64().unwrap_or(0) + (12 - i as u64) / 3);
                }
            }
            crawl(i as i64 + 1, now, 3 + 168 * (12 - i as i64), *s, older)
        })
        .collect();
    recent.push(crawl(13, now, 3, score, summary));
    let latest = recent.last().expect("the latest crawl").clone();

    let report = fixture.access_report;
    let intent = Intent::default();
    let found = findings(&report, &intent);
    let opened = now - Duration::hours(3);
    let open: Vec<Incident> = found
        .iter()
        .enumerate()
        .map(|(i, f)| Incident {
            id: i as i64 + 1,
            kind: f.kind,
            subject: f.subject.clone(),
            severity: f.severity,
            title: f.title.clone(),
            summary: f.summary.clone(),
            evidence: serde_json::to_value(&f.evidence).unwrap_or_default(),
            opened_crawl_id: Some(latest.id),
            opened_at: opened,
            last_seen_at: opened,
            resolved_at: None,
            resolution: None,
            quiet: false,
            before: None,
            accepted_at: None,
            accepted_by: None,
            snoozed_until: None,
            recovered_at: None,
            verified_at: None,
            resolved_by: None,
        })
        .collect();
    // The changes of the fixture's crawl: they pin its point on the trend.
    let changes = HashMap::from([(
        latest.id,
        ChangeCounts {
            rows: vec![
                (ChangeKind::AiBotBlocked, Severity::Critical, 1),
                (ChangeKind::StatusChanged, Severity::Warning, 1),
                (ChangeKind::NewUrl, Severity::Notice, 2),
                (ChangeKind::TitleChanged, Severity::Notice, 1),
            ],
        },
    )]);
    let times = ResponseBuckets {
        fast: i64::from(pages).saturating_sub(3),
        ok: 2,
        slow: 1,
        very_slow: 0,
    };
    let done = done_view(DoneInput {
        base: "/demo".to_owned(),
        demo: true,
        crawl: &latest,
        recent: &recent,
        changes: &changes,
        policy_pins: &HashMap::new(),
        thresholds: codoseo_checks::Thresholds::DEFAULT,
        times: &times,
        next: Some(now + Duration::days(6) + Duration::hours(21)),
        failed: None,
        // The fixture keeps counts, not pages.
        examples: &HashMap::new(),
        ai: Some(AiData {
            origin: DEMO_ORIGIN.to_owned(),
            report: report.clone(),
            intent: intent.clone(),
            open,
        }),
        now,
    });
    let remedies = finding_remedies(DEMO_ORIGIN, &report, &found);
    let fixes = found
        .into_iter()
        .zip(remedies)
        .enumerate()
        .map(|(i, (finding, remedy))| DemoFix {
            anchor: format!("fix-{}", i + 1),
            finding,
            remedy,
        })
        .collect();

    Ok(DemoPage {
        base: String::new(),
        body: AuditBody {
            base: "/demo".to_owned(),
            domain: DEMO_DOMAIN.to_owned(),
            done: Some(done),
            live: None,
            failure: None,
            demo: true,
        },
        fixes,
        umami: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixture_gives_a_full_audit_with_ai_findings_and_their_fixes() {
        let page = demo(OffsetDateTime::now_utc()).expect("the fixture loads");
        let done = page.body.done.as_ref().expect("a finished audit");
        assert!(!done.fixes.is_clear());
        assert!(done.trend.waiting.is_none());
        // The demo has no GEO score: those cards read "—", and the AI rows carry no points.
        let card = |k: &str| done.cards.iter().find(|c| c.key == k).unwrap();
        assert_eq!(card("geo").value, "—");
        assert_eq!(card("overall").value, "—");
        let ai: Vec<_> = done
            .fixes
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .filter(|r| r.kind == "ai")
            .collect();
        assert!(!ai.is_empty());
        assert!(ai.iter().all(|r| r.gain.is_none() && r.intend.is_none()));
        assert!(done.bots.is_some());
        assert!(!page.fixes.is_empty());
        assert!(page.fixes.iter().all(|f| !f.remedy.fix.is_empty()));
        // A sample on a reserved domain, never a crawled address.
        assert!(!FIXTURE.contains("127.0.0.1"));
        assert!(page.render().expect("renders").contains(DEMO_DOMAIN));
    }
}
