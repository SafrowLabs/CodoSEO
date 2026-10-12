//! What the access report means for the site owner, in the owner's terms.
//!
//! Four kinds of finding are grade A (documented by the operator): robots.txt can't be read,
//! bots the owner wants are blocked, bots the owner wants blocked still get in, and page markup
//! that takes pages out of an engine's AI answers. A fifth, grade B, comes from the synthetic fetch
//! probes: the site's CDN or firewall blocks, challenges or diverts bots it lets in by robots.txt.
//! A sixth, grade A-, reads the important pages as served: an empty app shell is all that AI
//! crawlers which don't run JavaScript get.
//! A seventh, grade A, is informational: an llms.txt the site served before has stopped answering.
//! It is never Critical, and llms.txt never counts toward the health score.
//! Severity follows the owner's [`Intent`]: a blocked training bot is not a problem unless the
//! owner said they want it to crawl.

use std::collections::{BTreeMap, BTreeSet};

use codoseo_core::check::Severity;
use serde::{Deserialize, Serialize};

use crate::content::{
    ContentVerdict, MEASURED_BOTS, NON_RENDERING_BOTS, SHELL_MAX_WORDS, ShellSignal,
};
use crate::eligibility::{Cause, CauseSource, DirectiveSlug, Effect, EngineId, engines};
use crate::intent::{Intent, Stance};
use crate::ledger::{finding_sources, for_kind};
use crate::llms::LlmsState;
use crate::probe::{ProbeFetch, ProbeReport, Vendor, Verdict};
use crate::registry::{Bot, Honours, Purpose, registry};
use crate::report::{AccessReport, Reason, path_and_query, slug_str};
use crate::robots::{GroupMatch, MatchedRule, RobotsAvailability};

/// How many URLs the evidence lists as a sample.
const SAMPLE_URLS: usize = 10;
/// Past this many names a list of bots is counted instead of spelled out.
const MAX_NAMES: usize = 3;
/// Google's account of what a failing robots.txt does, cited by `RobotsUnavailable`.
pub const ROBOTS_DOC: &str =
    "https://developers.google.com/search/docs/crawling-indexing/robots/robots_txt";
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    RobotsUnavailable,
    BotsBlocked,
    BotsNotBlocked,
    AnswersRestricted,
    /// Synthetic: a probe with a bot's user agent was blocked, challenged or diverted.
    FetchBlocked,
    /// Important pages are an empty app shell in the HTML as served.
    ContentMissing,
    /// An llms.txt the site had adopted answers 5xx or 429, or not at all.
    LlmsTxtUnavailable,
}

impl FindingKind {
    pub fn slug(self) -> &'static str {
        match self {
            FindingKind::RobotsUnavailable => "robots_unavailable",
            FindingKind::BotsBlocked => "bots_blocked",
            FindingKind::BotsNotBlocked => "bots_not_blocked",
            FindingKind::AnswersRestricted => "answers_restricted",
            FindingKind::FetchBlocked => "fetch_blocked",
            FindingKind::ContentMissing => "content_missing",
            FindingKind::LlmsTxtUnavailable => "llms_txt_unavailable",
        }
    }

    pub fn from_slug(slug: &str) -> Option<FindingKind> {
        [
            FindingKind::RobotsUnavailable,
            FindingKind::BotsBlocked,
            FindingKind::BotsNotBlocked,
            FindingKind::AnswersRestricted,
            FindingKind::FetchBlocked,
            FindingKind::ContentMissing,
            FindingKind::LlmsTxtUnavailable,
        ]
        .into_iter()
        .find(|k| k.slug() == slug)
    }
}

/// Evidence grade: A is what the operator documents; A- is read from the site's own HTML as
/// served, against a published measurement of how the bots fetch rather than the operators' own
/// documentation; B is what CodoSEO's synthetic check observed, against its own baseline and
/// confirmed by a second fetch; C is a heuristic CodoSEO infers; X is undocumented or unverified
/// (an operator lag nobody publishes). Which check has which grade is in [`crate::ledger`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Grade {
    A,
    #[serde(rename = "A-")]
    AMinus,
    B,
    C,
    X,
}

impl Grade {
    /// The letter as it is shown and serialised.
    pub fn label(self) -> &'static str {
        match self {
            Grade::A => "A",
            Grade::AMinus => "A-",
            Grade::B => "B",
            Grade::C => "C",
            Grade::X => "X",
        }
    }

    /// Strength, A highest.
    pub fn rank(self) -> u8 {
        match self {
            Grade::A => 4,
            Grade::AMinus => 3,
            Grade::B => 2,
            Grade::C => 1,
            Grade::X => 0,
        }
    }
}

/// One bot's part in a finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotEvidence {
    pub token: String,
    pub operator: String,
    pub purpose: Purpose,
    pub honours: Honours,
    /// Important URLs the bot may not fetch.
    pub urls_blocked: u32,
    pub urls_total: u32,
    pub home_blocked: bool,
    /// The rule that decided the home page (or, for a bot only blocked elsewhere, the first block).
    #[serde(default)]
    pub rule: Option<MatchedRule>,
    pub group: GroupMatch,
}

/// One engine's part in an `AnswersRestricted` finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineEvidence {
    pub engine: EngineId,
    pub name: String,
    /// The strongest effect the directive has on this engine.
    pub effect: Effect,
    /// Important pages the directive excludes from this engine.
    pub pages_excluded: u32,
    /// Important pages it limits.
    pub pages_limited: u32,
    /// Crawled pages that carry the directive.
    pub site_pages: u32,
}

/// One bot's part in a `FetchBlocked` finding: what the synthetic probes with its user agent got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeEvidence {
    pub token: String,
    pub operator: String,
    pub purpose: Purpose,
    /// Probed pages where the bot got this finding's cause.
    pub urls_affected: u32,
    /// Pages actually requested with its user agent: robots.txt skips, and pages whose baseline
    /// gave nothing to compare with, left out.
    pub urls_probed: u32,
    pub home_affected: bool,
    /// The status of the sample probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<Vendor>,
    /// The challenge marker the sample probe carried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// The affected pages, in the order they were probed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
    /// One representative probe (the first affected page's) and its baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<ProbeFetch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<ProbeFetch>,
}

/// One important page's part in a `ContentMissing` finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentEvidence {
    pub url: String,
    pub verdict: ContentVerdict,
    #[serde(default)]
    pub signals: Vec<ShellSignal>,
    /// Words in the HTML as served.
    pub words: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlSample {
    /// Up to ten important URLs.
    pub sample: Vec<String>,
    pub total: u32,
}

