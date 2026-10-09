//! The AI access report: what one crawl says about who can reach, read and quote a site.
//!
//! Pure data built from a [`CrawlOutput`]. It is stored as JSONB per crawl and read back by the web
//! UI, so it stays compact (URLs are `u16` indexes into `important`, rules are pooled per bot) and
//! every later-added field is `#[serde(default)]`. Findings are derived from it, never from the crawl.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use codoseo_core::crawl::RobotsFile;
use codoseo_core::output::{CrawlOutput, WellKnownFile};
use codoseo_core::page::PageRecord;
use codoseo_core::url::url_hash;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::declared::{
    TdmRepEntry, is_known_signal_key, is_known_usage_key, parse_content_signal,
    parse_content_usage, parse_tdmrep,
};
use crate::eligibility::{Cause, DirectiveSlug, Effect, EngineId, engines, record_effect};
use crate::registry::{Honours, Purpose, registry};
use crate::robots::{
    ContentSignal, ContentUsage, GroupMatch, MatchedRule, Pair, RobotsAvailability, RobotsTxt,
    availability,
};

/// Bumped when a stored report can no longer be read by the code that wrote the older shape.
pub const REPORT_VERSION: u32 = 1;

/// At most this many important URLs go into a report.
pub const MAX_IMPORTANT: usize = 60;
/// Declared preferences kept per list (header values, tdmrep entries).
const MAX_DECLARED: usize = 100;
/// Longest declared string kept, in characters.
const MAX_DECLARED_TEXT: usize = 512;
/// Pages from the top of the inlink ranking that count as important (as in `codoseo_diff::key_pages`).
const TOP_PAGES: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Home,
    MostLinked,
    Starred,
}

/// A page the report speaks about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportantUrl {
    pub url: String,
    pub reason: Reason,
    /// `None` when the page isn't in the crawl (only possible for the home page).
    pub status: Option<u16>,
    /// A successful HTML response: the only pages answer eligibility applies to.
    pub html_ok: bool,
}

/// The pages the report checks one by one: the origin page first, then the 20 on-origin pages with
/// the most inlinks (ties by URL), then starred pages present in the crawl by URL, deduplicated and
/// capped at [`MAX_IMPORTANT`].
///
/// The set agrees with `codoseo_diff::key_pages` (which a test asserts). Like it, the origin is
/// always included: when the crawl has no page for it (a redirect to elsewhere, a failed fetch) it
/// comes first with `status: None`. Unlike it, a starred hash that matches no crawled page has no URL
/// to show, so it is left out.
pub fn important_urls(
    pages: &[PageRecord],
    origin: &Url,
    starred: &HashSet<u64>,
) -> Vec<ImportantUrl> {
    let mut on_origin: Vec<&PageRecord> = pages
        .iter()
        .filter(|p| same_origin(&p.url, origin))
        .collect();
    on_origin.sort_by(|a, b| {
        (Reverse(a.inlinks), a.url.as_str()).cmp(&(Reverse(b.inlinks), b.url.as_str()))
    });
    let origin_hash = url_hash(origin);

    let entry = |p: &PageRecord, reason: Reason| ImportantUrl {
        url: p.url.as_str().to_owned(),
        reason,
        status: Some(p.status),
        html_ok: p.is_html_ok(),
    };
    let mut out: Vec<ImportantUrl> = Vec::new();
    let mut seen: HashSet<u64> = HashSet::new();

    match on_origin.iter().find(|p| url_hash(&p.url) == origin_hash) {
        Some(p) => out.push(entry(p, Reason::Home)),
        None => out.push(ImportantUrl {
            url: origin.as_str().to_owned(),
            reason: Reason::Home,
            status: None,
            html_ok: false,
        }),
    }
    seen.insert(origin_hash);

    let mut add = |out: &mut Vec<ImportantUrl>, p: &PageRecord, reason: Reason| {
        if seen.insert(url_hash(&p.url)) {
            out.push(entry(p, reason));
        }
    };
    for p in on_origin.iter().take(TOP_PAGES) {
        let reason = if starred.contains(&url_hash(&p.url)) {
            Reason::Starred
        } else {
            Reason::MostLinked
        };
        // The origin is already in; `seen` skips it.
        add(&mut out, p, reason);
    }
    let mut extra: Vec<&PageRecord> = on_origin
        .iter()
        .copied()
        .filter(|p| starred.contains(&url_hash(&p.url)))
        .collect();
    extra.sort_by(|a, b| a.url.as_str().cmp(b.url.as_str()));
    for p in extra {
        add(&mut out, p, Reason::Starred);
    }
    out.truncate(MAX_IMPORTANT);
    out
}

