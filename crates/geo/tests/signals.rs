//! Content Signals read from robots.txt, and the Markdown for agents check, end to end: the
//! parsed lines, the findings, the GEO score and the remedies.

mod common;

use std::collections::HashSet;

use codoseo_core::check::Severity;
use codoseo_core::output::{AlternateSource, CrawlOutput, MarkdownFetch, MarkdownProbe};
use codoseo_geo::findings::{FindingKind, evaluated_kinds, findings};
use codoseo_geo::intent::Intent;
use codoseo_geo::ledger;
use codoseo_geo::markdown::MarkdownState;
use codoseo_geo::remedy::{RemedyContext, agent_prompt, copy_fix, now_text, remedy};
use codoseo_geo::report::{AccessReport, build_report, important_urls};
use codoseo_geo::robots::RobotsTxt;
use codoseo_geo::score::{GeoCheck, GeoScore, Status, assess};
use codoseo_geo::signals::{SignalsSummary, summarize, training_hint};
use common::*;

fn report_for(out: &CrawlOutput) -> AccessReport {
    let important = important_urls(&out.pages, &out.origin, &HashSet::new());
    build_report(out, &important)
}

fn read(robots: &str) -> SignalsSummary {
    summarize(RobotsTxt::parse(robots.as_bytes()).content_signals())
}

fn intent(json: &str) -> Intent {
    serde_json::from_str(json).expect("intent")
}

fn status(s: &GeoScore, check: GeoCheck) -> Status {
    s.checks.iter().find(|c| c.check == check).unwrap().status
}

fn remedy_of(report: &AccessReport, kind: FindingKind) -> codoseo_geo::remedy::Remedy {
    let f = findings(report, &Intent::default())
        .into_iter()
        .find(|f| f.kind == kind)
        .expect("the finding");
    let ctx = RemedyContext {
        origin: ORIGIN,
        report: Some(report),
        before: None,
        lag: ledger::lag(f.kind, &f.evidence),
    };
    remedy(f.kind, &f.subject, &f.evidence, &ctx)
}

// ---- the parser ------------------------------------------------------------------------------

#[test]
fn reads_the_signals_in_any_case_with_comments() {
    let s = read(
        "# Content signals\nUser-agent: *\nCONTENT-SIGNAL: Search=YES, ai-input = yes, ai-train=no # our choice\nAllow: /\n",
    );
    assert!(s.declared());
    assert_eq!(s.lines, 1);
    assert_eq!(
        s.line_text().as_deref(),
        Some("search=yes, ai-input=yes, ai-train=no")
    );
    assert_eq!(s.for_everyone("ai-train"), Some(false));
    assert!(s.malformed.is_empty(), "{:?}", s.malformed);
}

#[test]
fn a_line_before_any_group_applies_to_everyone_and_groups_are_kept_apart() {
    let s = read(
        "Content-Signal: ai-train=no\n\nUser-agent: GPTBot\nContent-Signal: ai-input=no\nDisallow: /\n\nUser-agent: *\nContent-Signal: search=yes\n",
    );
    assert_eq!(s.lines, 3);
    assert_eq!(s.for_everyone("ai-train"), Some(false));
    assert_eq!(s.for_everyone("search"), Some(true));
    // The GPTBot line is not for everyone.
    assert_eq!(s.for_everyone("ai-input"), None);
    let gpt = s.signals.iter().find(|x| x.key == "ai-input").unwrap();
    assert_eq!(gpt.agents.len(), 1);
    assert!(
        gpt.agents[0].eq_ignore_ascii_case("gptbot"),
        "{:?}",
        gpt.agents
    );
    assert_eq!(s.line_text().as_deref(), Some("ai-train=no, search=yes"));
}