/// Structured proof for the UI to render.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bots: Vec<BotEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub engines: Vec<EngineEvidence>,
    #[serde(default)]
    pub urls: UrlSample,
    /// The robots.txt status, for `RobotsUnavailable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub robots_status: Option<u16>,
    /// The probed bots, for `FetchBlocked`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probes: Vec<ProbeEvidence>,
    /// Observed by CodoSEO's synthetic check rather than documented by an operator.
    #[serde(default, skip_serializing_if = "is_false")]
    pub synthetic: bool,
    /// The shell pages, for `ContentMissing`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<ContentEvidence>,
    /// The llms.txt status, for `LlmsTxtUnavailable`; `None` when no response arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llms_status: Option<u16>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Evidence {
    /// Who the finding is about: its bots' and probed bots' tokens (lower-cased), its engines'
    /// names and its shell pages as `url:<path>`. An incident whose finding gains a member has
    /// widened.
    pub fn members(&self) -> BTreeSet<String> {
        self.bots
            .iter()
            .map(|b| b.token.to_ascii_lowercase())
            .chain(self.probes.iter().map(|p| p.token.to_ascii_lowercase()))
            .chain(self.engines.iter().map(|e| e.name.clone()))
            .chain(
                self.content
                    .iter()
                    .map(|c| format!("url:{}", path_and_query(&c.url))),
            )
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub kind: FindingKind,
    /// Purpose slug for the bot findings, directive slug for `AnswersRestricted`, else empty.
    pub subject: String,
    pub severity: Severity,
    pub grade: Grade,
    pub title: String,
    pub summary: String,
    pub evidence: Evidence,
    /// Where the operators document what this rests on.
    pub sources: Vec<String>,
}

/// The kinds this report can speak to; open incidents of other kinds are left alone.
pub fn evaluated_kinds(report: &AccessReport) -> Vec<FindingKind> {
    let mut kinds = Vec::new();
    if report.robots.availability != RobotsAvailability::Unknown {
        kinds.push(FindingKind::RobotsUnavailable);
    }
    if report.has_verdicts() && !report.important.is_empty() {
        kinds.push(FindingKind::BotsBlocked);
        kinds.push(FindingKind::BotsNotBlocked);
    }
    if report.html_important() > 0 {
        kinds.push(FindingKind::AnswersRestricted);
    }
    if report.probes.as_ref().is_some_and(ProbeReport::conclusive) {
        kinds.push(FindingKind::FetchBlocked);
    }
    // Who may read the pages comes from robots.txt, so without verdicts there is nothing to say.
    if report.content.is_some() && report.has_verdicts() {
        kinds.push(FindingKind::ContentMissing);
    }
    // Any fetch speaks to it: a file or a 4xx resolves an open incident, a failure keeps it.
    if report.llms_txt.is_some() {
        kinds.push(FindingKind::LlmsTxtUnavailable);
    }
    kinds
}

/// Findings for the report under the owner's intent, ordered by kind then subject.
pub fn findings(report: &AccessReport, intent: &Intent) -> Vec<Finding> {
    let mut out = Vec::new();
    if let Some(f) = robots_unavailable(report, intent) {
        // Without a readable file there are no verdicts to speak of.
        out.push(f);
    } else {
        out.extend(bots_blocked(report, intent));
        out.extend(bots_not_blocked(report, intent));
        out.extend(fetch_blocked(report, intent));
        out.extend(content_missing(report, intent));
    }
    out.extend(answers_restricted(report, intent));
    out.extend(llms_txt_unavailable(report));
    out.sort_by(|a, b| (a.kind, &a.subject).cmp(&(b.kind, &b.subject)));
    out
}

// ---- wording helpers -------------------------------------------------------------------------

fn purpose_slug(p: Purpose) -> &'static str {
    match p {
        Purpose::Search => "search",
        Purpose::UserFetch => "user_fetch",
        Purpose::Agent => "agent",
        Purpose::Training => "training",
        Purpose::Ads => "ads",
    }
}

/// What the purpose is called in "You set X to Block".
fn purpose_label(p: Purpose) -> &'static str {
    match p {
        Purpose::Search => "AI search",
        Purpose::UserFetch => "user-triggered fetching",
        Purpose::Agent => "AI agents",
        Purpose::Training => "AI training",
        Purpose::Ads => "ads",
    }
}

fn purpose_noun(p: Purpose) -> &'static str {
    match p {
        Purpose::Search => "AI search bots",
        Purpose::UserFetch => "user-triggered fetchers",
        Purpose::Agent => "AI agents",
        Purpose::Training => "AI training bots",
        Purpose::Ads => "ad bots",
    }
}

/// `A`, `A and B`, `A, B and C`.
fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Like [`join_and`], but past [`MAX_NAMES`] items the rest are counted: `A, B, C and 2 more`.
fn join_capped(items: &[String]) -> String {
    if items.len() <= MAX_NAMES {
        join_and(items)
    } else {
        let shown = &items[..MAX_NAMES];
        format!("{} and {} more", shown.join(", "), items.len() - MAX_NAMES)
    }
}

/// The tokens spelled out, or counted past [`MAX_NAMES`]; and whether the phrase is plural.
fn bots_phrase(purpose: Purpose, tokens: &[String]) -> (String, bool) {
    if tokens.len() <= MAX_NAMES {
        (join_and(tokens), tokens.len() > 1)
    } else {
        (format!("{} {}", tokens.len(), purpose_noun(purpose)), true)
    }
}

fn directive_label(d: DirectiveSlug) -> &'static str {
    match d {
        DirectiveSlug::Noindex => "noindex",
        DirectiveSlug::Nosnippet => "nosnippet",
        DirectiveSlug::MaxSnippet => "max-snippet",
        DirectiveSlug::Noarchive => "noarchive",
        DirectiveSlug::Nocache => "nocache",
        DirectiveSlug::DataNosnippet => "data-nosnippet",
    }
}

fn rule_text(rule: &MatchedRule, group: GroupMatch, bot: &Bot) -> String {
    let token = &bot.token;
    let who = match (group, &bot.robots_fallback) {
        (GroupMatch::Named, _) => format!(" (User-agent: {token})"),
        (GroupMatch::Fallback, Some(fallback)) => {
            format!(" (User-agent: {fallback}, which {token} follows when no group names it)")
        }
        (GroupMatch::Wildcard, _) => " (User-agent: *)".to_owned(),
        (GroupMatch::Fallback | GroupMatch::None, _) => String::new(),
    };
    let word = if rule.allow { "Allow" } else { "Disallow" };
    format!(
        "robots.txt line {}: `{word}: {}`{who}",
        rule.line, rule.pattern
    )
}

fn push_source(sources: &mut Vec<String>, url: &str) {
    if !sources.iter().any(|s| s == url) {
        sources.push(url.to_owned());
    }
}

fn sample(report: &AccessReport, indexes: &BTreeSet<u16>) -> UrlSample {
    UrlSample {
        sample: indexes
            .iter()
            .take(SAMPLE_URLS)
            .filter_map(|i| report.important.get(usize::from(*i)))
            .map(|u| u.url.clone())
            .collect(),
        total: indexes.len() as u32,
    }
}

// ---- robots.txt unavailable ------------------------------------------------------------------

