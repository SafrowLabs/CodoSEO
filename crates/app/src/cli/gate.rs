//! `crawl --fail-on` and `--baseline`: which results end the run with exit code 1 and, against a
//! baseline audit, which of them are new.
//!
//! A gate is an SEO severity (`critical`, `warning`: a failing check at that severity or worse), a
//! GEO severity (`geo:critical`, `geo:warning`, `geo:notice`), a GEO finding kind with an optional
//! subject (`bots_blocked`, `answers_restricted:nosnippet`) or `geo`, the default GEO set. GEO never
//! touches the health score; a gate only decides the exit code.
//!
//! Without a baseline every result counts. With one, only what is new counts: a page that now
//! fails a check it passed (or a page the baseline's complete crawl did not have), a change the
//! diff reports, and an AI access finding that is new, got more severe or names a bot or engine it
//! did not.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use codoseo_checks::{Scope, def};
use codoseo_core::audit::Audit;
use codoseo_core::change::Change;
use codoseo_core::check::{CheckId, Severity};
use codoseo_diff::{diff, key_pages};
use codoseo_geo::eligibility::DirectiveSlug;
use codoseo_geo::findings::{Finding, FindingKind};
use codoseo_geo::incident::{OpenIncident, Transition, reconcile};
use codoseo_geo::registry::Purpose;

use super::output::{severity_label, slug};

/// The answer-restriction directives the `geo` gate set covers: the ones that take a page's text
/// out of AI answers without also taking it out of search (`noindex` is an SEO check's business).
const GEO_DEFAULT_DIRECTIVES: [&str; 2] = ["nosnippet", "noarchive"];

/// One `--fail-on` condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// A failing SEO check at this severity or worse.
    Seo(Severity),
    /// An AI access finding at this severity or worse.
    GeoAt(Severity),
    /// `geo`: robots.txt failing, a bot you want blocked, `nosnippet` or `noarchive` on important
    /// pages, important pages that are an empty app shell.
    GeoDefault,
    /// One finding kind, and optionally one subject of it.
    Geo {
        kind: FindingKind,
        subject: Option<String>,
    },
}

impl fmt::Display for Gate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Gate::Seo(s) => f.write_str(&slug(s)),
            Gate::GeoAt(s) => write!(f, "geo:{}", slug(s)),
            Gate::GeoDefault => f.write_str("geo"),
            Gate::Geo {
                kind,
                subject: None,
            } => f.write_str(kind.slug()),
            Gate::Geo {
                kind,
                subject: Some(s),
            } => write!(f, "{}:{s}", kind.slug()),
        }
    }
}

const KINDS: [FindingKind; 9] = FindingKind::ALL;

fn purposes() -> Vec<String> {
    [
        Purpose::Search,
        Purpose::UserFetch,
        Purpose::Agent,
        Purpose::Training,
        Purpose::Ads,
    ]
    .iter()
    .map(slug)
    .collect()
}

fn directives() -> Vec<String> {
    [
        DirectiveSlug::Noindex,
        DirectiveSlug::Nosnippet,
        DirectiveSlug::MaxSnippet,
        DirectiveSlug::Noarchive,
        DirectiveSlug::Nocache,
        DirectiveSlug::DataNosnippet,
    ]
    .iter()
    .map(slug)
    .collect()
}

/// What `--fail-on` accepts, for the error message.
fn accepted() -> String {
    let kinds: Vec<&str> = KINDS.iter().map(|k| k.slug()).collect();
    format!(
        "critical, warning, geo, geo:critical, geo:warning, geo:notice, or an AI access finding kind ({}) optionally followed by :subject",
        kinds.join(", ")
    )
}