fn same_origin(url: &Url, origin: &Url) -> bool {
    url.scheme() == origin.scheme()
        && url.host_str() == origin.host_str()
        && url.port_or_known_default() == origin.port_or_known_default()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RobotsInfo {
    pub status: Option<u16>,
    pub availability: RobotsAvailability,
    /// The crawl's fingerprint of the rules, for change detection.
    #[serde(default)]
    pub hash: Option<u64>,
}

/// One important URL a bot may not fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedUrl {
    /// Index into `AccessReport::important`.
    pub url: u16,
    /// Index into the bot's `rules`; `None` when no rule matched (never the case for a block, kept for safety).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<u8>,
}

/// What robots.txt says to one registry bot (control tokens included).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotAccess {
    pub token: String,
    /// The home page verdict; `None` when there are no important URLs.
    pub home_allowed: Option<bool>,
    pub group: GroupMatch,
    /// The rule that decided the home page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_rule: Option<MatchedRule>,
    /// The distinct rules that blocked something, referenced by `blocked[].rule`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<MatchedRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<BlockedUrl>,
}

impl BotAccess {
    /// The rule that blocked `b`.
    pub fn rule_of(&self, b: &BlockedUrl) -> Option<&MatchedRule> {
        b.rule.and_then(|i| self.rules.get(usize::from(i)))
    }
}

/// Important pages that look the same to one engine (same effect, same causes, same robots
/// verdict), so a site-wide directive is one group and not 60 copies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineGroup {
    pub effect: Effect,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes: Vec<Cause>,
    /// The engine's crawler may not fetch these URLs; the pages' own markup is moot.
    #[serde(default, skip_serializing_if = "is_false")]
    pub robots_blocked: bool,
    /// Indexes into `AccessReport::important`, ascending.
    pub urls: Vec<u16>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteCounts {
    /// On-origin pages that answered 200 with HTML.
    pub pages: u32,
    pub excluded: u32,
    pub limited: u32,
    /// Pages carrying each directive (one count per page), most first.
    #[serde(default)]
    pub by_cause: Vec<(DirectiveSlug, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineAccess {
    pub engine: EngineId,
    /// Important URLs that are HTML 200 pages.
    pub html_pages: u16,
    /// Important pages that are limited, excluded or unreachable for this engine; eligible and
    /// reachable ones are `html_pages` minus the pages listed here.
    #[serde(default)]
    pub groups: Vec<EngineGroup>,
    pub site: SiteCounts,
}

/// A `Content-Usage` header value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderUsage {
    pub path: Option<String>,
    pub pairs: Vec<Pair>,
}

/// Declared AI-use preferences of the home page's response headers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredHeaders {
    #[serde(default)]
    pub content_signal: Vec<Pair>,
    #[serde(default)]
    pub content_usage: Vec<HeaderUsage>,
    #[serde(default)]
    pub tdm_reservation: Option<String>,
    #[serde(default)]
    pub tdm_policy: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TdmMeta {
    #[serde(default)]
    pub reservation: Option<String>,
    #[serde(default)]
    pub policy: Option<String>,
}

/// `/.well-known/tdmrep.json` as fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TdmRepFile {
    pub status: u16,
    #[serde(default)]
    pub entries: Vec<TdmRepEntry>,
    /// Why a 2xx body didn't parse.
    #[serde(default)]
    pub error: Option<String>,
}