fn robots_unavailable(report: &AccessReport, intent: &Intent) -> Option<Finding> {
    if report.robots.availability != RobotsAvailability::Unavailable {
        return None;
    }
    let status = report.robots.status?;
    let evidence = Evidence {
        robots_status: Some(status),
        ..Evidence::default()
    };
    // The ledger's sources, then the pages of the bots the owner wants, which the evidence
    // doesn't carry.
    let mut sources = finding_sources(FindingKind::RobotsUnavailable, &evidence);
    let mut wanted = false;
    for bot in &registry().bots {
        if intent.effective(bot) == Stance::Allow && bot.honours_robots == Honours::Yes {
            wanted = true;
            push_source(&mut sources, &bot.source_url);
        }
    }
    let why = if status == 429 {
        "answers HTTP 429 (too many requests)"
    } else {
        "answers with a server error"
    };
    Some(Finding {
        kind: FindingKind::RobotsUnavailable,
        subject: String::new(),
        severity: if wanted {
            Severity::Critical
        } else {
            Severity::Notice
        },
        grade: for_kind(FindingKind::RobotsUnavailable).grade,
        title: format!(
            "robots.txt returns HTTP {status}, so bots treat the whole site as off limits"
        ),
        summary: format!(
            "Google treats a site as off limits while its robots.txt {why}, and bots that follow the same rules do too, so new and changed pages may not be picked up until it is fixed. Make /robots.txt answer 200, or 404 if you have none."
        ),
        evidence,
        sources,
    })
}

// ---- bots ------------------------------------------------------------------------------------

struct BotRow<'a> {
    bot: &'a Bot,
    evidence: BotEvidence,
}

fn bot_row<'a>(
    report: &AccessReport,
    access: &'a crate::report::BotAccess,
    bot: &'a Bot,
) -> BotRow<'a> {
    let first_block = access
        .blocked
        .first()
        .and_then(|b| report.rule_of(access, b));
    BotRow {
        bot,
        evidence: BotEvidence {
            token: bot.token.clone(),
            operator: bot.operator.clone(),
            purpose: bot.purpose,
            honours: bot.honours_robots,
            urls_blocked: access.blocked.len() as u32,
            urls_total: report.important.len() as u32,
            home_blocked: access.home_allowed == Some(false),
            rule: access
                .home_rule
                .clone()
                .filter(|r| !r.allow)
                .or_else(|| first_block.cloned()),
            group: access.group,
        },
    }
}

fn by_purpose<'a>(rows: Vec<BotRow<'a>>) -> BTreeMap<Purpose, Vec<BotRow<'a>>> {
    let mut map: BTreeMap<Purpose, Vec<BotRow<'a>>> = BTreeMap::new();
    for row in rows {
        map.entry(row.bot.purpose).or_default().push(row);
    }
    map
}

fn bots_blocked(report: &AccessReport, intent: &Intent) -> Vec<Finding> {
    if !report.has_verdicts() || report.important.is_empty() {
        return Vec::new();
    }
    let rows: Vec<BotRow> = report
        .bots
        .iter()
        .filter_map(|access| Some((access, registry().bot(&access.token)?)))
        .filter(|(access, bot)| {
            intent.effective(bot) == Stance::Allow && !access.blocked.is_empty()
        })
        .map(|(access, bot)| bot_row(report, access, bot))
        .collect();

    by_purpose(rows)
        .into_iter()
        .map(|(purpose, rows)| {
            let honoured = |r: &&BotRow| r.bot.honours_robots == Honours::Yes;
            let severity = if rows.iter().filter(honoured).any(|r| r.evidence.home_blocked) {
                Severity::Critical
            } else if rows.iter().any(|r| honoured(&r)) {
                Severity::Warning
            } else {
                Severity::Notice
            };
            let tokens: Vec<String> = rows.iter().map(|r| r.bot.token.clone()).collect();
            let (phrase, plural_subject) = bots_phrase(purpose, &tokens);
            let any_home = rows.iter().any(|r| r.evidence.home_blocked);
            let verb = if plural_subject { "are" } else { "is" };
            let how = if any_home { "blocked" } else { "partly blocked" };
            let title = format!("{phrase} {verb} {how} by robots.txt");

            let mut summary = String::new();
            summary.push_str(&consequence(purpose, &rows));
            // The rules, first one in full.
            let mut lines: Vec<u32> = Vec::new();
            let mut described = false;
            for r in &rows {
                if let Some(rule) = &r.evidence.rule {
                    if lines.contains(&rule.line) {
                        continue;
                    }
                    if !described {
                        summary.push(' ');
                        summary.push_str(&rule_text(rule, r.evidence.group, r.bot));
                        summary.push('.');
                        described = true;
                    }
                    lines.push(rule.line);
                }
            }
            if lines.len() > 1 {
                let rest: Vec<String> = lines[1..].iter().map(u32::to_string).collect();
                summary.push_str(&format!(
                    " Also blocked by {} {}.",
                    if rest.len() == 1 { "line" } else { "lines" },
                    join_capped(&rest)
                ));
            }
            if !any_home {
                let most = rows.iter().map(|r| r.evidence.urls_blocked).max().unwrap_or(0);
                summary.push_str(&format!(
                    " The home page is allowed; the block covers {most} of {} important pages.",
                    report.important.len()
                ));
            }
            let doubtful: Vec<String> = rows
                .iter()
                .filter(|r| r.bot.honours_robots != Honours::Yes)
                .map(|r| r.bot.token.clone())
                .collect();
            if !doubtful.is_empty() {
                summary.push_str(&format!(
                    " The operator doesn't promise that {} follow{} robots.txt, so the rule may not hold.",
                    join_capped(&doubtful),
                    if doubtful.len() == 1 { "s" } else { "" }
                ));
            }

            let mut urls = BTreeSet::new();
            for access in report.bots.iter().filter(|a| tokens.contains(&a.token)) {
                urls.extend(access.blocked.iter().map(|b| b.url));
            }
            let evidence = Evidence {
                bots: rows.into_iter().map(|r| r.evidence).collect(),
                urls: sample(report, &urls),
                ..Evidence::default()
            };
            Finding {
                kind: FindingKind::BotsBlocked,
                subject: purpose_slug(purpose).to_owned(),
                severity,
                grade: for_kind(FindingKind::BotsBlocked).grade,
                title,
                summary,
                sources: finding_sources(FindingKind::BotsBlocked, &evidence),
                evidence,
            }
        })
        .collect()
}

/// What the owner loses when these bots are blocked.
fn consequence(purpose: Purpose, rows: &[BotRow]) -> String {
    match purpose {
        Purpose::Search => {
            let names: Vec<String> = engines()
                .iter()
                .filter(|e| {
                    rows.iter()
                        .any(|r| r.bot.token.eq_ignore_ascii_case(e.crawler))
                })
                .map(|e| e.name.to_owned())
                .collect();
            if names.is_empty() {
                "Blocked AI search bots can't fetch your pages, so they may leave them out of answers."
                    .to_owned()
            } else {
                format!(
                    "Blocked AI search bots can't fetch your pages, so {} may leave them out of answers.",
                    join_and(&names)
                )
            }
        }
        Purpose::UserFetch => {
            "Blocked fetchers can't open your pages when someone asks an assistant about them."
                .to_owned()
        }
        Purpose::Agent => {
            "Blocked agents can't visit your site on behalf of their users.".to_owned()
        }
        Purpose::Training => {
            "These bots are told not to use your pages for AI training, although you allowed it."
                .to_owned()
        }
        Purpose::Ads => "Blocked ad bots can't check your pages.".to_owned(),
    }
}