/// Parses one `--fail-on` value (the flag splits on commas first).
pub fn parse_gate(raw: &str) -> Result<Gate, String> {
    let value = raw.trim().to_ascii_lowercase();
    match value.as_str() {
        "critical" => return Ok(Gate::Seo(Severity::Critical)),
        "warning" => return Ok(Gate::Seo(Severity::Warning)),
        "geo" => return Ok(Gate::GeoDefault),
        _ => {}
    }
    let (head, subject) = match value.split_once(':') {
        Some((h, s)) => (h, Some(s)),
        None => (value.as_str(), None),
    };
    if head == "geo" {
        return subject
            .and_then(Severity::from_slug)
            .map(Gate::GeoAt)
            .ok_or_else(|| format!("`{raw}`: geo takes critical, warning or notice"));
    }
    let Some(kind) = FindingKind::from_slug(head) else {
        return Err(format!(
            "`{raw}` is not a fail-on value; use {}",
            accepted()
        ));
    };
    let Some(subject) = subject else {
        return Ok(Gate::Geo {
            kind,
            subject: None,
        });
    };
    let known = match kind {
        FindingKind::BotsBlocked | FindingKind::BotsNotBlocked => Some(purposes()),
        FindingKind::AnswersRestricted => Some(directives()),
        // Causes such as challenge_cloudflare or blocked_403: an open set.
        FindingKind::FetchBlocked => None,
        FindingKind::RobotsUnavailable
        | FindingKind::ContentMissing
        | FindingKind::LlmsTxtUnavailable
        | FindingKind::ContentSignalsMissing
        | FindingKind::MarkdownUnavailable => {
            return Err(format!("`{raw}`: {} has no subject", kind.slug()));
        }
    };
    if subject.is_empty()
        || known
            .as_ref()
            .is_some_and(|k| !k.iter().any(|s| s == subject))
    {
        let hint = known.map_or_else(String::new, |k| format!(" ({})", k.join(", ")));
        return Err(format!(
            "`{raw}`: unknown subject for {}{hint}",
            kind.slug()
        ));
    }
    Ok(Gate::Geo {
        kind,
        subject: Some(subject.to_owned()),
    })
}

impl Gate {
    fn seo_threshold(&self) -> Option<Severity> {
        match self {
            Gate::Seo(s) => Some(*s),
            _ => None,
        }
    }

    /// Whether an AI access finding trips this gate.
    pub fn matches(&self, f: &Finding) -> bool {
        match self {
            Gate::Seo(_) => false,
            // Severity orders most severe first, so "this bad or worse" is `<=`.
            Gate::GeoAt(s) => f.severity <= *s,
            Gate::GeoDefault => match f.kind {
                FindingKind::RobotsUnavailable
                | FindingKind::BotsBlocked
                | FindingKind::ContentMissing => true,
                FindingKind::AnswersRestricted => {
                    GEO_DEFAULT_DIRECTIVES.contains(&f.subject.as_str())
                }
                // The advisory kinds are nice-to-haves: only `geo:notice` or their own name
                // gates on them.
                FindingKind::BotsNotBlocked
                | FindingKind::FetchBlocked
                | FindingKind::LlmsTxtUnavailable
                | FindingKind::ContentSignalsMissing
                | FindingKind::MarkdownUnavailable => false,
            },
            Gate::Geo { kind, subject } => {
                f.kind == *kind && subject.as_ref().is_none_or(|s| *s == f.subject)
            }
        }
    }
}

/// How a result compares with the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Novelty {
    /// Not in the baseline, or there is no baseline.
    New,
    /// In the baseline, and worse now: more severe, or naming more bots or engines.
    Updated,
    /// In the baseline as it is now.
    Unchanged,
}

impl Novelty {
    /// SARIF's `baselineState`.
    pub fn sarif(self) -> &'static str {
        match self {
            Novelty::New => "new",
            Novelty::Updated => "updated",
            Novelty::Unchanged => "unchanged",
        }
    }

    fn counts(self) -> bool {
        self != Novelty::Unchanged
    }
}

/// One result that tripped a gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tripped {
    /// The check slug, `<kind>[:<subject>]` for an AI access finding, `change:<kind>` for a diff
    /// change.
    pub rule: String,
    pub severity: Severity,
    pub text: String,
}

/// The verdict of the gates over one audit, and what the renderers need to mark new results.
#[derive(Debug, Default)]
pub struct Verdict {
    /// A baseline was given.
    pub baseline: bool,
    /// The gates, as given.
    pub gates: Vec<Gate>,
    /// Page-check pairs (URL as a string) the baseline did not have; site-wide checks have no
    /// URL. Empty without a baseline.
    new_issues: HashSet<(CheckId, Option<String>)>,
    /// Each AI access finding's novelty by (kind, subject).
    geo: HashMap<(FindingKind, String), Novelty>,
    /// What changed since the baseline, as `codoseo diff` reports it.
    pub changes: Vec<Change>,
    /// The results that tripped a gate, most severe first.
    pub tripped: Vec<Tripped>,
}

