mod common;

use codoseo_core::check::Severity;
use codoseo_geo::eligibility::{Effect, EngineId};
use codoseo_geo::findings::{BotEvidence, EngineEvidence, Evidence, Finding, FindingKind, Grade};
use codoseo_geo::incident::{OpenIncident, Transition, reconcile};
use codoseo_geo::robots::GroupMatch;
use codoseo_geo::{Honours, Purpose};

fn finding(kind: FindingKind, subject: &str, severity: Severity) -> Finding {
    Finding {
        kind,
        subject: subject.to_owned(),
        severity,
        grade: Grade::A,
        title: format!("{kind:?}/{subject}"),
        summary: String::new(),
        evidence: Evidence::default(),
        sources: Vec::new(),
    }
}

fn open(id: i64, kind: FindingKind, subject: &str, severity: Severity) -> OpenIncident {
    OpenIncident {
        id,
        kind,
        subject: subject.to_owned(),
        severity,
        members: Default::default(),
    }
}

const ALL: [FindingKind; 4] = [
    FindingKind::RobotsUnavailable,
    FindingKind::BotsBlocked,
    FindingKind::BotsNotBlocked,
    FindingKind::AnswersRestricted,
];

#[test]
fn a_new_finding_opens_an_incident() {
    let f = finding(FindingKind::BotsBlocked, "search", Severity::Critical);
    assert_eq!(
        reconcile(&[], std::slice::from_ref(&f), &ALL),
        [Transition::Opened { finding: f }]
    );
}

#[test]
fn a_persisting_finding_updates_and_flags_escalation() {
    let f = finding(FindingKind::BotsBlocked, "search", Severity::Critical);
    let same = reconcile(
        &[open(
            1,
            FindingKind::BotsBlocked,
            "search",
            Severity::Critical,
        )],
        std::slice::from_ref(&f),
        &ALL,
    );
    assert_eq!(
        same,
        [Transition::Updated {
            id: 1,
            finding: f.clone(),
            escalated: false,
            widened: false
        }]
    );
    let worse = reconcile(
        &[open(
            1,
            FindingKind::BotsBlocked,
            "search",
            Severity::Warning,
        )],
        std::slice::from_ref(&f),
        &ALL,
    );
    assert_eq!(
        worse,
        [Transition::Updated {
            id: 1,
            finding: f.clone(),
            escalated: true,
            widened: false
        }]
    );
    let better = reconcile(
        &[open(
            1,
            FindingKind::BotsBlocked,
            "search",
            Severity::Critical,
        )],
        &[finding(
            FindingKind::BotsBlocked,
            "search",
            Severity::Notice,
        )],
        &ALL,
    );
    assert!(matches!(
        better[0],
        Transition::Updated {
            escalated: false,
            ..
        }
    ));
}

/// A finding naming these bots.
fn naming(kind: FindingKind, tokens: &[&str]) -> Finding {
    let mut f = finding(kind, "search", Severity::Critical);
    f.evidence.bots = tokens
        .iter()
        .map(|t| BotEvidence {
            token: (*t).to_owned(),
            operator: String::new(),
            purpose: Purpose::Search,
            honours: Honours::Yes,
            urls_blocked: 1,
            urls_total: 1,
            home_blocked: true,
            rule: None,
            group: GroupMatch::Named,
        })
        .collect();
    f
}