fn bots_not_blocked(report: &AccessReport, intent: &Intent) -> Vec<Finding> {
    if !report.has_verdicts() || report.important.is_empty() {
        return Vec::new();
    }
    let rows: Vec<BotRow> = report
        .bots
        .iter()
        .filter_map(|access| Some((access, registry().bot(&access.token)?)))
        .filter(|(access, bot)| {
            intent.effective(bot) == Stance::Block
                && bot.honours_robots == Honours::Yes
                && access.home_allowed == Some(true)
        })
        .map(|(access, bot)| bot_row(report, access, bot))
        .collect();

    by_purpose(rows)
        .into_iter()
        .map(|(purpose, rows)| {
            let tokens: Vec<String> = rows.iter().map(|r| r.bot.token.clone()).collect();
            let (phrase, plural_subject) = bots_phrase(purpose, &tokens);
            // Control tokens never crawl: they only say what a crawler may do with the content.
            let controls = rows.iter().filter(|r| !r.bot.crawls).count();
            let tail = match (purpose, tokens.len() > MAX_NAMES) {
                _ if controls > 0 && tokens.len() > MAX_NAMES => "use your content",
                _ if controls > 0 && purpose == Purpose::Training => {
                    "use your content for AI training"
                }
                _ if controls > 0 => "use your content",
                (_, true) => "crawl your site",
                (Purpose::Search, _) => "crawl for AI search",
                (Purpose::UserFetch, _) => "fetch your pages for users",
                (Purpose::Agent, _) => "browse your site as AI agents",
                (Purpose::Training, _) => "crawl for AI training",
                (Purpose::Ads, _) => "crawl for ads",
            };
            let title = format!("{phrase} can still {tail}");

            let all_overrides = rows
                .iter()
                .all(|r| intent.bot_override(&r.bot.token).is_some());
            let choice = if all_overrides {
                format!("You chose to block {phrase}")
            } else {
                format!("You set {} to Block", purpose_label(purpose))
            };
            let mut summary = String::new();
            if report.robots.availability == RobotsAvailability::Missing {
                let status = report.robots.status.unwrap_or(404);
                summary.push_str(&if (300..400).contains(&status) {
                    format!("robots.txt redirects without reaching a file (HTTP {status}). ")
                } else {
                    format!("There is no robots.txt (HTTP {status}). ")
                });
            }
            summary.push_str(&format!(
                "{choice}, but robots.txt doesn't stop {}. Add `User-agent: {}` and `Disallow: /` to robots.txt.",
                if plural_subject { "them" } else { "it" },
                tokens[0],
            ));

            let evidence = Evidence {
                bots: rows.into_iter().map(|r| r.evidence).collect(),
                ..Evidence::default()
            };
            Finding {
                kind: FindingKind::BotsNotBlocked,
                subject: purpose_slug(purpose).to_owned(),
                severity: Severity::Warning,
                grade: for_kind(FindingKind::BotsNotBlocked).grade,
                title,
                summary,
                sources: finding_sources(FindingKind::BotsNotBlocked, &evidence),
                evidence,
            }
        })
        .collect()
}

// ---- answers ---------------------------------------------------------------------------------

#[derive(Default)]
struct PerEngine {
    excluded: BTreeSet<u16>,
    limited: BTreeSet<u16>,
    causes: Vec<Cause>,
}