impl Verdict {
    /// Exit code 1 is due.
    pub fn failed(&self) -> bool {
        !self.tripped.is_empty()
    }

    /// Whether one page's failing check is new against the baseline.
    pub fn issue(&self, id: CheckId, url: Option<&str>) -> Novelty {
        if !self.baseline || self.new_issues.contains(&(id, url.map(str::to_owned))) {
            Novelty::New
        } else {
            Novelty::Unchanged
        }
    }

    /// Whether an AI access finding is new against the baseline.
    pub fn finding(&self, f: &Finding) -> Novelty {
        self.geo
            .get(&(f.kind, f.subject.clone()))
            .copied()
            .unwrap_or(Novelty::New)
    }

    /// Whether the result tripped a gate, by its rule label.
    pub fn trips(&self, rule: &str) -> bool {
        self.tripped.iter().any(|t| t.rule == rule)
    }

    /// Whether a gate tripped on robots.txt failing. Such a crawl stops early, but the gate
    /// still answers it: exit code 1 rather than 2.
    pub fn robots_tripped(&self) -> bool {
        self.trips(FindingKind::RobotsUnavailable.slug())
    }
}

/// An AI access finding's rule label: its kind, and its subject when it has one.
pub fn finding_rule(f: &Finding) -> String {
    if f.subject.is_empty() {
        f.kind.slug().to_owned()
    } else {
        format!("{}:{}", f.kind.slug(), f.subject)
    }
}

/// The AI access findings saved in an audit; none for an audit from before they existed.
pub fn audit_findings(audit: &Audit) -> Vec<Finding> {
    audit
        .ai_access
        .as_ref()
        .and_then(|s| s.get("findings"))
        .and_then(|f| serde_json::from_value(f.clone()).ok())
        .unwrap_or_default()
}

/// Judges `audit` against the gates, and against `baseline` when there is one.
pub fn judge(audit: &Audit, baseline: Option<&Audit>, gates: &[Gate]) -> Verdict {
    let mut v = Verdict {
        baseline: baseline.is_some(),
        gates: gates.to_vec(),
        ..Verdict::default()
    };
    let findings = audit_findings(audit);
    if let Some(base) = baseline {
        v.new_issues = new_issues(audit, base);
        v.geo = geo_novelty(&findings, base);
        let key = key_pages(&base.snapshot, &HashSet::new());
        v.changes = diff(&base.snapshot, &audit.snapshot, &key);
    } else {
        v.geo = findings
            .iter()
            .map(|f| ((f.kind, f.subject.clone()), Novelty::New))
            .collect();
    }

    // The most lenient SEO threshold given decides.
    let seo = gates.iter().filter_map(Gate::seo_threshold).max();
    if let Some(threshold) = seo {
        v.tripped.extend(seo_tripped(audit, &v, threshold));
    }
    for f in &findings {
        if gates.iter().any(|g| g.matches(f)) && v.finding(f).counts() {
            let novelty = match (v.baseline, v.finding(f)) {
                (false, _) => "",
                (true, Novelty::Updated) => " (worse than the baseline)",
                (true, _) => " (new since the baseline)",
            };
            v.tripped.push(Tripped {
                rule: finding_rule(f),
                severity: f.severity,
                text: format!("{}{novelty}", f.title),
            });
        }
    }
    v.tripped.sort_by_key(|t| t.severity);
    v
}

