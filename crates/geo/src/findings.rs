//! What the access report means for the site owner, in the owner's terms.
//!
//! Four kinds of finding, all grade A (documented by the operator): robots.txt can't be read,
//! bots the owner wants are blocked, bots the owner wants blocked still get in, and page markup
//! that takes pages out of an engine's AI answers. Severity follows the owner's [`Intent`]: a
//! blocked training bot is not a problem unless the owner said they want it to crawl.

use std::collections::{BTreeMap, BTreeSet};

use codoseo_core::check::Severity;
use serde::{Deserialize, Serialize};

use crate::eligibility::{Cause, CauseSource, DirectiveSlug, Effect, EngineId, engines};
use crate::intent::{Intent, Stance};
use crate::registry::{Bot, Honours, Purpose, registry};
use crate::report::{AccessReport, slug_str};
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
}

impl FindingKind {
    pub fn slug(self) -> &'static str {
        match self {
            FindingKind::RobotsUnavailable => "robots_unavailable",
            FindingKind::BotsBlocked => "bots_blocked",
            FindingKind::BotsNotBlocked => "bots_not_blocked",
            FindingKind::AnswersRestricted => "answers_restricted",
        }
    }

    pub fn from_slug(slug: &str) -> Option<FindingKind> {
        [
            FindingKind::RobotsUnavailable,
            FindingKind::BotsBlocked,
            FindingKind::BotsNotBlocked,
            FindingKind::AnswersRestricted,
        ]
        .into_iter()
        .find(|k| k.slug() == slug)
    }
}

/// Evidence grade: A is what the operator documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Grade {
    A,
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
}

impl Evidence {
    /// Who the finding is about: its bots' tokens (lower-cased) and its engines' names. An
    /// incident whose finding gains a member has widened.
    pub fn members(&self) -> BTreeSet<String> {
        self.bots
            .iter()
            .map(|b| b.token.to_ascii_lowercase())
            .chain(self.engines.iter().map(|e| e.name.clone()))
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
    }
    out.extend(answers_restricted(report, intent));
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
    let mut sources = vec![ROBOTS_DOC.to_owned()];
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
        grade: Grade::A,
        title: format!(
            "robots.txt returns HTTP {status}, so bots treat the whole site as off limits"
        ),
        summary: format!(
            "Google treats a site as off limits while its robots.txt {why}, and bots that follow the same rules do too, so new and changed pages may not be picked up until it is fixed. Make /robots.txt answer 200, or 404 if you have none."
        ),
        evidence: Evidence {
            robots_status: Some(status),
            ..Evidence::default()
        },
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
            let mut sources = Vec::new();
            for access in report.bots.iter().filter(|a| tokens.contains(&a.token)) {
                urls.extend(access.blocked.iter().map(|b| b.url));
            }
            for r in &rows {
                push_source(&mut sources, &r.bot.source_url);
            }
            Finding {
                kind: FindingKind::BotsBlocked,
                subject: purpose_slug(purpose).to_owned(),
                severity,
                grade: Grade::A,
                title,
                summary,
                evidence: Evidence {
                    bots: rows.into_iter().map(|r| r.evidence).collect(),
                    urls: sample(report, &urls),
                    ..Evidence::default()
                },
                sources,
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

            let mut sources = Vec::new();
            for r in &rows {
                push_source(&mut sources, &r.bot.source_url);
            }
            Finding {
                kind: FindingKind::BotsNotBlocked,
                subject: purpose_slug(purpose).to_owned(),
                severity: Severity::Warning,
                grade: Grade::A,
                title,
                summary,
                evidence: Evidence {
                    bots: rows.into_iter().map(|r| r.evidence).collect(),
                    ..Evidence::default()
                },
                sources,
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

            let mut sources = Vec::new();
            let mut engine_evidence = Vec::new();
            for (id, p) in &per_engine {
                let Some(engine) = engines().iter().find(|e| e.id == *id) else {
                    continue;
                };
                push_source(&mut sources, engine.source_url);
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
            Finding {
                kind: FindingKind::AnswersRestricted,
                subject: key.to_owned(),
                severity,
                grade: Grade::A,
                title,
                summary,
                evidence: Evidence {
                    engines: engine_evidence,
                    urls: sample(report, &urls),
                    ..Evidence::default()
                },
                sources,
            }
        })
        .collect()
}
