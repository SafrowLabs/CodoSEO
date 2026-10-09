mod common;

use codoseo_core::Url;
use codoseo_core::audit::{AUDIT_FORMAT_VERSION, Audit, AuditError};
use codoseo_core::check::CheckId;
use codoseo_core::crawl::SitemapSummary;
use codoseo_core::output::{CrawlOutput, LinkGraph, SiteSignals, StopReason};
use codoseo_core::page::PageRecord;
use codoseo_core::report::{CrawlReport, CrawlSummary, StatusCounts};
use codoseo_core::snapshot::Snapshot;
use common::sample_record;

fn output() -> CrawlOutput {
    CrawlOutput {
        origin: Url::parse("https://e.com/").unwrap(),
        pages: vec![sample_record()],
        links: LinkGraph::default(),
        robots: None,
        sitemap: SitemapSummary::default(),
        stop: StopReason::Completed,
        duration_ms: 1500,
        signals: SiteSignals::default(),
    }
}

fn audit() -> Audit {
    let out = output();
    Audit {
        format_version: AUDIT_FORMAT_VERSION,
        tool_version: "0.0.1".into(),
        created_at: 1_790_000_000,
        duration_ms: out.duration_ms,
        start_url: out.origin.clone(),
        report: CrawlReport {
            health_score: 97,
            checks_passed: 43,
            checks_total: 44,
            counts: vec![(CheckId::OgMissing, 1)],
            inlink_samples: Vec::new(),
            summary: CrawlSummary {
                pages: 1,
                indexable: 1,
                status: StatusCounts {
                    ok: 1,
                    redirect: 0,
                    client_error: 0,
                    server_error: 0,
                    failed: 0,
                    blocked: 0,
                },
                depth: vec![0, 1],
                no_depth: 0,
                avg_response_ms: 120,
            },
        },
        snapshot: Snapshot::from_output(&out),
        ai_access: None,
    }
}

#[test]
fn audit_round_trips_through_json() {
    let a = audit();
    let json = serde_json::to_vec(&a).unwrap();
    assert_eq!(Audit::from_json(&json).unwrap(), a);
    assert_eq!(a.snapshot.pages.len(), 1);
    assert_eq!(a.snapshot.origin, a.start_url);
}

#[test]
fn old_page_records_without_new_fields_still_load() {
    let mut v = serde_json::to_value(sample_record()).unwrap();
    let obj = v.as_object_mut().unwrap();
    for key in ["redirect_target", "outlinks_nofollow", "error"] {
        assert!(obj.remove(key).is_some(), "{key} should be serialised");
    }
    let p: PageRecord = serde_json::from_value(v).unwrap();
    assert_eq!(p, sample_record());
}

#[test]
fn other_format_versions_are_rejected() {
    let err = Audit::from_json(br#"{"format_version": 2, "anything": true}"#).unwrap_err();
    assert!(matches!(err, AuditError::Version(2)));
    assert!(matches!(
        Audit::from_json(b"not json"),
        Err(AuditError::Json(_))
    ));
}

#[test]
fn stop_reason_json_shape() {
    assert_eq!(
        serde_json::to_string(&StopReason::Unreachable("x".into())).unwrap(),
        r#"{"kind":"unreachable","reason":"x"}"#
    );
    assert_eq!(
        serde_json::to_string(&StopReason::PageLimit).unwrap(),
        r#"{"kind":"page_limit"}"#
    );
}

#[test]
fn stop_reason_predicates() {
    assert!(StopReason::Completed.is_complete());
    assert!(!StopReason::PageLimit.is_complete());
    for s in [
        StopReason::Completed,
        StopReason::PageLimit,
        StopReason::TimeLimit,
    ] {
        assert!(s.crawl_ran());
    }
    for s in [
        StopReason::Unreachable("x".into()),
        StopReason::Blocked("x".into()),
        StopReason::RobotsBlocked,
    ] {
        assert!(!s.crawl_ran());
    }
}

#[test]
fn link_graph_resolves_interned_anchors() {
    use codoseo_core::output::Edge;
    let g = LinkGraph {
        edges: vec![Edge {
            from: 0,
            to: 1,
            anchor: 1,
            nofollow: false,
        }],
        anchors: vec![String::new(), "Home".into()],
    };
    assert_eq!(g.anchor(&g.edges[0]), "Home");
}

#[test]
fn counts_for_unknown_checks_are_skipped() {
    // An audit written by a later version that knows more checks still loads.
    let a = audit();
    let mut v = serde_json::to_value(&a).unwrap();
    let counts = v["report"]["counts"].as_array_mut().unwrap();
    counts.insert(0, serde_json::json!(["future_check", 3]));
    counts.push(serde_json::json!(["another_new_one", 1]));
    let json = serde_json::to_vec(&v).unwrap();
    let loaded = Audit::from_json(&json).unwrap();
    assert_eq!(loaded.report.counts, vec![(CheckId::OgMissing, 1)]);
    assert_eq!(loaded, a);

    // Malformed entries are still errors.
    let mut bad = serde_json::to_value(&a).unwrap();
    bad["report"]["counts"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!(["og_missing", "three"]));
    assert!(Audit::from_json(&serde_json::to_vec(&bad).unwrap()).is_err());
}

#[test]
fn owned_snapshot_moves_the_pages_and_matches_the_borrowed_one() {
    let out = output();
    let borrowed = Snapshot::from_output(&out);
    let owned = Snapshot::from_output_owned(out);
    assert_eq!(owned, borrowed);
}