/// What the site says about AI use outside the allow/deny rules. Declared, not enforced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declared {
    #[serde(default)]
    pub content_signals: Vec<ContentSignal>,
    #[serde(default)]
    pub content_usage: Vec<ContentUsage>,
    #[serde(default)]
    pub headers: DeclaredHeaders,
    #[serde(default)]
    pub tdm_meta: TdmMeta,
    #[serde(default)]
    pub tdmrep: Option<TdmRepFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessReport {
    #[serde(default)]
    pub version: u32,
    pub registry_version: u32,
    pub robots: RobotsInfo,
    pub important: Vec<ImportantUrl>,
    /// Empty unless robots.txt is available (2xx) or missing (4xx), the only cases with verdicts.
    #[serde(default)]
    pub bots: Vec<BotAccess>,
    #[serde(default)]
    pub engines: Vec<EngineAccess>,
    #[serde(default)]
    pub declared: Declared,
    #[serde(default)]
    pub pages_crawled: u32,
}

impl EngineAccess {
    /// Important pages that are limited, excluded or unreachable.
    pub fn affected(&self) -> usize {
        self.groups.iter().map(|g| g.urls.len()).sum()
    }
}

impl AccessReport {
    /// Robots verdicts exist: the file was read, or there is none and everything is allowed.
    pub fn has_verdicts(&self) -> bool {
        matches!(
            self.robots.availability,
            RobotsAvailability::Ok | RobotsAvailability::Missing
        )
    }

    pub fn bot(&self, token: &str) -> Option<&BotAccess> {
        self.bots
            .iter()
            .find(|b| b.token.eq_ignore_ascii_case(token))
    }

    pub fn engine(&self, id: EngineId) -> Option<&EngineAccess> {
        self.engines.iter().find(|e| e.engine == id)
    }

    /// Important URLs that are HTML 200 pages.
    pub fn html_important(&self) -> usize {
        self.important.iter().filter(|u| u.html_ok).count()
    }
}

/// The fixed lower-case name of a directive, as in the saved data and the finding subjects.
pub fn slug_str(slug: DirectiveSlug) -> &'static str {
    match slug {
        DirectiveSlug::Noindex => "noindex",
        DirectiveSlug::Nosnippet => "nosnippet",
        DirectiveSlug::MaxSnippet => "max_snippet",
        DirectiveSlug::Noarchive => "noarchive",
        DirectiveSlug::Nocache => "nocache",
        DirectiveSlug::DataNosnippet => "data_nosnippet",
    }
}

