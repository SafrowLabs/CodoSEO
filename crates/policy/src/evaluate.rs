//! The policy against a crawl: one result per rule (met, how many gaps, not checked, ...) and the
//! gaps themselves, the way the policy screen's comparison rail and the API show them.
//!
//! Only the AI rules are evaluated so far. They are not a second judge: the gaps are the AI
//! access findings ([`codoseo_geo::findings::findings`]) under the policy's intent, regrouped by
//! purpose, so the policy screen and the AI access screen always agree.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use codoseo_core::check::Severity;
use codoseo_core::page::PageRecord;
use codoseo_geo::eligibility::DirectiveSlug;
use codoseo_geo::findings::{Finding, FindingKind, evaluated_kinds, findings};
use codoseo_geo::intent::{Intent, Stance};
use codoseo_geo::registry::{Bot, Honours, Purpose, registry};
use codoseo_geo::report::{AccessReport, slug_from_str};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::SitePolicy;

/// Most URLs a rule result lists.
pub const MAX_RULE_URLS: usize = 20;
/// Past this many names a list of bots is counted instead of spelled out.
const MAX_NAMES: usize = 3;

/// Purposes in the order the screens list them.
pub const PURPOSES: [Purpose; 5] = [
    Purpose::Search,
    Purpose::UserFetch,
    Purpose::Agent,
    Purpose::Training,
    Purpose::Ads,
];

/// What a policy is evaluated against. Only `access` is read so far; the pages, the origin and
/// the starred pages are for the SEO rules.
#[derive(Debug, Clone, Copy, Default)]
pub struct PolicyInput<'a> {
    pub pages: &'a [PageRecord],
    pub origin: Option<&'a Url>,
    pub starred: Option<&'a HashSet<u64>>,
    /// The crawl's AI access report; `None` before the first crawl that made one.
    pub access: Option<&'a AccessReport>,
}

impl<'a> PolicyInput<'a> {
    /// Just an AI access report (or none).
    pub fn ai_only(access: Option<&'a AccessReport>) -> PolicyInput<'a> {
        PolicyInput {
            access,
            ..PolicyInput::default()
        }
    }
}

/// Which rule a result is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleId {
    Address,
    Index,
    Params,
    MustWork,
    AiSearch,
    AiUserFetch,
    AiAgent,
    AiTraining,
    AiAds,
    /// "Answer limits you use on purpose".
    AiAnswerLimits,
}

impl RuleId {
    pub fn for_purpose(p: Purpose) -> RuleId {
        match p {
            Purpose::Search => RuleId::AiSearch,
            Purpose::UserFetch => RuleId::AiUserFetch,
            Purpose::Agent => RuleId::AiAgent,
            Purpose::Training => RuleId::AiTraining,
            Purpose::Ads => RuleId::AiAds,
        }
    }

    pub fn kind(self) -> GapKind {
        match self {
            RuleId::Address | RuleId::Index | RuleId::Params | RuleId::MustWork => GapKind::Seo,
            _ => GapKind::Ai,
        }
    }
}

/// How the site measures up to one rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The site does what the rule says.
    Met,
    /// It doesn't, in `gaps` places.
    Gaps,
    /// Half-way: e.g. kept out by robots.txt but not by `noindex`.
    Partly,
    /// No rule, or nothing to compare with yet.
    NotChecked,
    /// The rule can't fully work: the bots it names don't follow robots.txt.
    LimitedEffect,
}

/// One rule's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleResult {
    pub id: RuleId,
    pub status: Status,
    /// Gaps found (0 unless `status` is `gaps`).
    pub gaps: u32,
    /// What the screens show: `GPTBot is blocked by robots.txt · CCBot gets in`.
    pub text: String,
    /// Up to [`MAX_RULE_URLS`] pages the result is about.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    Seo,
    Ai,
}

