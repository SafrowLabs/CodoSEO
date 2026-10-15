//! The GEO score of real access reports: findings under the owner's intent, the SEO checks'
//! counts and the llms.txt state, turned into check outcomes.

mod common;

use std::collections::HashSet;

use codoseo_core::check::{CheckId, Severity};
use codoseo_core::output::CrawlOutput;
use codoseo_geo::intent::Intent;
use codoseo_geo::report::{AccessReport, build_report, important_urls};
use codoseo_geo::score::{GeoCheck, GeoScore, Outcome, Status, assess, outcomes};
use common::*;

fn report_for(out: &CrawlOutput) -> AccessReport {
    let important = important_urls(&out.pages, &out.origin, &HashSet::new());
    build_report(out, &important)
}

fn intent(json: &str) -> Intent {
    serde_json::from_str(json).expect("intent")
}

fn status(s: &GeoScore, check: GeoCheck) -> Status {
    s.checks.iter().find(|c| c.check == check).unwrap().status
}

const BLOCK_OAI_SEARCH: &str = "User-agent: *\nContent-Signal: search=yes, ai-input=yes\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /\n";
/// Allows everything and declares Content Signals.
const CLEAN: &str =
    "User-agent: *\nContent-Signal: search=yes, ai-input=yes, ai-train=no\nAllow: /\n";

#[test]
fn a_clean_site_scores_100_and_skips_what_did_not_run() {
    let report = report_for(&with_robots(site(5, |p| p), 200, CLEAN));
    let s = assess(&report, &Intent::default(), &[], 5).unwrap();
    assert_eq!(s.score, 100);
    assert_eq!(s.checks_passed, s.checks_applicable);
    // No probes ran, and neither llms.txt nor the home page as Markdown was asked for.
    for check in [
        GeoCheck::FetchBlocked,
        GeoCheck::LlmsTxt,
        GeoCheck::MarkdownNegotiation,
    ] {
        assert_eq!(status(&s, check), Status::Skipped, "{check:?}");
    }
    assert_eq!(status(&s, GeoCheck::BotsBlocked), Status::Pass);
    assert_eq!(status(&s, GeoCheck::ContentSignals), Status::Pass);

    // Without a Content-Signal line, that check fails: a Notice's whole weight.
    let bare = report_for(&site(5, |p| p));
    let s = assess(&bare, &Intent::default(), &[], 5).unwrap();
    assert_eq!(status(&s, GeoCheck::ContentSignals), Status::Fail);
    assert_eq!(s.score, 99);
}

#[test]
fn a_blocked_search_bot_costs_points_unless_the_owner_chose_it() {
    let report = report_for(&with_robots(site(5, |p| p), 200, BLOCK_OAI_SEARCH));
    let s = assess(&report, &Intent::default(), &[], 5).unwrap();
    assert_eq!(status(&s, GeoCheck::BotsBlocked), Status::Fail);
    assert!(s.score < 100);
    assert_eq!(s.checks_passed + 1, s.checks_applicable);

    // Marked intended: no finding, no loss.
    let chosen = intent(r#"{"bots":{"OAI-SearchBot":"block"}}"#);
    let s = assess(&report, &chosen, &[], 5).unwrap();
    assert_eq!(s.score, 100);
    assert_eq!(status(&s, GeoCheck::BotsBlocked), Status::Pass);
}

#[test]
fn a_broken_robots_file_fails_once_and_skips_the_checks_that_rest_on_it() {
    let report = report_for(&with_robots(site(5, |p| p), 503, ""));
    let found = outcomes(&report, &Intent::default(), &[], 5);
    let of = |c: GeoCheck| found.iter().find(|(k, _)| *k == c).unwrap().1;
    assert!(matches!(
        of(GeoCheck::RobotsUnavailable),
        Outcome::Fail { share, .. } if share == 1.0
    ));
    for check in [
        GeoCheck::BotsBlocked,
        GeoCheck::BotsNotBlocked,
        GeoCheck::FetchBlocked,
        GeoCheck::ContentMissing,
    ] {
        assert_eq!(of(check), Outcome::Skipped, "{check:?}");
    }
}

#[test]
fn seo_counts_fill_the_recommended_tier() {
    let report = report_for(&site(5, |p| p));
    let counts = [(CheckId::TitleMissing, 2), (CheckId::SitemapMissing, 1)];
    let found = outcomes(&report, &Intent::default(), &counts, 5);
    let of = |c: GeoCheck| found.iter().find(|(k, _)| *k == c).unwrap().1;
    assert_eq!(
        of(GeoCheck::TitleMissing),
        Outcome::Fail {
            severity: Severity::Warning,
            share: 0.4
        }
    );
    // Site-wide: the whole share.
    assert_eq!(
        of(GeoCheck::Sitemap),
        Outcome::Fail {
            severity: Severity::Notice,
            share: 1.0
        }
    );
    assert_eq!(of(GeoCheck::JsonldInvalid), Outcome::Pass);
    // No pages, nothing for the SEO checks to say.
    let none = outcomes(&report, &Intent::default(), &counts, 0);
    assert!(
        none.iter()
            .any(|(c, o)| *c == GeoCheck::TitleMissing && *o == Outcome::Skipped)
    );
}