fn answers_restricted(report: &AccessReport, intent: &Intent) -> Vec<Finding> {
    // directive -> engine -> pages
    let mut table: BTreeMap<&'static str, (DirectiveSlug, BTreeMap<EngineId, PerEngine>)> =
        BTreeMap::new();
    for engine in engines().iter().filter(|e| e.page_controls) {
        let wanted = registry()
            .bot(engine.crawler)
            .is_some_and(|b| intent.effective(b) == Stance::Allow);
        let Some(access) = report.engine(engine.id).filter(|_| wanted) else {
            continue;
        };
        for (group, url) in access
            .groups
            .iter()
            .filter(|g| !g.robots_blocked)
            .flat_map(|g| g.urls.iter().map(move |u| (g, *u)))
        {
            let entry = group;
            let mut per_directive: BTreeMap<&'static str, (DirectiveSlug, Effect)> =
                BTreeMap::new();
            for cause in &entry.causes {
                // `noindex` is the SEO checks' business, and a directive the owner accepted is
                // their choice; the matrix still shows both.
                if cause.directive == DirectiveSlug::Noindex
                    || intent.accepted_directives.contains(&cause.directive)
                {
                    continue;
                }
                let e = per_directive
                    .entry(slug_str(cause.directive))
                    .or_insert((cause.directive, cause.effect));
                e.1 = e.1.max(cause.effect);
            }
            for (key, (slug, effect)) in per_directive {
                let per = table
                    .entry(key)
                    .or_insert_with(|| (slug, BTreeMap::new()))
                    .1
                    .entry(engine.id)
                    .or_default();
                match effect {
                    Effect::Excluded => per.excluded.insert(url),
                    _ => per.limited.insert(url),
                };
                for cause in entry.causes.iter().filter(|c| c.directive == slug) {
                    if !per.causes.contains(cause) {
                        per.causes.push(cause.clone());
                    }
                }
            }
        }
    }

    let total = report.html_important();
    table
        .into_iter()
        .map(|(key, (slug, per_engine))| {
            let label = directive_label(slug);
            let mut urls: BTreeSet<u16> = BTreeSet::new();
            for p in per_engine.values() {
                urls.extend(&p.excluded);
                urls.extend(&p.limited);
            }
            let home_affected = urls.contains(&0);
            let home_excluded = per_engine.values().any(|p| p.excluded.contains(&0));
            let any_exclusion = per_engine.values().any(|p| !p.excluded.is_empty());
            let half = per_engine
                .values()
                .any(|p| p.excluded.len() * 2 >= total.max(1));
            let severity = if !any_exclusion {
                Severity::Notice
            } else if home_excluded || half {
                Severity::Critical
            } else {
                Severity::Warning
            };

            let n = urls.len();
            let reach = if n == total && total > 1 {
                format!("all {total} important pages")
            } else if total == 1 {
                "your only important page".to_owned()
            } else {
                format!("{n} of {total} important pages")
            };
            let home = if home_affected && total > 1 {
                ", including the home page"
            } else {
                ""
            };
            let title = format!("{label} on {reach}{home}");

            let name_of = |id: EngineId| {
                engines()
                    .iter()
                    .find(|e| e.id == id)
                    .map_or("", |e| e.name)
                    .to_owned()
            };
            let excluded: Vec<String> = per_engine
                .iter()
                .filter(|(_, p)| !p.excluded.is_empty())
                .map(|(id, _)| name_of(*id))
                .collect();
            let limited: Vec<String> = per_engine
                .iter()
                .filter(|(_, p)| p.excluded.is_empty() && !p.limited.is_empty())
                .map(|(id, _)| name_of(*id))
                .collect();
            let mut effects = Vec::new();
            if !excluded.is_empty() {
                effects.push(format!("keeps these pages out of {}", join_and(&excluded)));
            }
            if !limited.is_empty() {
                effects.push(format!(
                    "limits what {} can quote from them",
                    join_and(&limited)
                ));
            }
            let mut summary = format!("`{label}` {}.", effects.join(" and "));
            let mut where_set: Vec<String> = Vec::new();
            for c in per_engine.values().flat_map(|p| &p.causes) {
                let w = match &c.source {
                    CauseSource::Meta { name } => format!("the `{name}` meta tag"),
                    CauseSource::Header { .. } => "the X-Robots-Tag header".to_owned(),
                    CauseSource::Attribute => "`data-nosnippet` attributes".to_owned(),
                };
                if !where_set.contains(&w) {
                    where_set.push(w);
                }
            }
            if !where_set.is_empty() {
                summary.push_str(&format!(" Set by {}.", join_and(&where_set)));
            }
            let site_pages = per_engine
                .keys()
                .filter_map(|id| report.engine(*id))
                .flat_map(|e| e.site.by_cause.iter())
                .filter(|(s, _)| *s == slug)
                .map(|(_, n)| *n)
                .max()
                .unwrap_or(0);
            if site_pages > 0 {
                summary.push_str(&if site_pages == 1 {
                    " 1 crawled page carries it.".to_owned()
                } else {
                    format!(" {site_pages} crawled pages carry it.")
                });
            }

            let mut engine_evidence = Vec::new();
            for (id, p) in &per_engine {
                let Some(engine) = engines().iter().find(|e| e.id == *id) else {
                    continue;
                };
                engine_evidence.push(EngineEvidence {
                    engine: *id,
                    name: engine.name.to_owned(),
                    effect: if p.excluded.is_empty() {
                        Effect::Limited
                    } else {
                        Effect::Excluded
                    },
                    pages_excluded: p.excluded.len() as u32,
                    pages_limited: p.limited.len() as u32,
                    site_pages: report
                        .engine(*id)
                        .and_then(|e| e.site.by_cause.iter().find(|(s, _)| *s == slug))
                        .map_or(0, |(_, n)| *n),
                });
            }
            let evidence = Evidence {
                engines: engine_evidence,
                urls: sample(report, &urls),
                ..Evidence::default()
            };
            Finding {
                kind: FindingKind::AnswersRestricted,
                subject: key.to_owned(),
                severity,
                grade: for_kind(FindingKind::AnswersRestricted).grade,
                title,
                summary,
                sources: finding_sources(FindingKind::AnswersRestricted, &evidence),
                evidence,
            }
        })
        .collect()
}

// ---- synthetic fetch probes ------------------------------------------------------------------

/// One bot's results under one cause, while they are gathered.
struct ProbeRow<'a> {
    bot: &'a Bot,
    urls: Vec<u16>,
    home: bool,
    sample: Option<ProbeFetch>,
    baseline: Option<ProbeFetch>,
    vendor: Option<Vendor>,
}