/// One place where the site disagrees with the policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    pub rule: RuleId,
    pub kind: GapKind,
    pub severity: Severity,
    /// `GPTBot gets in, policy says Block`.
    pub text: String,
    /// The bot token or directive it is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

/// The policy against one crawl.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyReport {
    pub rules: Vec<RuleResult>,
    pub gaps: Vec<Gap>,
    pub seo_gaps: u32,
    pub ai_gaps: u32,
}

impl PolicyReport {
    /// Rules the site meets.
    pub fn met(&self) -> usize {
        self.rules.iter().filter(|r| r.status == Status::Met).count()
    }

    pub fn rule(&self, id: RuleId) -> Option<&RuleResult> {
        self.rules.iter().find(|r| r.id == id)
    }
}

/// Evaluates the policy. Without an access report every AI rule is "not checked".
pub fn evaluate(policy: &SitePolicy, input: &PolicyInput) -> PolicyReport {
    let mut report = PolicyReport::default();
    evaluate_ai(policy, input.access, &mut report);
    report.seo_gaps = report
        .gaps
        .iter()
        .filter(|g| g.kind == GapKind::Seo)
        .count() as u32;
    report.ai_gaps = report
        .gaps
        .iter()
        .filter(|g| g.kind == GapKind::Ai)
        .count() as u32;
    report
}

fn names(tokens: &[String]) -> String {
    if tokens.len() > MAX_NAMES {
        format!(
            "{} and {} more",
            tokens[..MAX_NAMES].join(", "),
            tokens.len() - MAX_NAMES
        )
    } else {
        tokens.join(", ")
    }
}

/// How each bot fares with robots.txt and the fetch check, as the policy rail says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotNow {
    /// robots.txt lets it in (and the fetch check, if any, saw nothing wrong).
    Allowed,
    /// robots.txt keeps it out of the home page.
    Blocked,
    /// robots.txt lets it in but a fetch as this bot was blocked or challenged.
    Firewall,
    /// Nothing to say: no verdicts yet.
    Unknown,
}

/// The current state of `bot` in `report`.
pub fn bot_now(report: Option<&AccessReport>, bot: &Bot) -> BotNow {
    let Some(report) = report.filter(|r| r.has_verdicts()) else {
        return BotNow::Unknown;
    };
    match report.bot(&bot.token).and_then(|a| a.home_allowed) {
        Some(false) => BotNow::Blocked,
        Some(true) => {
            let firewall = report.probes.as_ref().is_some_and(|p| {
                p.urls.iter().any(|u| {
                    u.results.iter().any(|r| {
                        r.token.eq_ignore_ascii_case(&bot.token)
                            && matches!(
                                r.verdict,
                                codoseo_geo::probe::Verdict::Blocked { .. }
                                    | codoseo_geo::probe::Verdict::Challenged { .. }
                            )
                    })
                })
            });
            if firewall {
                BotNow::Firewall
            } else {
                BotNow::Allowed
            }
        }
        None => BotNow::Unknown,
    }
}

/// The important pages carrying each answer-limiting directive (across every engine's groups),
/// whether or not the owner accepted it. `noindex` is left out: the SEO checks report it.
pub fn directive_pages(report: Option<&AccessReport>) -> BTreeMap<DirectiveSlug, usize> {
    let mut by: BTreeMap<DirectiveSlug, BTreeSet<u16>> = BTreeMap::new();
    for e in report.map(|r| r.engines.as_slice()).unwrap_or_default() {
        for g in &e.groups {
            for c in &g.causes {
                if c.directive != DirectiveSlug::Noindex {
                    by.entry(c.directive).or_default().extend(&g.urls);
                }
            }
        }
    }
    by.into_iter().map(|(d, u)| (d, u.len())).collect()
}