/// Failing checks at `threshold` or worse: all of them, or with a baseline the new ones and the
/// changes the diff reports at that severity.
fn seo_tripped(audit: &Audit, v: &Verdict, threshold: Severity) -> Vec<Tripped> {
    let mut out = Vec::new();
    for (id, pages) in &audit.report.counts {
        let d = def(*id);
        if d.severity > threshold {
            continue;
        }
        let text = if !v.baseline {
            match (d.scope, pages) {
                (Scope::SiteWide, _) => format!("{} (site-wide)", d.title),
                (_, 1) => format!("{} (1 page)", d.title),
                (_, n) => format!("{} ({n} pages)", d.title),
            }
        } else {
            let new = v.new_issues.iter().filter(|(c, _)| c == id).count();
            match (d.scope, new) {
                (_, 0) => continue,
                (Scope::SiteWide, _) => format!("{} (site-wide, new since the baseline)", d.title),
                (_, 1) => format!("{} (1 page new since the baseline)", d.title),
                (_, n) => format!("{} ({n} pages new since the baseline)", d.title),
            }
        };
        out.push(Tripped {
            rule: id.slug().to_owned(),
            severity: d.severity,
            text,
        });
    }
    for c in v.changes.iter().filter(|c| c.severity <= threshold) {
        let at = c
            .url
            .as_ref()
            .map_or_else(String::new, |u| format!(" on {u}"));
        out.push(Tripped {
            rule: format!("change:{}", c.kind.slug()),
            severity: c.severity,
            text: format!(
                "{}{at}: {} → {}",
                c.kind.label(),
                or_none(&c.before),
                or_none(&c.after)
            ),
        });
    }
    out
}

fn or_none(s: &str) -> &str {
    if s.is_empty() { "none" } else { s }
}

/// Page-check pairs failing now that did not fail in the baseline. A page the baseline lacks
/// counts only when the baseline crawl was complete (else it may simply not have reached it).
fn new_issues(audit: &Audit, base: &Audit) -> HashSet<(CheckId, Option<String>)> {
    let before: HashMap<&str, u64> = base
        .snapshot
        .pages
        .iter()
        .map(|p| (p.url.as_str(), p.issues.0))
        .collect();
    let base_complete = base.snapshot.stop.is_complete();
    let mut out = HashSet::new();
    for page in &audit.snapshot.pages {
        let had = before.get(page.url.as_str()).copied();
        if had.is_none() && !base_complete {
            continue;
        }
        for id in page.issues.iter().filter_map(CheckId::from_bit) {
            if had.is_none_or(|bits| bits & (1u64 << id.bit()) == 0) {
                out.insert((id, Some(page.url.as_str().to_owned())));
            }
        }
    }
    let base_site_wide: BTreeSet<CheckId> = base.report.counts.iter().map(|(id, _)| *id).collect();
    for (id, _) in &audit.report.counts {
        if def(*id).scope == Scope::SiteWide && !base_site_wide.contains(id) {
            out.insert((*id, None));
        }
    }
    out
}

/// Each finding's novelty: the baseline's findings play the open incidents, so a finding is new,
/// updated (more severe, or naming a bot or engine it did not) or unchanged exactly as the web
/// app's incidents open, escalate, widen or carry on. A baseline without an AI access section
/// (an older audit) makes every finding new.
fn geo_novelty(findings: &[Finding], base: &Audit) -> HashMap<(FindingKind, String), Novelty> {
    let open: Vec<OpenIncident> = audit_findings(base)
        .into_iter()
        .enumerate()
        .map(|(i, f)| OpenIncident {
            id: i as i64,
            kind: f.kind,
            subject: f.subject,
            severity: f.severity,
            members: f.evidence.members(),
        })
        .collect();
    let mut out: BTreeMap<(FindingKind, String), Novelty> = BTreeMap::new();
    for t in reconcile(&open, findings, &[]) {
        match t {
            Transition::Opened { finding } => {
                out.insert((finding.kind, finding.subject), Novelty::New);
            }
            Transition::Updated {
                finding,
                escalated,
                widened,
                ..
            } => {
                let novelty = if escalated || widened {
                    Novelty::Updated
                } else {
                    Novelty::Unchanged
                };
                out.insert((finding.kind, finding.subject), novelty);
            }
            Transition::Resolved { .. } => {}
        }
    }
    out.into_iter().collect()
}