fn fetch_blocked(report: &AccessReport, intent: &Intent) -> Vec<Finding> {
    let Some(probes) = &report.probes else {
        return Vec::new();
    };
    let is_home = |i: u16| {
        report
            .important
            .get(usize::from(i))
            .is_some_and(|u| u.reason == Reason::Home)
    };
    // cause -> rows, both in the order the probes ran.
    let mut causes: Vec<(&'static str, Vec<ProbeRow>)> = Vec::new();
    for url in &probes.urls {
        for result in &url.results {
            let Some(cause) = result.verdict.cause_slug() else {
                continue;
            };
            let Some(bot) = registry()
                .bot(&result.token)
                .filter(|b| intent.effective(b) == Stance::Allow)
            else {
                continue;
            };
            let rows = match causes.iter().position(|(c, _)| *c == cause) {
                Some(i) => &mut causes[i].1,
                None => {
                    causes.push((cause, Vec::new()));
                    &mut causes.last_mut().expect("just pushed").1
                }
            };
            let row = match rows.iter().position(|r| r.bot.token == bot.token) {
                Some(i) => &mut rows[i],
                None => {
                    rows.push(ProbeRow {
                        bot,
                        urls: Vec::new(),
                        home: false,
                        sample: result.fetch.clone(),
                        baseline: Some(url.baseline.clone()),
                        vendor: match result.verdict {
                            Verdict::Challenged { vendor } => Some(vendor),
                            _ => None,
                        },
                    });
                    rows.last_mut().expect("just pushed")
                }
            };
            if !row.urls.contains(&url.url) {
                row.urls.push(url.url);
            }
            row.home |= is_home(url.url);
        }
    }

    let probed = |token: &str| {
        probes
            .urls
            .iter()
            .filter(|u| {
                u.results
                    .iter()
                    .any(|r| r.token == token && r.fetch.is_some())
            })
            .count() as u32
    };
    let mut out: Vec<Finding> = causes
        .into_iter()
        .filter_map(|(cause, rows)| {
            // The cause's vendor, from the first bot it hit, for every bot under it.
            let vendor = rows.first().and_then(|r| r.vendor);
            let evidence = rows
                .into_iter()
                .map(|r| ProbeEvidence {
                    token: r.bot.token.clone(),
                    operator: r.bot.operator.clone(),
                    purpose: r.bot.purpose,
                    urls_affected: r.urls.len() as u32,
                    urls_probed: probed(&r.bot.token),
                    home_affected: r.home,
                    status: r.sample.as_ref().and_then(|s| s.status),
                    vendor,
                    signal: r
                        .sample
                        .as_ref()
                        .and_then(|s| s.challenge.as_ref())
                        .map(|c| c.signal.clone()),
                    urls: r
                        .urls
                        .iter()
                        .filter_map(|i| report.important.get(usize::from(*i)))
                        .map(|u| u.url.clone())
                        .collect(),
                    sample: r.sample,
                    baseline: r.baseline,
                })
                .collect();
            fetch_finding(report, cause, evidence)
        })
        .collect();
    out.sort_by(|a, b| a.subject.cmp(&b.subject));
    out
}

/// The `FetchBlocked` finding for one cause from its bots' evidence, first bot first: the
/// wording follows the first, the severity and the pages all of them. `None` without bots.
fn fetch_finding(report: &AccessReport, cause: &str, rows: Vec<ProbeEvidence>) -> Option<Finding> {
    let first = rows.first()?;
    let verdict = cause_verdict(cause, first);
    let vendor = first.vendor;
    let soft = matches!(verdict, Verdict::Throttled | Verdict::Degraded);
    let severity = if !soft && rows.iter().any(|r| r.home_affected) {
        Severity::Warning
    } else {
        Severity::Notice
    };
    // Every affected page once, in the report's order (pages it no longer lists last).
    let mut urls: Vec<&str> = Vec::new();
    for u in rows.iter().flat_map(|r| &r.urls) {
        if !urls.contains(&u.as_str()) {
            urls.push(u);
        }
    }
    let position = |u: &str| {
        report
            .important
            .iter()
            .position(|i| i.url == u)
            .unwrap_or(usize::MAX)
    };
    urls.sort_by_key(|u| position(u));
    let tokens: Vec<String> = rows.iter().map(|r| r.token.clone()).collect();
    let pages = if urls.len() == 1 && rows.iter().any(|r| r.home_affected) {
        "the home page".to_owned()
    } else if urls.len() == 1 {
        "1 page".to_owned()
    } else {
        format!("{} pages", urls.len())
    };
    let title = format!(
        "{} {} on {pages} (synthetic check)",
        probe_headline(&verdict),
        join_capped(&tokens)
    );
    let summary = probe_summary(&verdict, &tokens, vendor);

    let evidence = Evidence {
        urls: UrlSample {
            sample: urls
                .iter()
                .take(SAMPLE_URLS)
                .map(|u| (*u).to_owned())
                .collect(),
            total: urls.len() as u32,
        },
        probes: rows,
        synthetic: true,
        ..Evidence::default()
    };
    Some(Finding {
        kind: FindingKind::FetchBlocked,
        subject: cause.to_owned(),
        severity,
        grade: for_kind(FindingKind::FetchBlocked).grade,
        title,
        summary,
        sources: finding_sources(FindingKind::FetchBlocked, &evidence),
        evidence,
    })
}

/// The verdict a cause stands for, as the bot's evidence shows it: the vendor of a challenge,
/// the status of a block, where a redirect went.
fn cause_verdict(cause: &str, p: &ProbeEvidence) -> Verdict {
    match (cause, p.vendor) {
        (_, Some(vendor)) => Verdict::Challenged { vendor },
        ("blocked_dropped", _) => Verdict::Blocked { status: None },
        (c, _) if c.starts_with("blocked_") => Verdict::Blocked { status: p.status },
        ("diverted", _) => Verdict::Diverted {
            to: p
                .sample
                .as_ref()
                .and_then(|s| s.final_url.clone())
                .or_else(|| p.urls.first().cloned())
                .unwrap_or_default(),
        },
        ("throttled", _) => Verdict::Throttled,
        _ => Verdict::Degraded,
    }
}

/// This run's `FetchBlocked` findings with the bots it could not judge held back. A bot that is
/// inconclusive on any page it was sent to (the baseline failed or was challenged, no response,
/// a re-fetch that disagreed) says nothing new about an open incident: it keeps the evidence the
/// incident last had for it, and neither narrows, widens, resolves nor re-escalates it. `open`
/// holds each open `FetchBlocked` incident's subject and evidence as last written. An incident
/// still resolves once every bot it covers was judged this run and none of them has its cause.
/// A report whose probes judge nothing leaves `found` as it is: the kind is not evaluated.
pub fn hold_unjudged_probes(
    report: &AccessReport,
    intent: &Intent,
    open: &[(String, Evidence)],
    mut found: Vec<Finding>,
) -> Vec<Finding> {
    let Some(probes) = report.probes.as_ref().filter(|p| p.conclusive()) else {
        return found;
    };
    let judged = probes.judged_bots();
    let is_judged = |t: &str| judged.iter().any(|j| j.eq_ignore_ascii_case(t));
    // Only bots this run probed are held: one no longer probed has nothing more to say.
    let probed = |t: &str| probes.bots.iter().any(|b| b.eq_ignore_ascii_case(t));
    let wanted = |t: &str| {
        registry()
            .bot(t)
            .is_some_and(|b| intent.effective(b) == Stance::Allow)
    };
    for (subject, before) in open {
        let at = found
            .iter()
            .position(|f| f.kind == FindingKind::FetchBlocked && &f.subject == subject);
        let now: &[ProbeEvidence] = at.map_or(&[], |i| &found[i].evidence.probes);
        let held = |p: &&ProbeEvidence| !is_judged(&p.token) && probed(&p.token);
        if !before.probes.iter().any(|p| held(&p) && wanted(&p.token))
            && now.iter().all(|p| is_judged(&p.token))
        {
            continue;
        }
        // The incident's bots in its order, each with this run's evidence when it was judged
        // and its previous evidence when not; then bots judged for the first time.
        let mut rows: Vec<ProbeEvidence> = Vec::new();
        for p in &before.probes {
            if held(&p) {
                if wanted(&p.token) {
                    rows.push(p.clone());
                }
            } else if let Some(n) = now.iter().find(|n| n.token == p.token) {
                rows.push(n.clone());
            }
        }
        for n in now {
            if is_judged(&n.token) && !rows.iter().any(|r| r.token == n.token) {
                rows.push(n.clone());
            }
        }
        match (at, fetch_finding(report, subject, rows)) {
            (Some(i), Some(f)) => found[i] = f,
            (Some(i), None) => {
                found.remove(i);
            }
            (None, Some(f)) => found.push(f),
            (None, None) => {}
        }
    }
    found.sort_by(|a, b| (a.kind, &a.subject).cmp(&(b.kind, &b.subject)));
    found
}

/// Whether `report` judged any member of `finding` itself, rather than only holding it from the
/// open incident ([`hold_unjudged_probes`], [`hold_unjudged_content`]). A `FetchBlocked` finding
/// all of whose bots went unjudged, or a `ContentMissing` finding none of whose pages had raw
/// content this run, was not seen by the run: it only stands because nothing new was learned.
/// The other kinds are always judged when they are evaluated.
pub fn judged_by(report: &AccessReport, finding: &Finding) -> bool {
    match finding.kind {
        FindingKind::FetchBlocked => {
            let Some(probes) = report.probes.as_ref().filter(|p| p.conclusive()) else {
                return false;
            };
            let judged = probes.judged_bots();
            finding
                .evidence
                .probes
                .iter()
                .any(|p| judged.iter().any(|j| j.eq_ignore_ascii_case(&p.token)))
        }
        FindingKind::ContentMissing => {
            let Some(content) = report.content.as_ref() else {
                return false;
            };
            finding.evidence.content.iter().any(|c| {
                report
                    .important
                    .iter()
                    .position(|u| u.url == c.url)
                    .and_then(|i| u16::try_from(i).ok())
                    .is_some_and(|i| content.pages.iter().any(|p| p.url == i))
            })
        }
        _ => true,
    }
}

/// The start of a `FetchBlocked` title, before the bots' names.
fn probe_headline(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Challenged {
            vendor: Vendor::Generic,
        } => "CAPTCHA challenge served to".to_owned(),
        Verdict::Challenged { vendor } => format!("{} challenge served to", vendor.label()),
        Verdict::Blocked { status: None } => "Connection dropped for".to_owned(),
        Verdict::Blocked {
            status: Some(s @ (401 | 403 | 404 | 451)),
        } => format!("HTTP {s} served to"),
        Verdict::Blocked { status: Some(s) } if *s >= 500 => "Server errors served to".to_owned(),
        Verdict::Blocked { .. } => "Client errors served to".to_owned(),
        Verdict::Diverted { .. } => "Redirect elsewhere served to".to_owned(),
        Verdict::Degraded => "Smaller, different page served to".to_owned(),
        Verdict::Throttled => "Rate limiting (HTTP 429) served to".to_owned(),
        Verdict::Same | Verdict::Inconclusive { .. } | Verdict::SkippedRobots => String::new(),
    }
}

