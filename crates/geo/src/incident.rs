//! The life of an AI-access incident: opened when a finding first appears, updated while it
//! persists, resolved when the next report that could see it no longer has it. When it opens,
//! [`before_of`] keeps what the newest earlier report held about the same thing, so a repair can
//! put it back.

use std::collections::{BTreeMap, BTreeSet};

use codoseo_core::check::Severity;
use serde::{Deserialize, Serialize};

use crate::eligibility::{Cause, Effect, EngineId};
use crate::findings::{Evidence, Finding, FindingKind, evaluated_kinds};
use crate::llms::LlmsState;
use crate::probe::Verdict;
use crate::report::{AccessReport, path_and_query, slug_from_str};
use crate::robots::{GroupMatch, MatchedRule, RobotsAvailability};

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

// ---- before ----------------------------------------------------------------------------------

/// What was true before an incident opened, as the newest earlier report that judged its kind
/// holds it. Only what reports keep is here: the rule that decided a page is known when it
/// blocked the page or decided the home page, so an allowed page other than the home page has no
/// rule. Stored as JSON on the incident; empty parts are left out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Before {
    /// `BotsBlocked` and `BotsNotBlocked`: each bot's verdicts on the home page and the pages the
    /// finding names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bots: Vec<BotBefore>,
    /// `RobotsUnavailable`: the robots.txt status (`None` inside when it was not fetched).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub robots_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub robots: Option<RobotsAvailability>,
    /// `LlmsTxtUnavailable`: what /llms.txt answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llms: Option<LlmsBefore>,
    /// `ContentMissing`: each named page's verdict and word count.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<ContentBefore>,
    /// `FetchBlocked`: what each named bot's probe got on each named page.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probes: Vec<ProbeBefore>,
    /// `AnswersRestricted`: each named engine's view of each named page.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<ControlsBefore>,
}

/// One bot's robots.txt verdicts before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotBefore {
    pub token: String,
    pub group: GroupMatch,
    pub paths: Vec<PathBefore>,
}

impl BotBefore {
    /// The patterns of the `Disallow` rules that blocked the pages, each once, in page order.
    pub fn disallows(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for r in self.paths.iter().filter_map(|p| p.rule.as_ref()) {
            if !r.allow && !out.contains(&r.pattern.as_str()) {
                out.push(&r.pattern);
            }
        }
        out
    }
}

/// One page's verdict for a bot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathBefore {
    pub path: String,
    pub allowed: bool,
    /// The deciding rule, when the report kept it (see [`Before`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<MatchedRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmsBefore {
    pub state: LlmsState,
    /// 0 when no response arrived.
    pub status: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentBefore {
    pub url: String,
    pub verdict: crate::content::ContentVerdict,
    pub words: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeBefore {
    pub token: String,
    pub url: String,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

/// One engine's view of one page: its effect and the directives behind it (none when eligible).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlsBefore {
    pub engine: EngineId,
    pub url: String,
    pub effect: Effect,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes: Vec<Cause>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub robots_blocked: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Before {
    fn is_empty(&self) -> bool {
        *self == Before::default()
    }
}

/// What `prev`, an earlier report, held about the finding of `kind` and `subject` with evidence
/// `ev`; `None` when `prev` could not judge the kind or holds nothing about the finding's bots,
/// pages or engines.
pub fn before_of(
    kind: FindingKind,
    subject: &str,
    ev: &Evidence,
    prev: &AccessReport,
) -> Option<Before> {
    if !evaluated_kinds(prev).contains(&kind) {
        return None;
    }
    let index = |url: &str| {
        prev.important
            .iter()
            .position(|u| u.url == url)
            .and_then(|i| u16::try_from(i).ok())
    };
    let mut before = Before::default();
    match kind {
        FindingKind::RobotsUnavailable => {
            before.robots_status = prev.robots.status;
            before.robots = Some(prev.robots.availability);
        }
        FindingKind::BotsBlocked | FindingKind::BotsNotBlocked => {
            // The home page, then the pages the finding names.
            let mut urls: Vec<&str> = prev
                .important
                .first()
                .map(|u| u.url.as_str())
                .into_iter()
                .collect();
            for u in &ev.urls.sample {
                if !urls.contains(&u.as_str()) {
                    urls.push(u);
                }
            }
            for b in &ev.bots {
                let Some(access) = prev.bot(&b.token) else {
                    continue;
                };
                let paths = urls
                    .iter()
                    .filter_map(|u| {
                        let i = index(u)?;
                        let blocked = access.blocked.iter().find(|x| x.url == i);
                        let rule = match blocked {
                            Some(x) => prev.rule_of(access, x).cloned(),
                            None if i == 0 => access.home_rule.clone(),
                            None => None,
                        };
                        Some(PathBefore {
                            path: path_and_query(u),
                            allowed: blocked.is_none(),
                            rule,
                        })
                    })
                    .collect();
                before.bots.push(BotBefore {
                    token: access.token.clone(),
                    group: access.group,
                    paths,
                });
            }
        }
        FindingKind::AnswersRestricted => {
            let directive = slug_from_str(subject);
            for e in &ev.engines {
                let Some(access) = prev.engine(e.engine) else {
                    continue;
                };
                for u in &ev.urls.sample {
                    let Some(i) = index(u) else { continue };
                    let group = access.groups.iter().find(|g| g.urls.contains(&i));
                    before.controls.push(ControlsBefore {
                        engine: e.engine,
                        url: u.clone(),
                        effect: group.map_or(Effect::Eligible, |g| g.effect),
                        causes: group
                            .map(|g| {
                                g.causes
                                    .iter()
                                    .filter(|c| directive.is_none_or(|d| c.directive == d))
                                    .cloned()
                                    .collect()
                            })
                            .unwrap_or_default(),
                        robots_blocked: group.is_some_and(|g| g.robots_blocked),
                    });
                }
            }
        }
        FindingKind::FetchBlocked => {
            let Some(probes) = &prev.probes else {
                return None;
            };
            for p in &ev.probes {
                for u in &p.urls {
                    let Some(i) = index(u) else { continue };
                    let Some(result) = probes
                        .urls
                        .iter()
                        .filter(|x| x.url == i)
                        .flat_map(|x| &x.results)
                        .find(|r| r.token.eq_ignore_ascii_case(&p.token))
                    else {
                        continue;
                    };
                    before.probes.push(ProbeBefore {
                        token: p.token.clone(),
                        url: u.clone(),
                        verdict: result.verdict.clone(),
                        status: result.fetch.as_ref().and_then(|f| f.status),
                    });
                }
            }
        }
        FindingKind::ContentMissing => {
            let Some(content) = &prev.content else {
                return None;
            };
            for c in &ev.content {
                let Some(i) = index(&c.url) else { continue };
                if let Some(page) = content.pages.iter().find(|p| p.url == i) {
                    before.content.push(ContentBefore {
                        url: c.url.clone(),
                        verdict: page.verdict,
                        words: page.words,
                    });
                }
            }
        }
        FindingKind::LlmsTxtUnavailable => {
            let llms = prev.llms_txt.as_ref()?;
            before.llms = Some(LlmsBefore {
                state: llms.state,
                status: llms.status,
            });
        }
    }
    (!before.is_empty()).then_some(before)
}