#[test]
fn a_finding_that_names_a_new_bot_or_engine_has_widened() {
    let was = naming(FindingKind::BotsBlocked, &["OAI-SearchBot"]);
    let incident = OpenIncident {
        members: was.evidence.members(),
        ..open(3, FindingKind::BotsBlocked, "search", Severity::Critical)
    };
    let widened = |f: &Finding| match &reconcile(
        std::slice::from_ref(&incident),
        std::slice::from_ref(f),
        &ALL,
    )[0]
    {
        Transition::Updated {
            widened, escalated, ..
        } => {
            assert!(!escalated, "same severity");
            *widened
        }
        other => panic!("{other:?}"),
    };
    assert!(!widened(&was));
    // Case does not matter, and losing a bot is not widening.
    assert!(!widened(&naming(
        FindingKind::BotsBlocked,
        &["oai-searchbot"]
    )));
    assert!(!widened(&naming(FindingKind::BotsBlocked, &[])));
    assert!(widened(&naming(
        FindingKind::BotsBlocked,
        &["OAI-SearchBot", "PerplexityBot"]
    )));
    // One leaves and another joins: still a new bot.
    assert!(widened(&naming(
        FindingKind::BotsBlocked,
        &["PerplexityBot"]
    )));

    // Engines count the same way.
    let mut engines = finding(FindingKind::AnswersRestricted, "search", Severity::Critical);
    engines.evidence.engines = vec![EngineEvidence {
        engine: EngineId::Bing,
        name: "Bing".to_owned(),
        effect: Effect::Excluded,
        pages_excluded: 1,
        pages_limited: 0,
        site_pages: 1,
    }];
    let incident = OpenIncident {
        kind: FindingKind::AnswersRestricted,
        ..incident
    };
    let t = reconcile(std::slice::from_ref(&incident), &[engines], &ALL);
    assert!(matches!(t[0], Transition::Updated { widened: true, .. }));
}

#[test]
fn a_subject_is_part_of_the_key() {
    let f = finding(FindingKind::BotsBlocked, "training", Severity::Warning);
    let t = reconcile(
        &[open(
            1,
            FindingKind::BotsBlocked,
            "search",
            Severity::Critical,
        )],
        std::slice::from_ref(&f),
        &ALL,
    );
    assert_eq!(
        t,
        [
            Transition::Resolved {
                id: 1,
                kind: FindingKind::BotsBlocked,
                subject: "search".to_owned()
            },
            Transition::Opened { finding: f }
        ]
    );
}

#[test]
fn a_finding_that_is_gone_resolves_when_its_kind_was_evaluated() {
    let incident = open(7, FindingKind::BotsBlocked, "search", Severity::Critical);
    assert_eq!(
        reconcile(std::slice::from_ref(&incident), &[], &ALL),
        [Transition::Resolved {
            id: 7,
            kind: FindingKind::BotsBlocked,
            subject: "search".to_owned()
        }]
    );
    // Not evaluated (robots.txt was down, say): the incident stays open and untouched.
    assert!(reconcile(&[incident], &[], &[FindingKind::RobotsUnavailable]).is_empty());
}

#[test]
fn the_order_is_by_kind_then_subject() {
    let findings = [
        finding(
            FindingKind::AnswersRestricted,
            "nosnippet",
            Severity::Notice,
        ),
        finding(FindingKind::BotsBlocked, "training", Severity::Notice),
        finding(FindingKind::BotsBlocked, "search", Severity::Notice),
    ];
    let open = [open(
        3,
        FindingKind::RobotsUnavailable,
        "",
        Severity::Critical,
    )];
    let order: Vec<String> = reconcile(&open, &findings, &ALL)
        .iter()
        .map(|t| match t {
            Transition::Opened { finding } | Transition::Updated { finding, .. } => {
                format!("{}/{}", finding.kind.slug(), finding.subject)
            }
            Transition::Resolved { kind, subject, .. } => format!("{}/{subject}", kind.slug()),
        })
        .collect();
    assert_eq!(
        order,
        [
            "robots_unavailable/",
            "bots_blocked/search",
            "bots_blocked/training",
            "answers_restricted/nosnippet"
        ]
    );
}

// ---- what was true before an incident opened ------------------------------------------------

mod before {
    use std::collections::HashSet;

    use codoseo_core::output::CrawlOutput;
    use codoseo_geo::findings::{FindingKind, findings};
    use codoseo_geo::incident::{Before, before_of};
    use codoseo_geo::report::{AccessReport, build_report, important_urls};
    use codoseo_geo::robots::{GroupMatch, MatchedRule, RobotsAvailability};

    use super::common::*;
    use codoseo_geo::Intent;

    fn report_for(out: &CrawlOutput) -> AccessReport {
        let important = important_urls(&out.pages, &out.origin, &HashSet::new());
        build_report(out, &important)
    }

    fn pages() -> CrawlOutput {
        crawl(vec![page("/", 3), page("/private/a", 2), page("/p1", 1)])
    }