/// What the probes saw, what to check next, and what a synthetic check can't tell.
fn probe_summary(verdict: &Verdict, tokens: &[String], vendor: Option<Vendor>) -> String {
    let what = match verdict {
        Verdict::Challenged {
            vendor: Vendor::Generic,
        } => "a CAPTCHA or \"verify you are human\" page".to_owned(),
        Verdict::Challenged { vendor } => format!("a {} challenge or block page", vendor.label()),
        Verdict::Blocked { status: None } => "a dropped connection".to_owned(),
        Verdict::Blocked { status: Some(s) } => format!("HTTP {s}"),
        Verdict::Diverted { to } => format!("a redirect to {to}"),
        Verdict::Degraded => "a much smaller page with another title".to_owned(),
        Verdict::Throttled => "HTTP 429 (too many requests)".to_owned(),
        Verdict::Same | Verdict::Inconclusive { .. } | Verdict::SkippedRobots => String::new(),
    };
    let ua = if tokens.len() == 1 {
        format!("{}'s documented user agent", tokens[0])
    } else {
        format!("the documented user agents of {}", join_capped(tokens))
    };
    let next = match vendor {
        Some(Vendor::Cloudflare) => {
            "Check AI Crawl Control and Security › Bots in the Cloudflare dashboard. Cloudflare may also refuse requests that claim to be a verified bot from an unverified address, so confirm in your logs that the real bots are refused too."
        }
        Some(Vendor::Vercel) => {
            "Check the Bot Protection and AI Bots managed rulesets under Firewall in the Vercel dashboard."
        }
        Some(Vendor::AwsWaf) => "Check the Bot Control rule group in your AWS WAF web ACL.",
        Some(Vendor::DataDome) => "Check your DataDome bot rules and allow the AI bots you want.",
        Some(Vendor::Human) => "Check the known bots and crawlers policy in HUMAN Bot Defender.",
        Some(Vendor::Imperva) => "Check the bot management settings in Imperva.",
        Some(Vendor::Akamai) => "Check the bot categories and actions in Akamai Bot Manager.",
        Some(Vendor::Sucuri) => {
            "Check the access control and blocked user agents in the Sucuri firewall."
        }
        Some(Vendor::Generic) => {
            "Find which plugin, CDN or firewall serves the CAPTCHA and let the AI bots you want through."
        }
        None => {
            "Check your CDN, firewall and server rules for blocks by user agent, and let the AI bots you want through."
        }
    };
    format!(
        "A request with {ua}, sent from CodoSEO's address, got {what}, while the same request as CodoSEObot got the normal page. {next} Real bots come from their own addresses and may be treated differently."
    )
}

// ---- content missing from the HTML as served -------------------------------------------------

/// The `ContentMissing` finding: important pages that are, or are likely, an empty app shell, and
/// that at least one AI crawler which doesn't run JavaScript ([`NON_RENDERING_BOTS`]) may fetch by
/// robots.txt and the owner wants. One per site; its members are the pages.
fn content_missing(report: &AccessReport, intent: &Intent) -> Option<Finding> {
    let content = report.content.as_ref()?;
    content_finding(
        report,
        intent,
        content.shells().map(ShellPage::of).collect(),
    )
}

/// One shell page a `ContentMissing` finding may name: from this run's content report, or held
/// from the open incident's evidence by [`hold_unjudged_content`].
struct ShellPage {
    /// Index into `AccessReport::important`.
    url: u16,
    verdict: ContentVerdict,
    signals: Vec<ShellSignal>,
    words: u32,
}

impl ShellPage {
    fn of(c: &crate::content::RawContent) -> ShellPage {
        ShellPage {
            url: c.url,
            verdict: c.verdict,
            signals: c.signals.clone(),
            words: c.words,
        }
    }
}

/// This run's `ContentMissing` finding with the pages it could not judge held back. A page the
/// open incident names that is still an important page but has no raw content this run (it
/// answered 5xx, timed out, or was not HTML) says nothing new: it keeps the evidence the incident
/// last had for it, so one failed fetch neither resolves the incident nor reopens it a crawl
/// later. A page that is no longer among the important pages leaves the incident: it stopped
/// being one of the pages this check watches. Held pages still need a wanted non-rendering bot
/// that robots.txt lets fetch them. The incident resolves once every page it names was judged
/// `Present` or `Thin`, or left the important pages. `open` is the open incident's evidence as
/// last written; a report that cannot judge the kind leaves `found` as it is.
pub fn hold_unjudged_content(
    report: &AccessReport,
    intent: &Intent,
    open: Option<&Evidence>,
    mut found: Vec<Finding>,
) -> Vec<Finding> {
    let (Some(content), Some(before)) = (report.content.as_ref(), open) else {
        return found;
    };
    if !report.has_verdicts() {
        return found;
    }
    let judged: BTreeSet<u16> = content.pages.iter().map(|p| p.url).collect();
    let held: Vec<ShellPage> = before
        .content
        .iter()
        .filter_map(|c| {
            let at = report.important.iter().position(|u| u.url == c.url)?;
            let url = u16::try_from(at).ok()?;
            (!judged.contains(&url)).then(|| ShellPage {
                url,
                verdict: c.verdict,
                signals: c.signals.clone(),
                words: c.words,
            })
        })
        .collect();
    if held.is_empty() {
        return found;
    }
    let mut pages: Vec<ShellPage> = content.shells().map(ShellPage::of).collect();
    pages.extend(held);
    pages.sort_by_key(|p| p.url);
    found.retain(|f| f.kind != FindingKind::ContentMissing);
    found.extend(content_finding(report, intent, pages));
    found.sort_by(|a, b| (a.kind, &a.subject).cmp(&(b.kind, &b.subject)));
    found
}