/// Builds the report for one crawl over the `important` pages.
pub fn build_report(out: &CrawlOutput, important: &[ImportantUrl]) -> AccessReport {
    let robots_file: Option<&RobotsFile> = out.robots.as_ref();
    let status = robots_file.map(|r| r.status);
    let avail = availability(status);
    let parsed = match (avail, robots_file) {
        (RobotsAvailability::Ok, Some(f)) => Some(RobotsTxt::parse(f.body.as_bytes())),
        (RobotsAvailability::Missing, _) => Some(RobotsTxt::parse(b"")),
        _ => None,
    };

    let important: Vec<ImportantUrl> = important.iter().take(MAX_IMPORTANT).cloned().collect();
    let paths: Vec<String> = important.iter().map(|i| path_and_query(&i.url)).collect();
    let by_url: HashMap<&str, &PageRecord> =
        out.pages.iter().map(|p| (p.url.as_str(), p)).collect();

    let bots = match &parsed {
        Some(robots) => registry()
            .bots
            .iter()
            .map(|bot| bot_access(robots, &bot.token, &paths))
            .collect(),
        None => Vec::new(),
    };

    let site_pages: Vec<&PageRecord> = out
        .pages
        .iter()
        .filter(|p| same_origin(&p.url, &out.origin) && p.is_html_ok())
        .collect();
    let engines_out = engines()
        .iter()
        .map(|engine| {
            let mut groups: Vec<EngineGroup> = Vec::new();
            let mut html_pages = 0u16;
            for (i, imp) in important.iter().enumerate() {
                let Some(page) = by_url.get(imp.url.as_str()) else {
                    continue;
                };
                let Some((effect, causes)) = record_effect(engine, page) else {
                    continue;
                };
                html_pages = html_pages.saturating_add(1);
                let robots_blocked = parsed
                    .as_ref()
                    .is_some_and(|r| !r.verdict(engine.crawler, &paths[i]).allowed);
                if robots_blocked || effect != Effect::Eligible {
                    match groups.iter_mut().find(|g| {
                        g.effect == effect
                            && g.robots_blocked == robots_blocked
                            && g.causes == causes
                    }) {
                        Some(g) => g.urls.push(i as u16),
                        None => groups.push(EngineGroup {
                            effect,
                            causes,
                            robots_blocked,
                            urls: vec![i as u16],
                        }),
                    }
                }
            }

            let mut site = SiteCounts {
                pages: site_pages.len() as u32,
                ..SiteCounts::default()
            };
            let mut by_cause: HashMap<DirectiveSlug, u32> = HashMap::new();
            for page in &site_pages {
                let Some((effect, causes)) = record_effect(engine, page) else {
                    continue;
                };
                match effect {
                    Effect::Excluded => site.excluded += 1,
                    Effect::Limited => site.limited += 1,
                    Effect::Eligible => {}
                }
                let slugs: HashSet<DirectiveSlug> = causes.iter().map(|c| c.directive).collect();
                for slug in slugs {
                    *by_cause.entry(slug).or_default() += 1;
                }
            }
            let mut by_cause: Vec<(DirectiveSlug, u32)> = by_cause.into_iter().collect();
            by_cause.sort_by_key(|(slug, n)| (Reverse(*n), slug_str(*slug)));
            site.by_cause = by_cause;

            EngineAccess {
                engine: engine.id,
                html_pages,
                groups,
                site,
            }
        })
        .collect();

    let home = important
        .first()
        .and_then(|i| by_url.get(i.url.as_str()))
        .copied();
    AccessReport {
        version: REPORT_VERSION,
        registry_version: registry().version,
        robots: RobotsInfo {
            status,
            availability: avail,
            hash: robots_file.map(|r| r.hash),
        },
        important,
        bots,
        engines: engines_out,
        declared: declared(out, avail, robots_file, home),
        pages_crawled: out.pages.len() as u32,
    }
}

fn path_and_query(url: &str) -> String {
    match Url::parse(url) {
        Ok(u) => match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_owned(),
        },
        Err(_) => "/".to_owned(),
    }
}

fn bot_access(robots: &RobotsTxt, token: &str, paths: &[String]) -> BotAccess {
    let mut rules: Vec<MatchedRule> = Vec::new();
    let mut blocked = Vec::new();
    let mut home: Option<(bool, GroupMatch, Option<MatchedRule>)> = None;
    for (i, path) in paths.iter().enumerate() {
        let v = robots.verdict(token, path);
        if i == 0 {
            home = Some((v.allowed, v.group, v.rule.clone()));
        }
        if !v.allowed {
            let rule = v.rule.map(|r| match rules.iter().position(|x| *x == r) {
                Some(p) => p as u8,
                None => {
                    rules.push(r);
                    (rules.len() - 1) as u8
                }
            });
            blocked.push(BlockedUrl {
                url: i as u16,
                rule,
            });
        }
    }
    // The group is the same for every path; with no URLs, ask about the root.
    let (home_allowed, group, home_rule) = match home {
        Some((allowed, group, rule)) => (Some(allowed), group, rule),
        None => (None, robots.verdict(token, "/").group, None),
    };
    BotAccess {
        token: token.to_owned(),
        home_allowed,
        group,
        home_rule,
        rules,
        blocked,
    }
}