    const PRIVATE: &str =
        "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /private/\n";
    const EVERYTHING: &str = "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /\n";

    #[test]
    fn a_widened_block_remembers_the_rule_that_decided_each_page_before() {
        let prev = report_for(&with_robots(pages(), 200, PRIVATE));
        let now = report_for(&with_robots(pages(), 200, EVERYTHING));
        let f = findings(&now, &Intent::default());
        assert_eq!(f.len(), 1);
        let before = before_of(f[0].kind, &f[0].subject, &f[0].evidence, &prev).expect("before");
        assert_eq!(before.bots.len(), 1);
        let bot = &before.bots[0];
        assert_eq!(bot.token, "OAI-SearchBot");
        assert_eq!(bot.group, GroupMatch::Named);
        let private = MatchedRule {
            line: 5,
            allow: false,
            pattern: "/private/".to_owned(),
        };
        let by_path: Vec<(&str, bool, Option<&MatchedRule>)> = bot
            .paths
            .iter()
            .map(|p| (p.path.as_str(), p.allowed, p.rule.as_ref()))
            .collect();
        assert_eq!(
            by_path,
            [
                ("/", true, None),
                ("/private/a", false, Some(&private)),
                ("/p1", true, None)
            ]
        );
        assert_eq!(bot.disallows(), ["/private/"]);
        // Compact: what isn't known isn't written.
        let json = serde_json::to_string(&before).unwrap();
        assert!(!json.contains("null"), "{json}");
        let back: Before = serde_json::from_str(&json).unwrap();
        assert_eq!(back, before);
    }

    #[test]
    fn an_earlier_report_that_could_not_judge_the_kind_gives_nothing() {
        let prev = report_for(&with_robots(pages(), 503, ""));
        let now = report_for(&with_robots(pages(), 200, EVERYTHING));
        let f = &findings(&now, &Intent::default())[0];
        assert_eq!(before_of(f.kind, &f.subject, &f.evidence, &prev), None);
    }

    #[test]
    fn a_broken_robots_txt_remembers_the_status_it_had() {
        let prev = report_for(&with_robots(pages(), 200, PRIVATE));
        let now = report_for(&with_robots(pages(), 503, ""));
        let f = &findings(&now, &Intent::default())[0];
        assert_eq!(f.kind, FindingKind::RobotsUnavailable);
        let before = before_of(f.kind, &f.subject, &f.evidence, &prev).unwrap();
        assert_eq!(before.robots_status, Some(200));
        assert_eq!(before.robots, Some(RobotsAvailability::Ok));
    }

    #[test]
    fn a_header_directive_remembers_each_engines_controls() {
        let prev = report_for(&crawl(vec![page("/", 2), page("/pricing", 1)]));
        let now = report_for(&crawl(vec![
            page("/", 2),
            page("/pricing", 1).x_robots("nosnippet"),
        ]));
        let f = findings(&now, &Intent::default());
        let f = f
            .iter()
            .find(|f| f.kind == FindingKind::AnswersRestricted)
            .unwrap();
        let before = before_of(f.kind, &f.subject, &f.evidence, &prev).unwrap();
        assert!(!before.controls.is_empty());
        assert!(
            before
                .controls
                .iter()
                .all(|c| c.url.ends_with("/pricing") && c.causes.is_empty()),
            "{before:?}"
        );
    }
}

#[test]
fn only_kinds_without_an_intent_path_can_be_accepted() {
    use codoseo_geo::incident::acceptable;
    // The owner's intent ("Mark intended") is the way to make these a choice.
    for kind in [
        FindingKind::BotsBlocked,
        FindingKind::BotsNotBlocked,
        FindingKind::AnswersRestricted,
    ] {
        assert!(!acceptable(kind), "{kind:?}");
    }
    // A failing robots.txt is never fine: it can only be snoozed.
    assert!(!acceptable(FindingKind::RobotsUnavailable));
    for kind in [
        FindingKind::FetchBlocked,
        FindingKind::ContentMissing,
        FindingKind::LlmsTxtUnavailable,
    ] {
        assert!(acceptable(kind), "{kind:?}");
    }
}