/// The `ContentMissing` finding over `shells`, the shell pages in the report's order.
fn content_finding(
    report: &AccessReport,
    intent: &Intent,
    shells: Vec<ShellPage>,
) -> Option<Finding> {
    if !report.has_verdicts() {
        return None;
    }
    let readers: Vec<(&Bot, &crate::report::BotAccess)> = NON_RENDERING_BOTS
        .iter()
        .filter_map(|token| Some((registry().bot(token)?, report.bot(token)?)))
        .filter(|(bot, _)| intent.effective(bot) == Stance::Allow)
        .collect();
    let may_fetch =
        |access: &crate::report::BotAccess, url: u16| !access.blocked.iter().any(|b| b.url == url);
    let pages: Vec<ShellPage> = shells
        .into_iter()
        .filter(|c| readers.iter().any(|(_, a)| may_fetch(a, c.url)))
        .collect();
    if pages.is_empty() {
        return None;
    }
    let tokens: Vec<String> = readers
        .iter()
        .filter(|(_, a)| pages.iter().any(|c| may_fetch(a, c.url)))
        .map(|(bot, _)| bot.token.clone())
        .collect();

    let is_home = |i: u16| {
        report
            .important
            .get(usize::from(i))
            .is_some_and(|u| u.reason == Reason::Home)
    };
    let home = pages.iter().any(|c| is_home(c.url));
    let html = report.html_important();
    let severity = if home || pages.len() * 2 >= html.max(1) {
        Severity::Warning
    } else {
        Severity::Notice
    };

    let certain = pages.iter().all(|c| c.verdict == ContentVerdict::Shell);
    let n = pages.len();
    let who = "AI crawlers that don't run JavaScript";
    let title = match (n, home, certain) {
        (1, true, true) => format!("The home page is an empty app shell to {who}"),
        (1, true, false) => format!("The home page looks like an empty app shell to {who}"),
        (1, false, true) => format!("1 important page is an empty app shell to {who}"),
        (1, false, false) => format!("1 important page looks like an empty app shell to {who}"),
        (_, _, true) => format!("{n} important pages are empty app shells to {who}"),
        (_, _, false) => format!("{n} important pages look like empty app shells to {who}"),
    };

    let mut signals: BTreeSet<ShellSignal> = BTreeSet::new();
    for c in &pages {
        signals.extend(c.signals.iter().copied());
    }
    let what: Vec<String> = signals
        .iter()
        .map(|s| {
            match s {
                ShellSignal::MountEmpty => "the app's mount element is empty",
                ShellSignal::NoscriptJs => "a `<noscript>` notice asks for JavaScript",
                ShellSignal::ScriptHeavy => "inline script makes up most of the page",
                ShellSignal::ManyScripts => {
                    "it loads several scripts but has no H1 and no internal links"
                }
            }
            .to_owned()
        })
        .collect();
    let subject = if n == 1 && home {
        "the home page has"
    } else if n == 1 {
        "this page has"
    } else {
        "these pages have"
    };
    // The remedy first: API and MCP clients clip summaries.
    let mut summary = format!(
        "Render the main content on the server, or prerender {}, so it is in the HTML. In the HTML as served, {subject} fewer than {SHELL_MAX_WORDS} words of text",
        if n == 1 { "the page" } else { "these pages" },
    );
    if !what.is_empty() {
        summary.push_str(&format!(" and {}", join_and(&what)));
    }
    summary.push('.');
    // The measurement is cited only for the crawlers it measured; the others are their
    // operators' other fetchers.
    let (measured, others): (Vec<String>, Vec<String>) = tokens
        .iter()
        .cloned()
        .partition(|t| MEASURED_BOTS.contains(&t.as_str()));
    if !measured.is_empty() {
        summary.push_str(&format!(
            " Vercel and MERJ measured in 2024 that {} {} run JavaScript.",
            join_and(&measured),
            if measured.len() == 1 {
                "doesn't"
            } else {
                "don't"
            },
        ));
    }
    if !others.is_empty() {
        summary.push_str(&format!(
            " {} {} from the same operators as the crawlers they measured (GPTBot, ClaudeBot and PerplexityBot) and {} expected to fetch the same way.",
            join_capped(&others),
            if others.len() == 1 { "comes" } else { "come" },
            if others.len() == 1 { "is" } else { "are" },
        ));
    }
    summary.push_str(
        " A bot that doesn't run JavaScript reads the page as served, not what a browser shows once its scripts have run.",
    );

    let indexes: BTreeSet<u16> = pages.iter().map(|c| c.url).collect();
    let content = pages
        .iter()
        .filter_map(|c| {
            Some(ContentEvidence {
                url: report.important.get(usize::from(c.url))?.url.clone(),
                verdict: c.verdict,
                signals: c.signals.clone(),
                words: c.words,
            })
        })
        .collect();
    let evidence = Evidence {
        urls: sample(report, &indexes),
        content,
        ..Evidence::default()
    };
    Some(Finding {
        kind: FindingKind::ContentMissing,
        subject: String::new(),
        severity,
        grade: for_kind(FindingKind::ContentMissing).grade,
        title,
        summary,
        sources: finding_sources(FindingKind::ContentMissing, &evidence),
        evidence,
    })
}

// ---- llms.txt unavailable --------------------------------------------------------------------

/// The `LlmsTxtUnavailable` finding: the site had adopted llms.txt (served one before, as carried
/// by [`crate::llms::carry_adoption`]) and it now answers 5xx or 429, or not at all, as
/// Lighthouse's audit counts a failure. Informational: Warning at most, whatever the intent, since
/// no bot depends on the file to crawl. A site that never served one gets nothing, and a 4xx or a
/// soft 404 is a removal, not a failure.
fn llms_txt_unavailable(report: &AccessReport) -> Option<Finding> {
    let llms = report.llms_txt.as_ref()?;
    if llms.state != LlmsState::Unavailable || !llms.adopted {
        return None;
    }
    let status = (llms.status != 0).then_some(llms.status);
    let (title, what) = match (status, llms.error.as_deref()) {
        (Some(429), _) => (
            "llms.txt returns HTTP 429".to_owned(),
            "now answers HTTP 429 (too many requests)".to_owned(),
        ),
        (Some(s), _) => (
            format!("llms.txt returns HTTP {s}"),
            format!("now answers HTTP {s}"),
        ),
        (None, Some(e)) => (
            "llms.txt could not be fetched".to_owned(),
            format!("now gets no answer ({e})"),
        ),
        (None, None) => (
            "llms.txt could not be fetched".to_owned(),
            "now gets no answer".to_owned(),
        ),
    };
    let evidence = Evidence {
        llms_status: status,
        ..Evidence::default()
    };
    Some(Finding {
        kind: FindingKind::LlmsTxtUnavailable,
        subject: String::new(),
        severity: Severity::Warning,
        grade: for_kind(FindingKind::LlmsTxtUnavailable).grade,
        title,
        summary: format!(
            "Make /llms.txt answer 200 again, or 404 if you removed it on purpose. Your site served it before, and it {what}, so AI agents that look for it get nothing; Lighthouse counts this as a failed llms.txt check. llms.txt is informational: it does not affect your health score or how search engines crawl the site."
        ),
        sources: finding_sources(FindingKind::LlmsTxtUnavailable, &evidence),
        evidence,
    })
}