fn evaluate_ai(policy: &SitePolicy, access: Option<&AccessReport>, out: &mut PolicyReport) {
    let intent = &policy.ai.intent;
    let judged = access.map(evaluated_kinds).unwrap_or_default();
    let found: Vec<Finding> = access.map(|r| findings(r, intent)).unwrap_or_default();
    let bots_judged = judged.contains(&FindingKind::BotsBlocked);
    let probes_judged = policy.ai.probes && judged.contains(&FindingKind::FetchBlocked);

    for purpose in PURPOSES {
        let (rule, gaps) = purpose_rule(
            purpose,
            intent,
            &found,
            bots_judged,
            probes_judged,
            access.is_some(),
        );
        out.rules.push(rule);
        out.gaps.extend(gaps);
    }

    // Answer limits.
    let id = RuleId::AiAnswerLimits;
    let restricted: Vec<&Finding> = found
        .iter()
        .filter(|f| f.kind == FindingKind::AnswersRestricted)
        .collect();
    let rule = if !judged.contains(&FindingKind::AnswersRestricted) {
        RuleResult {
            id,
            status: Status::NotChecked,
            gaps: 0,
            text: "Not checked yet: needs a crawl that read the key pages.".to_owned(),
            urls: Vec::new(),
        }
    } else if restricted.is_empty() {
        RuleResult {
            id,
            status: Status::Met,
            gaps: 0,
            text: "No page markup limits AI answers beyond what you accept.".to_owned(),
            urls: Vec::new(),
        }
    } else {
        let mut urls: Vec<String> = Vec::new();
        for f in &restricted {
            let slug = slug_from_str(&f.subject);
            out.gaps.push(Gap {
                rule: id,
                kind: GapKind::Ai,
                severity: f.severity,
                text: format!(
                    "{} on {} limits AI answers",
                    slug.map_or_else(|| f.subject.clone(), |d| label(d).to_owned()),
                    pages(f.evidence.urls.total as usize)
                ),
                subject: Some(f.subject.clone()),
            });
            for u in &f.evidence.urls.sample {
                if urls.len() < MAX_RULE_URLS && !urls.contains(u) {
                    urls.push(u.clone());
                }
            }
        }
        RuleResult {
            id,
            status: Status::Gaps,
            gaps: restricted.len() as u32,
            text: restricted
                .iter()
                .map(|f| slug_from_str(&f.subject).map_or(f.subject.clone(), |d| label(d).to_owned()))
                .collect::<Vec<_>>()
                .join(" · ")
                + " limit AI answers and aren't ticked",
            urls,
        }
    };
    out.rules.push(rule);
}

fn pages(n: usize) -> String {
    if n == 1 {
        "1 key page".to_owned()
    } else {
        format!("{n} key pages")
    }
}

/// A directive as written in markup: `max-snippet`.
pub fn label(d: DirectiveSlug) -> &'static str {
    match d {
        DirectiveSlug::Noindex => "noindex",
        DirectiveSlug::Nosnippet => "nosnippet",
        DirectiveSlug::MaxSnippet => "max-snippet",
        DirectiveSlug::Noarchive => "noarchive",
        DirectiveSlug::Nocache => "nocache",
        DirectiveSlug::DataNosnippet => "data-nosnippet",
    }
}

/// Why one bot is a gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    GetsIn,
    Robots,
    Firewall,
}

