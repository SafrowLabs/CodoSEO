//! The life of an AI-access incident: opened when a finding first appears, updated while it
//! persists, resolved when the next report that could see it no longer has it.

use std::collections::{BTreeMap, BTreeSet};

use codoseo_core::check::Severity;

use crate::findings::{Finding, FindingKind};

/// What the store knows about an unresolved incident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenIncident {
    pub id: i64,
    pub kind: FindingKind,
    pub subject: String,
    pub severity: Severity,
    /// The bots or engines its finding named when last written ([`Evidence::members`]).
    ///
    /// [`Evidence::members`]: crate::findings::Evidence::members
    pub members: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    /// A finding with no open incident.
    Opened { finding: Finding },
    /// The incident is still there; `escalated` when it got worse, `widened` when it now names
    /// a bot or engine it did not (another search bot blocked, another engine restricted).
    Updated {
        id: i64,
        finding: Finding,
        escalated: bool,
        widened: bool,
    },
    /// The report evaluated this kind and the finding is gone.
    Resolved {
        id: i64,
        kind: FindingKind,
        subject: String,
    },
}

impl Transition {
    fn key(&self) -> (FindingKind, &str) {
        match self {
            Transition::Opened { finding } | Transition::Updated { finding, .. } => {
                (finding.kind, &finding.subject)
            }
            Transition::Resolved { kind, subject, .. } => (*kind, subject),
        }
    }
}

/// Matches findings to open incidents by (kind, subject). Incidents whose kind was not evaluated
/// are left alone. The result is ordered by (kind, subject).
pub fn reconcile(
    open: &[OpenIncident],
    findings: &[Finding],
    evaluated: &[FindingKind],
) -> Vec<Transition> {
    let by_key: BTreeMap<(FindingKind, &str), &OpenIncident> = open
        .iter()
        .map(|i| ((i.kind, i.subject.as_str()), i))
        .collect();
    let mut out = Vec::new();
    for finding in findings {
        out.push(
            match by_key.get(&(finding.kind, finding.subject.as_str())) {
                None => Transition::Opened {
                    finding: finding.clone(),
                },
                Some(incident) => Transition::Updated {
                    id: incident.id,
                    finding: finding.clone(),
                    // Critical sorts before Warning before Notice.
                    escalated: finding.severity < incident.severity,
                    widened: !finding.evidence.members().is_subset(&incident.members),
                },
            },
        );
    }
    for incident in open {
        let found = findings
            .iter()
            .any(|f| f.kind == incident.kind && f.subject == incident.subject);
        if !found && evaluated.contains(&incident.kind) {
            out.push(Transition::Resolved {
                id: incident.id,
                kind: incident.kind,
                subject: incident.subject.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.key().cmp(&b.key()));
    out
}
