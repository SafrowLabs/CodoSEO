mod common;

use codoseo_core::check::Severity;
use codoseo_geo::findings::{Evidence, Finding, FindingKind, Grade};
use codoseo_geo::incident::{OpenIncident, Transition, reconcile};

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
            escalated: false
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
            escalated: true
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