fn declared(
    out: &CrawlOutput,
    avail: RobotsAvailability,
    robots_file: Option<&RobotsFile>,
    home: Option<&PageRecord>,
) -> Declared {
    let mut d = Declared::default();
    if let (RobotsAvailability::Ok, Some(f)) = (avail, robots_file) {
        let robots = RobotsTxt::parse(f.body.as_bytes());
        d.content_signals = robots.content_signals().to_vec();
        d.content_usage = robots.content_usage().to_vec();
    }
    for (name, value) in &out.signals.home_headers {
        let value = &clip(value);
        match name.to_ascii_lowercase().as_str() {
            "content-signal" => d.headers.content_signal.extend(
                parse_content_signal(value)
                    .into_iter()
                    .map(|(key, value)| pair(key, value, is_known_signal_key))
                    .take(MAX_DECLARED.saturating_sub(d.headers.content_signal.len())),
            ),
            "content-usage" if d.headers.content_usage.len() < MAX_DECLARED => {
                let (path, pairs) = parse_content_usage(value);
                d.headers.content_usage.push(HeaderUsage {
                    path,
                    pairs: pairs
                        .into_iter()
                        .map(|(key, value)| pair(key, value, is_known_usage_key))
                        .take(MAX_DECLARED)
                        .collect(),
                });
            }
            "tdm-reservation" => {
                d.headers
                    .tdm_reservation
                    .get_or_insert_with(|| value.trim().to_owned());
            }
            "tdm-policy" => {
                d.headers
                    .tdm_policy
                    .get_or_insert_with(|| value.trim().to_owned());
            }
            _ => {}
        }
    }
    if let Some(page) = home {
        d.tdm_meta = TdmMeta {
            reservation: page.fields.ai.tdm_reservation.as_deref().map(clip),
            policy: page.fields.ai.tdm_policy.as_deref().map(clip),
        };
    }
    d.tdmrep = out.signals.tdmrep.as_ref().map(tdmrep_file);
    d
}

/// A declared string cut to [`MAX_DECLARED_TEXT`] characters.
fn clip(s: &str) -> String {
    s.trim().chars().take(MAX_DECLARED_TEXT).collect()
}

fn pair(key: String, value: String, known: fn(&str) -> bool) -> Pair {
    Pair {
        known: known(&key),
        key,
        value,
    }
}

fn tdmrep_file(f: &WellKnownFile) -> TdmRepFile {
    if !(200..300).contains(&f.status) {
        return TdmRepFile {
            status: f.status,
            entries: Vec::new(),
            error: None,
        };
    }
    match parse_tdmrep(&f.body) {
        Ok(mut entries) => TdmRepFile {
            status: f.status,
            entries: {
                entries.truncate(MAX_DECLARED);
                for e in &mut entries {
                    e.location = clip(&e.location);
                    e.policy = e.policy.as_deref().map(clip);
                }
                entries
            },
            error: None,
        },
        Err(error) => TdmRepFile {
            status: f.status,
            entries: Vec::new(),
            error: Some(clip(&error)),
        },
    }
}

/// What robots.txt says to one registry bot for one path, flattened for the CLI and MCP tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotVerdict {
    pub token: String,
    pub operator: String,
    pub purpose: Purpose,
    pub honours_robots: Honours,
    pub allowed: bool,
    /// The robots.txt line of the rule that decided it; `None` when no rule matched.
    pub line: Option<u32>,
    /// The deciding rule as written, such as `Disallow: /private`.
    pub rule: Option<String>,
}

/// The Content-Signal and Content-Usage lines of a robots.txt: stated preferences, not enforced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RobotsDeclared {
    pub content_signals: Vec<ContentSignal>,
    pub content_usage: Vec<ContentUsage>,
}

impl RobotsDeclared {
    pub fn of(robots: &RobotsTxt) -> Self {
        RobotsDeclared {
            content_signals: robots.content_signals().to_vec(),
            content_usage: robots.content_usage().to_vec(),
        }
    }
}

/// Every registry bot's verdict for `path` (with its query, if any), in registry order.
pub fn bot_verdicts(robots: &RobotsTxt, path: &str) -> Vec<BotVerdict> {
    registry()
        .bots
        .iter()
        .map(|bot| {
            let v = robots.verdict(&bot.token, path);
            BotVerdict {
                token: bot.token.clone(),
                operator: bot.operator.clone(),
                purpose: bot.purpose,
                honours_robots: bot.honours_robots,
                allowed: v.allowed,
                line: v.rule.as_ref().map(|r| r.line),
                rule: v.rule.map(|r| {
                    format!(
                        "{}: {}",
                        if r.allow { "Allow" } else { "Disallow" },
                        r.pattern
                    )
                }),
            }
        })
        .collect()
}