fn purpose_rule(
    purpose: Purpose,
    intent: &Intent,
    found: &[Finding],
    bots_judged: bool,
    probes_judged: bool,
    has_report: bool,
) -> (RuleResult, Vec<Gap>) {
    let id = RuleId::for_purpose(purpose);
    let bots: Vec<&Bot> = registry()
        .bots
        .iter()
        .filter(|b| b.purpose == purpose)
        .collect();
    let stance = intent.purpose_stance(purpose);
    let overrides = bots
        .iter()
        .filter(|b| intent.bot_override(&b.token).is_some())
        .count();
    let judged: Vec<&&Bot> = bots
        .iter()
        .filter(|b| intent.effective(b) != Stance::Any)
        .collect();
    let not_checked = |text: &str| RuleResult {
        id,
        status: Status::NotChecked,
        gaps: 0,
        text: text.to_owned(),
        urls: Vec::new(),
    };
    if judged.is_empty() || (stance == Stance::Any && overrides == 0) {
        return (
            not_checked("No preference: we won't flag these bots."),
            Vec::new(),
        );
    }
    if !has_report {
        return (
            not_checked("Not checked yet: it applies from the next crawl."),
            Vec::new(),
        );
    }
    if !bots_judged {
        return (
            not_checked("Not checked: the latest crawl could not read robots.txt."),
            Vec::new(),
        );
    }

    // The bots the findings name, in finding order, each once.
    let mut bad: Vec<(String, Why, Severity)> = Vec::new();
    let mut push = |token: &str, why: Why, severity: Severity| {
        if !bad.iter().any(|(t, ..)| t.eq_ignore_ascii_case(token)) {
            bad.push((token.to_owned(), why, severity));
        }
    };
    for f in found {
        match f.kind {
            FindingKind::BotsBlocked | FindingKind::BotsNotBlocked => {
                for b in f.evidence.bots.iter().filter(|b| b.purpose == purpose) {
                    let why = if f.kind == FindingKind::BotsBlocked {
                        Why::Robots
                    } else {
                        Why::GetsIn
                    };
                    push(&b.token, why, f.severity);
                }
            }
            FindingKind::FetchBlocked if probes_judged => {
                for p in f.evidence.probes.iter().filter(|p| p.purpose == purpose) {
                    push(&p.token, Why::Firewall, f.severity);
                }
            }
            _ => {}
        }
    }
    let gaps: Vec<Gap> = bad
        .iter()
        .map(|(token, why, severity)| Gap {
            rule: id,
            kind: GapKind::Ai,
            severity: *severity,
            text: match why {
                Why::GetsIn => format!("{token} gets in, policy says Block"),
                Why::Robots => format!("{token} is blocked by robots.txt, policy says Allow"),
                Why::Firewall => format!("{token} is blocked by your firewall"),
            },
            subject: Some(token.clone()),
        })
        .collect();

    // Blocking a bot that doesn't follow robots.txt only works at the firewall.
    let ignores: Vec<String> = judged
        .iter()
        .filter(|b| intent.effective(b) == Stance::Block && b.honours_robots == Honours::No)
        .map(|b| b.token.clone())
        .collect();
    let limited = if ignores.is_empty() {
        None
    } else {
        Some(format!(
            "{} {} follow robots.txt; block {} in your firewall.",
            names(&ignores),
            if ignores.len() == 1 { "doesn't" } else { "don't" },
            if ignores.len() == 1 { "it" } else { "them" }
        ))
    };

    let rule = if !bad.is_empty() {
        let mut text = bad
            .iter()
            .take(MAX_NAMES)
            .map(|(t, why, _)| match why {
                Why::GetsIn => format!("{t} gets in"),
                Why::Robots => format!("{t} is blocked by robots.txt"),
                Why::Firewall => format!("{t} is blocked by your firewall"),
            })
            .collect::<Vec<_>>()
            .join(" · ");
        if bad.len() > MAX_NAMES {
            text.push_str(&format!(" · and {} more", bad.len() - MAX_NAMES));
        }
        if let Some(l) = &limited {
            text.push_str(" · ");
            text.push_str(l);
        }
        RuleResult {
            id,
            status: Status::Gaps,
            gaps: bad.len() as u32,
            text,
            urls: Vec::new(),
        }
    } else if let Some(l) = limited {
        RuleResult {
            id,
            status: Status::LimitedEffect,
            gaps: 0,
            text: l,
            urls: Vec::new(),
        }
    } else {
        RuleResult {
            id,
            status: Status::Met,
            gaps: 0,
            text: if judged.len() == 1 {
                "It gets what you want.".to_owned()
            } else {
                format!("All {} get what you want.", judged.len())
            },
            urls: Vec::new(),
        }
    };
    (rule, gaps)
}