#[test]
fn malformed_values_are_listed_and_do_not_count() {
    let s = read(
        "User-agent: *\nContent-Signal: ai-train=maybe, tracking=no\nContent-Signal: nonsense\n",
    );
    assert!(!s.declared());
    assert_eq!(s.lines, 2);
    assert_eq!(
        s.malformed,
        vec![
            "line 2: ai-train=maybe (the value must be yes or no)".to_owned(),
            "line 2: tracking=no (not a signal the policy defines)".to_owned(),
            "line 3: no key=value pair".to_owned(),
        ]
    );
    // One valid pair among bad ones is enough.
    let s = read("User-agent: *\nContent-Signal: ai-train=perhaps, search=yes\n");
    assert!(s.declared());
    assert_eq!(s.malformed.len(), 1);
}

#[test]
fn the_training_hint_follows_the_intent() {
    let no_training = read("User-agent: *\nContent-Signal: ai-train=no\n");
    assert_eq!(training_hint(&no_training, &Intent::default()), None);
    let wants_training = intent(r#"{"purposes":{"training":"allow"}}"#);
    assert!(
        training_hint(&no_training, &wants_training)
            .unwrap()
            .contains("ai-train=no")
    );
    let yes_training = read("User-agent: *\nContent-Signal: ai-train=yes\n");
    let blocks_training = intent(r#"{"purposes":{"training":"block"}}"#);
    assert!(
        training_hint(&yes_training, &blocks_training)
            .unwrap()
            .contains("ai-train=yes")
    );
    assert_eq!(training_hint(&yes_training, &wants_training), None);
}

// ---- the Content Signals finding -------------------------------------------------------------

#[test]
fn no_content_signal_is_an_advisory_notice_with_a_fix() {
    let report = report_for(&site(3, |p| p));
    assert!(evaluated_kinds(&report).contains(&FindingKind::ContentSignalsMissing));
    let f = findings(&report, &Intent::default())
        .into_iter()
        .find(|f| f.kind == FindingKind::ContentSignalsMissing)
        .expect("the finding");
    assert_eq!(f.severity, Severity::Notice);
    assert!(f.kind.is_advisory());
    assert_eq!(f.title, "robots.txt declares no Content Signals");
    assert!(
        f.summary.contains("Content-Signal: search=yes"),
        "{}",
        f.summary
    );
    assert_eq!(f.evidence.signal_lines, Some(0));
    assert!(f.sources.iter().any(|s| s.contains("contentsignals.org")));
    assert_eq!(
        now_text(f.kind, &f.evidence),
        "robots.txt has no Content-Signal line"
    );

    let r = remedy_of(&report, FindingKind::ContentSignalsMissing);
    assert_eq!(
        copy_fix(&r),
        "User-agent: *\nContent-Signal: search=yes, ai-input=yes, ai-train=no"
    );
    assert!(
        r.steps
            .iter()
            .any(|s| s.contains("https://contentsignals.org/"))
    );
    let prompt = agent_prompt(&r, &Default::default());
    assert!(prompt.contains("Content-Signal"), "{prompt}");
}

#[test]
fn malformed_lines_are_in_the_evidence_and_valid_ones_pass() {
    let bad = report_for(&with_robots(
        site(3, |p| p),
        200,
        "User-agent: *\nContent-Signal: ai-train=maybe\nAllow: /\n",
    ));
    let f = findings(&bad, &Intent::default())
        .into_iter()
        .find(|f| f.kind == FindingKind::ContentSignalsMissing)
        .expect("still missing");
    assert_eq!(
        f.title,
        "robots.txt Content-Signal lines declare no valid signal"
    );
    assert_eq!(f.evidence.signal_lines, Some(1));
    assert_eq!(f.evidence.malformed.len(), 1);
    let r = remedy_of(&bad, FindingKind::ContentSignalsMissing);
    assert!(r.steps.iter().any(|s| s.contains("ai-train=maybe")));

    let good = report_for(&with_robots(
        site(3, |p| p),
        200,
        "User-agent: *\nContent-Signal: search=yes, ai-input=yes, ai-train=no\nAllow: /\n",
    ));
    assert!(
        findings(&good, &Intent::default())
            .iter()
            .all(|f| f.kind != FindingKind::ContentSignalsMissing)
    );
    let s = assess(&good, &Intent::default(), &[], 3).unwrap();
    assert_eq!(status(&s, GeoCheck::ContentSignals), Status::Pass);
}

#[test]
fn a_missing_robots_file_fails_and_a_broken_one_skips() {
    let missing = report_for(&with_robots(site(3, |p| p), 404, ""));
    let f = findings(&missing, &Intent::default())
        .into_iter()
        .find(|f| f.kind == FindingKind::ContentSignalsMissing)
        .expect("no robots.txt, no signals");
    assert_eq!(f.title, "No robots.txt, so no Content Signals");

    let broken = report_for(&with_robots(site(3, |p| p), 503, ""));
    assert!(!evaluated_kinds(&broken).contains(&FindingKind::ContentSignalsMissing));
    let s = assess(&broken, &Intent::default(), &[], 3).unwrap();
    assert_eq!(status(&s, GeoCheck::ContentSignals), Status::Skipped);
}

// ---- Markdown for agents ---------------------------------------------------------------------

fn fetch(status: u16, content_type: &str, body_bytes: u64) -> MarkdownFetch {
    MarkdownFetch {
        url: "https://example.com/".to_owned(),
        status,
        error: None,
        content_type: Some(content_type.to_owned()),
        body_bytes,
    }
}

fn with_markdown(probe: MarkdownProbe) -> AccessReport {
    let mut out = site(3, |p| p);
    out.signals.markdown = Some(probe);
    report_for(&out)
}

#[test]
fn markdown_served_or_advertised_passes() {
    let served = with_markdown(MarkdownProbe {
        negotiated: fetch(200, "text/markdown; charset=utf-8", 120),
        ..MarkdownProbe::default()
    });
    assert!(served.declared.home_read, "the fixture's home page is HTML");
    assert_eq!(
        served.markdown.as_ref().unwrap().state,
        MarkdownState::Negotiated
    );
    assert!(evaluated_kinds(&served).contains(&FindingKind::MarkdownUnavailable));
    assert!(
        findings(&served, &Intent::default())
            .iter()
            .all(|f| f.kind != FindingKind::MarkdownUnavailable)
    );

    let alternate = with_markdown(MarkdownProbe {
        negotiated: fetch(200, "text/html", 5000),
        alternate: Some(("/index.md".to_owned(), AlternateSource::LinkTag)),
        alternate_fetch: Some(fetch(200, "text/markdown", 80)),
    });
    assert_eq!(
        alternate.markdown.as_ref().unwrap().state,
        MarkdownState::Alternate
    );
    let s = assess(&alternate, &Intent::default(), &[], 3).unwrap();
    assert_eq!(status(&s, GeoCheck::MarkdownNegotiation), Status::Pass);
}

#[test]
fn html_only_is_an_advisory_notice_saying_what_came_back() {
    for (got, said) in [
        (
            fetch(200, "text/html; charset=utf-8", 5000),
            "HTTP 200 text/html",
        ),
        (fetch(406, "text/html", 10), "HTTP 406 text/html"),
        (fetch(500, "text/plain", 10), "HTTP 500 text/plain"),
        (
            fetch(200, "text/markdown", 0),
            "HTTP 200 text/markdown (empty)",
        ),
    ] {
        let report = with_markdown(MarkdownProbe {
            negotiated: got,
            ..MarkdownProbe::default()
        });
        let f = findings(&report, &Intent::default())
            .into_iter()
            .find(|f| f.kind == FindingKind::MarkdownUnavailable)
            .expect("not served");
        assert_eq!(f.severity, Severity::Notice);
        assert!(f.summary.contains(said), "{}", f.summary);
        assert!(now_text(f.kind, &f.evidence).contains(said));
        let r = remedy_of(&report, FindingKind::MarkdownUnavailable);
        assert!(r.steps.iter().any(|s| s.contains("Markdown for Agents")));
        assert!(r.steps.iter().any(|s| s.contains("llms.txt")));
        assert!(r.verify[0].contains("Accept: text/markdown"));
    }
}

#[test]
fn markdown_is_skipped_without_an_answer_or_in_older_reports() {
    let unreachable = with_markdown(MarkdownProbe {
        negotiated: MarkdownFetch {
            url: "https://example.com/".to_owned(),
            status: 0,
            error: Some("request timed out".to_owned()),
            content_type: None,
            body_bytes: 0,
        },
        ..MarkdownProbe::default()
    });
    assert!(!evaluated_kinds(&unreachable).contains(&FindingKind::MarkdownUnavailable));
    // A report from before the check: no `markdown` at all.
    let old = report_for(&site(3, |p| p));
    assert_eq!(old.markdown, None);
    let s = assess(&old, &Intent::default(), &[], 3).unwrap();
    assert_eq!(status(&s, GeoCheck::MarkdownNegotiation), Status::Skipped);
    // A stored report without the field still reads.
    let mut v = serde_json::to_value(&old).unwrap();
    v.as_object_mut().unwrap().remove("markdown");
    let back: AccessReport = serde_json::from_value(v).unwrap();
    assert_eq!(back.markdown, None);
}

#[test]
fn both_checks_count_in_the_geo_score_once_judged() {
    let report = with_markdown(MarkdownProbe {
        negotiated: fetch(200, "text/html", 5000),
        ..MarkdownProbe::default()
    });
    let s = assess(&report, &Intent::default(), &[], 3).unwrap();
    for check in [GeoCheck::ContentSignals, GeoCheck::MarkdownNegotiation] {
        let line = s.checks.iter().find(|c| c.check == check).unwrap();
        assert_eq!(line.status, Status::Fail, "{check:?}");
        // A Notice on the whole site costs its whole weight. llms.txt was not fetched (skipped),
        // so the Recommended tier's 30 points are split over 22: 30 x 1/22 each.
        assert!((line.points_lost - 30.0 / 22.0).abs() < 1e-9, "{line:?}");
    }
    assert_eq!(s.score, 97);
}

// ---- recheck ---------------------------------------------------------------------------------

#[test]
fn a_recheck_reads_robots_txt_or_asks_for_markdown_again() {
    use codoseo_core::crawl::RobotsFile;
    use codoseo_geo::recheck::{Observations, overlay, plan};

    let stored = with_markdown(MarkdownProbe {
        negotiated: fetch(200, "text/html", 5000),
        ..MarkdownProbe::default()
    });
    let found = findings(&stored, &Intent::default());
    let of = |k: FindingKind| found.iter().find(|f| f.kind == k).unwrap();

    // Content Signals: robots.txt only, and its lines come with it.
    let f = of(FindingKind::ContentSignalsMissing);
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);
    assert!(p.robots && !p.markdown && !p.llms && p.pages.is_empty());
    let obs = Observations {
        robots: Some(RobotsFile {
            status: 200,
            body: "User-agent: *\nContent-Signal: ai-train=no\nAllow: /\n".to_owned(),
            hash: 1,
        }),
        ..Observations::default()
    };
    let (now, kinds) = overlay(&stored, &obs, &p);
    assert_eq!(kinds, vec![FindingKind::ContentSignalsMissing]);
    assert!(
        findings(&now, &Intent::default())
            .iter()
            .all(|f| f.kind != FindingKind::ContentSignalsMissing)
    );

    // Markdown: the home page again; an answer judges it, no answer judges nothing.
    let f = of(FindingKind::MarkdownUnavailable);
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);
    assert!(p.markdown && !p.robots && p.pages.is_empty());
    let served = Observations {
        markdown: Some(MarkdownProbe {
            negotiated: fetch(200, "text/markdown", 50),
            ..MarkdownProbe::default()
        }),
        ..Observations::default()
    };
    let (now, kinds) = overlay(&stored, &served, &p);
    assert_eq!(kinds, vec![FindingKind::MarkdownUnavailable]);
    assert_eq!(now.markdown.unwrap().state, MarkdownState::Negotiated);
    let silent = Observations {
        markdown: Some(MarkdownProbe {
            negotiated: MarkdownFetch {
                status: 0,
                error: Some("request timed out".to_owned()),
                ..fetch(0, "", 0)
            },
            ..MarkdownProbe::default()
        }),
        ..Observations::default()
    };
    let (_, kinds) = overlay(&stored, &silent, &p);
    assert!(kinds.is_empty(), "inconclusive");
}