/// The lines `crawl` prints on stderr about the gates.
pub fn summary_lines(v: &Verdict) -> Vec<String> {
    if v.gates.is_empty() {
        return Vec::new();
    }
    let gates: Vec<String> = v.gates.iter().map(Gate::to_string).collect();
    let against = if v.baseline {
        " against the baseline"
    } else {
        ""
    };
    if v.tripped.is_empty() {
        return vec![format!("fail-on {}: passed{against}", gates.join(","))];
    }
    let mut lines = vec![format!(
        "fail-on {}: {} result{} failed{against}",
        gates.join(","),
        v.tripped.len(),
        if v.tripped.len() == 1 { "" } else { "s" }
    )];
    for t in &v.tripped {
        lines.push(format!(
            "  {:<8}  {}  {}",
            severity_label(t.severity),
            t.rule,
            super::output::clean(&t.text)
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_gate_form_and_rejects_the_rest() {
        assert_eq!(parse_gate("critical"), Ok(Gate::Seo(Severity::Critical)));
        assert_eq!(parse_gate(" Warning "), Ok(Gate::Seo(Severity::Warning)));
        assert_eq!(parse_gate("geo"), Ok(Gate::GeoDefault));
        assert_eq!(parse_gate("geo:notice"), Ok(Gate::GeoAt(Severity::Notice)));
        assert_eq!(
            parse_gate("bots_blocked:search"),
            Ok(Gate::Geo {
                kind: FindingKind::BotsBlocked,
                subject: Some("search".to_owned())
            })
        );
        assert_eq!(
            parse_gate("content_missing"),
            Ok(Gate::Geo {
                kind: FindingKind::ContentMissing,
                subject: None
            })
        );
        assert!(parse_gate("fetch_blocked:challenge_cloudflare").is_ok());
        // SEO notices never fail a build: there are too many of them to gate on.
        assert!(parse_gate("notice").is_err());
        assert!(parse_gate("geo:bogus").is_err());
        assert!(parse_gate("bots_blocked:robots").is_err());
        assert!(parse_gate("answers_restricted:nosnippet").is_ok());
        assert!(parse_gate("content_missing:x").is_err());
        assert!(parse_gate("bogus").unwrap_err().contains("bots_blocked"));
        for g in [
            "critical",
            "geo",
            "geo:warning",
            "bots_blocked",
            "answers_restricted:noarchive",
        ] {
            assert_eq!(parse_gate(g).unwrap().to_string(), g);
        }
    }

    fn finding(kind: FindingKind, subject: &str, severity: Severity) -> Finding {
        Finding {
            kind,
            subject: subject.to_owned(),
            severity,
            grade: codoseo_geo::findings::Grade::A,
            title: "t".to_owned(),
            summary: String::new(),
            evidence: Default::default(),
            sources: Vec::new(),
        }
    }

    #[test]
    fn the_geo_set_covers_the_ci_regressions_only() {
        let g = Gate::GeoDefault;
        assert!(g.matches(&finding(
            FindingKind::RobotsUnavailable,
            "",
            Severity::Critical
        )));
        assert!(g.matches(&finding(
            FindingKind::BotsBlocked,
            "search",
            Severity::Warning
        )));
        assert!(g.matches(&finding(
            FindingKind::AnswersRestricted,
            "nosnippet",
            Severity::Warning
        )));
        assert!(g.matches(&finding(
            FindingKind::AnswersRestricted,
            "noarchive",
            Severity::Notice
        )));
        assert!(g.matches(&finding(FindingKind::ContentMissing, "", Severity::Notice)));
        assert!(!g.matches(&finding(
            FindingKind::AnswersRestricted,
            "max_snippet",
            Severity::Warning
        )));
        assert!(!g.matches(&finding(
            FindingKind::LlmsTxtUnavailable,
            "",
            Severity::Warning
        )));
        assert!(!g.matches(&finding(
            FindingKind::FetchBlocked,
            "blocked_403",
            Severity::Warning
        )));
        let at = Gate::GeoAt(Severity::Warning);
        assert!(at.matches(&finding(
            FindingKind::LlmsTxtUnavailable,
            "",
            Severity::Warning
        )));
        assert!(!at.matches(&finding(FindingKind::ContentMissing, "", Severity::Notice)));
        assert!(!Gate::Seo(Severity::Notice).matches(&finding(
            FindingKind::BotsBlocked,
            "search",
            Severity::Critical
        )));
    }
}
