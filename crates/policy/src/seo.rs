//! The SEO rules against a crawl: preferred address, what to index, URL parameters that don't
//! change the page, and pages that must always work.
//!
//! Each evaluator is pure: it reads the crawl's pages (and the inlink samples, for links), says
//! how the site measures up ([`RuleResult`], with one [`RulePart`] per index rule or must-work
//! page), lists the gaps, and marks the pages behind them with the policy checks
//! ([`CheckId::POLICY`]), so a gap is also an ordinary issue on the page. A rule the policy
//! doesn't declare is "not checked" and marks nothing, so the default policy changes nothing.

use std::collections::{BTreeMap, HashMap, HashSet};

use codoseo_core::check::{CheckId, IssueBits, Severity};
use codoseo_core::page::{Indexability, PageRecord};
use codoseo_core::url::{normalize, url_hash};
use url::Url;

use crate::evaluate::{
    Gap, GapKind, MAX_RULE_URLS, PolicyInput, RuleId, RulePart, RuleResult, Status,
};
use crate::{HostPref, IndexStance, SeoRules, SitePolicy, Slash, glob_ci};

/// What the SEO evaluators found: the rule results and gaps for the report, and the policy check
/// bits for each page of the input, in page order.
#[derive(Debug, Clone, Default)]
pub struct SeoOutcome {
    pub rules: Vec<RuleResult>,
    pub gaps: Vec<Gap>,
    pub bits: Vec<IssueBits>,
}

/// The rules the policy declares, as the checks they turn on. A check counts toward the checks
/// total only when it is listed here.
pub fn declared_checks(seo: &SeoRules) -> Vec<CheckId> {
    let mut out = Vec::new();
    if seo.address.is_some() {
        out.push(CheckId::PolicyAddress);
    }
    if index_declared(seo) {
        out.push(CheckId::PolicyIndexMismatch);
        if seo.index_default == IndexStance::KeepOut
            || seo.index.iter().any(|r| r.stance == IndexStance::KeepOut)
        {
            out.push(CheckId::PolicyKeepOutPartial);
        }
    }
    if !seo.ignore_params.is_empty() {
        out.push(CheckId::PolicyParamCanonical);
    }
    if !seo.must_work.is_empty() {
        out.push(CheckId::MustWorkFailed);
    }
    out
}

fn index_declared(seo: &SeoRules) -> bool {
    seo.index_default != IndexStance::None
        || seo.index.iter().any(|r| r.stance != IndexStance::None)
}

/// Runs the four SEO evaluators.
pub fn evaluate_seo(policy: &SitePolicy, input: &PolicyInput) -> SeoOutcome {
    let mut out = SeoOutcome {
        bits: vec![IssueBits::default(); input.pages.len()],
        ..SeoOutcome::default()
    };
    let origin = origin(input);
    address(policy, input, origin.as_ref(), &mut out);
    index(policy, input, &mut out);
    params(policy, input, &mut out);
    must_work(policy, input, origin.as_ref(), &mut out);
    out
}

fn origin(input: &PolicyInput) -> Option<Url> {
    input
        .origin
        .cloned()
        .or_else(|| input.pages.first().map(|p| p.url.clone()))
        .and_then(|u| u.join("/").ok())
}

/// `/a/b?x=1`, what a pattern is matched against.
pub fn path_and_query(u: &Url) -> String {
    match u.query() {
        Some(q) => format!("{}?{q}", u.path()),
        None => u.path().to_owned(),
    }
}

pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn are(n: usize) -> &'static str {
    if n == 1 { "is" } else { "are" }
}

fn not_checked(id: RuleId, text: &str) -> RuleResult {
    RuleResult {
        id,
        status: Status::NotChecked,
        gaps: 0,
        text: text.to_owned(),
        urls: Vec::new(),
        parts: Vec::new(),
    }
}

const NO_CRAWL: &str = "Not checked yet: it applies from the next crawl.";

fn push_url(urls: &mut Vec<String>, u: &Url) {
    if urls.len() < MAX_RULE_URLS && !urls.iter().any(|x| x == u.as_str()) {
        urls.push(u.as_str().to_owned());
    }
}

fn gap(rule: RuleId, severity: Severity, text: String, subject: Option<String>) -> Gap {
    Gap {
        rule,
        kind: GapKind::Seo,
        severity,
        text,
        subject,
    }
}

// ---- preferred address -----------------------------------------------------------------------

/// The one address form the policy wants.
#[derive(Debug, Clone)]
pub struct Preferred {
    pub scheme: String,
    pub host: String,
    pub slash: Slash,
}

/// Why a URL is not in the preferred form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Off {
    Scheme,
    Host,
    Slash,
}

impl Preferred {
    /// The form for `origin` (its scheme, the apex or www host).
    pub fn new(origin: &Url, host: HostPref, slash: Slash) -> Preferred {
        let h = origin.host_str().unwrap_or_default().to_ascii_lowercase();
        let apex = h.strip_prefix("www.").unwrap_or(&h).to_owned();
        Preferred {
            scheme: origin.scheme().to_owned(),
            host: match host {
                HostPref::Apex => apex,
                HostPref::Www => format!("www.{apex}"),
            },
            slash,
        }
    }

    /// `https://www.example.com`
    pub fn base(&self) -> String {
        format!("{}://{}", self.scheme, self.host)
    }

    fn off(&self, u: &Url) -> Option<Off> {
        if u.scheme() != self.scheme {
            return Some(Off::Scheme);
        }
        if !u
            .host_str()
            .is_some_and(|h| h.eq_ignore_ascii_case(&self.host))
        {
            return Some(Off::Host);
        }
        (!slash_ok(u.path(), self.slash)).then_some(Off::Slash)
    }

    /// Whether `u` is in the preferred form.
    pub fn matches(&self, u: &Url) -> bool {
        self.off(u).is_none()
    }
}

/// The home page and file-like paths (`/feed.xml`) are fine either way.
fn slash_ok(path: &str, slash: Slash) -> bool {
    if path == "/" || slash == Slash::Either {
        return true;
    }
    if path.ends_with('/') {
        return slash == Slash::With;
    }
    let last = path.rsplit('/').next().unwrap_or_default();
    last.contains('.') || slash == Slash::Without
}

fn off_words(o: Off) -> &'static str {
    match o {
        Off::Scheme => "http://",
        Off::Host => "another host",
        Off::Slash => "the other ending",
    }
}

fn address(policy: &SitePolicy, input: &PolicyInput, origin: Option<&Url>, out: &mut SeoOutcome) {
    let id = RuleId::Address;
    let Some(rule) = &policy.seo.address else {
        out.rules.push(not_checked(id, "No preferred address set."));
        return;
    };
    let Some(origin) = origin.filter(|_| !input.pages.is_empty()) else {
        out.rules.push(not_checked(id, NO_CRAWL));
        return;
    };
    let want = Preferred::new(origin, rule.host, rule.trailing_slash);
    let pages = input.pages;
    let by_url: HashMap<&str, usize> = pages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.url.as_str(), i))
        .collect();

    let mut total = 0usize;
    let mut served: Vec<usize> = Vec::new();
    let mut served_off: BTreeMap<Off, usize> = BTreeMap::new();
    let mut canonical = 0usize;
    let mut redirects = 0usize;
    let mut urls = Vec::new();
    let mut flagged: HashSet<usize> = HashSet::new();

    for (i, p) in pages.iter().enumerate() {
        if p.is_html_ok() {
            total += 1;
            if let Some(o) = want.off(&p.url) {
                served.push(i);
                *served_off.entry(o).or_default() += 1;
                flagged.insert(i);
                push_url(&mut urls, &p.url);
            } else if p
                .fields
                .canonical
                .as_ref()
                .is_some_and(|c| !want.matches(c))
            {
                canonical += 1;
                flagged.insert(i);
                push_url(&mut urls, &p.url);
            }
        } else if (300..400).contains(&p.status) {
            // Where the redirect ends, followed through the crawl's own records.
            let mut at = p.redirect_target.clone();
            for _ in 0..10 {
                let Some(next) = at
                    .as_ref()
                    .and_then(|t| by_url.get(t.as_str()))
                    .map(|&j| &pages[j])
                    .filter(|t| (300..400).contains(&t.status))
                else {
                    break;
                };
                at = next.redirect_target.clone();
            }
            if at.as_ref().is_some_and(|t| !want.matches(t)) {
                redirects += 1;
                flagged.insert(i);
                push_url(&mut urls, &p.url);
            }
        }
    }

    // Internal links to an address that only redirects: each costs a hop.
    let mut links: HashSet<(u32, u32)> = HashSet::new();
    let mut link_off: BTreeMap<Off, usize> = BTreeMap::new();
    for s in input.inlinks {
        let (Some(t), Some(src)) = (pages.get(s.target as usize), pages.get(s.source as usize))
        else {
            continue;
        };
        if s.source == s.target || !(300..400).contains(&t.status) || !src.is_html_ok() {
            continue;
        }
        let Some(o) = want.off(&t.url) else { continue };
        if links.insert((s.source, s.target)) {
            *link_off.entry(o).or_default() += 1;
            flagged.insert(s.source as usize);
        }
    }

    for &i in &flagged {
        out.bits[i].set_check(CheckId::PolicyAddress);
    }
    let ending = match rule.trailing_slash {
        Slash::Without => ", no trailing slash",
        Slash::With => ", with a trailing slash",
        Slash::Either => "",
    };
    if flagged.is_empty() {
        out.rules.push(RuleResult {
            id,
            status: Status::Met,
            gaps: 0,
            text: format!(
                "All {} use {}{ending}.",
                plural(total, "page", "pages"),
                want.base()
            ),
            urls: Vec::new(),
            parts: Vec::new(),
        });
        return;
    }

    let mut bits: Vec<String> = Vec::new();
    let mut add_gap = |text: String| {
        out.gaps
            .push(gap(id, Severity::Warning, text.clone(), None));
        bits.push(text);
    };
    if !served.is_empty() {
        let why = served_off.keys().next().copied().unwrap_or(Off::Host);
        add_gap(format!(
            "{} {} served on {}",
            plural(served.len(), "page", "pages"),
            are(served.len()),
            match why {
                Off::Scheme => "http://".to_owned(),
                Off::Host => "another host".to_owned(),
                Off::Slash => match rule.trailing_slash {
                    Slash::With => "an address without the trailing slash".to_owned(),
                    _ => "an address with a trailing slash".to_owned(),
                },
            }
        ));
    }
    if canonical > 0 {
        add_gap(format!(
            "{} to another address",
            plural(canonical, "page canonicalises", "pages canonicalise")
        ));
    }
    if redirects > 0 {
        add_gap(format!(
            "{} to another address",
            plural(redirects, "URL redirects", "URLs redirect")
        ));
    }
    if !links.is_empty() {
        let why = link_off
            .iter()
            .max_by_key(|(_, n)| **n)
            .map_or(Off::Scheme, |(o, _)| *o);
        let n = links.len();
        add_gap(format!(
            "{}{} use {}, each a redirect hop",
            if input.inlinks_capped {
                "at least "
            } else {
                ""
            },
            plural(n, "internal link", "internal links"),
            off_words(why)
        ));
    }
    let matched = total - served.len();
    out.rules.push(RuleResult {
        id,
        status: Status::Gaps,
        gaps: flagged.len() as u32,
        text: format!("{matched} of {total} pages match. {}.", bits.join(" · ")),
        urls,
        parts: Vec::new(),
    });
}

// ---- what to index ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Tally {
    total: usize,
    indexable: Vec<usize>,
    noindex: Vec<usize>,
    canonicalised: Vec<usize>,
    blocked: Vec<usize>,
}

/// Which pages an index rule is about: HTML answers and URLs robots.txt keeps us from.
fn index_subject(p: &PageRecord) -> bool {
    p.is_html_ok() || p.indexability == Indexability::BlockedByRobots
}

fn short_list(pages: &[PageRecord], idx: &[usize]) -> String {
    let paths: Vec<&str> = idx.iter().take(3).map(|&i| pages[i].url.path()).collect();
    match paths.len() {
        0 => String::new(),
        1 => paths[0].to_owned(),
        2 => format!("{} and {}", paths[0], paths[1]),
        _ => format!("{}, {} and {}", paths[0], paths[1], paths[2]),
    }
}

fn in_sitemap(pages: &[PageRecord], idx: &[usize]) -> usize {
    idx.iter().filter(|&&i| pages[i].in_sitemap).count()
}

/// The result of one row (an index rule or "everything else"), its gap, and the pages to mark.
fn index_row(
    subject: &str,
    stance: IndexStance,
    t: &Tally,
    pages: &[PageRecord],
    out: &mut SeoOutcome,
    urls: &mut Vec<String>,
) -> RulePart {
    let note = if t.total > 0 && t.blocked.len() == t.total {
        plural(t.total, "URL linked", "URLs linked")
    } else {
        plural(t.total, "page", "pages")
    };
    let part = |status, gaps: usize, text: String| RulePart {
        subject: subject.to_owned(),
        note: note.clone(),
        status,
        gaps: gaps as u32,
        text,
    };
    if stance == IndexStance::None {
        return part(Status::NotChecked, 0, "No rule for these pages.".to_owned());
    }
    if t.total == 0 {
        return part(Status::NotChecked, 0, "No crawled page matches.".to_owned());
    }
    let id = RuleId::Index;
    match stance {
        IndexStance::KeepOut => {
            let bad = &t.indexable;
            for &i in bad {
                out.bits[i].set_check(CheckId::PolicyIndexMismatch);
                push_url(urls, &pages[i].url);
            }
            for &i in &t.blocked {
                out.bits[i].set_check(CheckId::PolicyKeepOutPartial);
                push_url(urls, &pages[i].url);
            }
            if !bad.is_empty() {
                let k = in_sitemap(pages, bad);
                let mut text = if bad.len() == t.total {
                    "Indexable today: no noindex".to_owned()
                } else {
                    format!(
                        "{} {} indexable today: no noindex",
                        bad.len(),
                        are(bad.len())
                    )
                };
                if k > 0 {
                    text.push_str(&format!(", and {k} {} in the sitemap", are(k)));
                }
                text.push('.');
                if !t.blocked.is_empty() {
                    text.push_str(&format!(
                        " {} more {} blocked by robots.txt only.",
                        t.blocked.len(),
                        are(t.blocked.len())
                    ));
                }
                out.gaps.push(gap(
                    id,
                    Severity::Warning,
                    format!(
                        "{subject}: {} indexable today",
                        plural(bad.len(), "page", "pages")
                    ),
                    Some(subject.to_owned()),
                ));
                part(Status::Gaps, bad.len(), text)
            } else if !t.blocked.is_empty() {
                let n = t.blocked.len();
                let text = if t.noindex.is_empty() && t.canonicalised.is_empty() {
                    "Blocked in robots.txt but no noindex, so Google can still index the URLs from links. Allow crawling and add noindex.".to_owned()
                } else {
                    format!(
                        "{} {} blocked in robots.txt with no noindex, so Google can still index {} from links. Allow crawling and add noindex.",
                        n,
                        are(n),
                        if n == 1 { "it" } else { "them" }
                    )
                };
                out.gaps.push(gap(
                    id,
                    Severity::Notice,
                    format!(
                        "{subject}: robots.txt only, so {} can still be indexed",
                        if n == 1 { "it" } else { "they" }
                    ),
                    Some(subject.to_owned()),
                ));
                part(Status::Partly, 0, text)
            } else {
                let met: Vec<usize> = t.noindex.iter().chain(&t.canonicalised).copied().collect();
                let k = in_sitemap(pages, &met);
                let mut text = if t.canonicalised.is_empty() {
                    if t.total == 1 {
                        "It carries noindex".to_owned()
                    } else {
                        format!("All {} carry noindex", t.total)
                    }
                } else {
                    format!(
                        "{} carry noindex, {} canonicalise elsewhere",
                        t.noindex.len(),
                        t.canonicalised.len()
                    )
                };
                if k == 0 {
                    text.push_str(" and are out of the sitemap.");
                } else {
                    text.push_str(&format!("; {k} {} still in the sitemap.", are(k)));
                }
                part(Status::Met, 0, text)
            }
        }
        IndexStance::Index => {
            let bad: Vec<usize> = t.noindex.iter().chain(&t.blocked).copied().collect();
            for &i in &bad {
                out.bits[i].set_check(CheckId::PolicyIndexMismatch);
                push_url(urls, &pages[i].url);
            }
            let ok = t.indexable.len() + t.canonicalised.len();
            if bad.is_empty() {
                return part(Status::Met, 0, format!("{ok} indexable."));
            }
            let mut text = Vec::new();
            if ok > 0 {
                text.push(format!("{ok} indexable."));
            }
            if !t.noindex.is_empty() {
                text.push(if t.noindex.len() == t.total {
                    format!(
                        "{} noindex.",
                        if t.total == 1 {
                            "It carries".to_owned()
                        } else {
                            format!("All {} carry", t.total)
                        }
                    )
                } else if t.noindex.len() <= 3 {
                    format!(
                        "{} {} noindex.",
                        short_list(pages, &t.noindex),
                        if t.noindex.len() == 1 {
                            "carries"
                        } else {
                            "carry"
                        }
                    )
                } else {
                    format!("{} carry noindex.", t.noindex.len())
                });
            }
            if !t.blocked.is_empty() {
                text.push(if t.blocked.len() == t.total {
                    "robots.txt blocks crawling.".to_owned()
                } else {
                    format!(
                        "robots.txt blocks crawling of {}.",
                        plural(t.blocked.len(), "URL", "URLs")
                    )
                });
            }
            let severity = if t.noindex.is_empty() {
                Severity::Warning
            } else {
                Severity::Critical
            };
            let what = match (t.noindex.is_empty(), t.blocked.is_empty()) {
                (false, true) => format!(
                    "{} noindex",
                    plural(t.noindex.len(), "page carries", "pages carry")
                ),
                (true, false) => format!(
                    "{} blocked by robots.txt",
                    plural(t.blocked.len(), "URL is", "URLs are")
                ),
                _ => format!(
                    "{} not indexable",
                    plural(bad.len(), "page is", "pages are")
                ),
            };
            out.gaps.push(gap(
                id,
                severity,
                format!("{subject}: {what}"),
                Some(subject.to_owned()),
            ));
            part(Status::Gaps, bad.len(), text.join(" "))
        }
        IndexStance::None => unreachable!("handled above"),
    }
}

/// Which row each page falls in: the first rule it matches (`Some(i)`), else everything else.
pub fn index_rows(seo: &SeoRules, pages: &[PageRecord]) -> Vec<Option<usize>> {
    pages
        .iter()
        .map(|p| {
            let pq = path_and_query(&p.url);
            seo.index.iter().position(|r| r.pattern.matches(&pq))
        })
        .collect()
}

/// The label of the "everything else" row.
pub const EVERYTHING_ELSE: &str = "Everything else";

fn index(policy: &SitePolicy, input: &PolicyInput, out: &mut SeoOutcome) {
    let id = RuleId::Index;
    let seo = &policy.seo;
    if !index_declared(seo) {
        out.rules.push(not_checked(id, "No index rules set."));
        return;
    }
    if input.pages.is_empty() {
        out.rules.push(not_checked(id, NO_CRAWL));
        return;
    }
    let pages = input.pages;
    let mut tallies: Vec<Tally> = (0..=seo.index.len()).map(|_| Tally::default()).collect();
    for (i, (p, row)) in pages.iter().zip(index_rows(seo, pages)).enumerate() {
        if !index_subject(p) {
            continue;
        }
        let t = &mut tallies[row.unwrap_or(seo.index.len())];
        t.total += 1;
        match p.indexability {
            Indexability::Indexable => t.indexable.push(i),
            Indexability::Noindex => t.noindex.push(i),
            Indexability::Canonicalised => t.canonicalised.push(i),
            Indexability::BlockedByRobots => t.blocked.push(i),
            _ => t.total -= 1,
        }
    }
    let mut urls = Vec::new();
    let mut parts = Vec::new();
    for (r, t) in seo.index.iter().zip(&tallies) {
        parts.push(index_row(
            r.pattern.as_str(),
            r.stance,
            t,
            pages,
            out,
            &mut urls,
        ));
    }
    parts.push(index_row(
        EVERYTHING_ELSE,
        seo.index_default,
        &tallies[seo.index.len()],
        pages,
        out,
        &mut urls,
    ));

    let gaps: u32 = parts.iter().map(|p| p.gaps).sum();
    let partly: usize = parts.iter().filter(|p| p.status == Status::Partly).count();
    let checked = parts
        .iter()
        .filter(|p| matches!(p.status, Status::Met | Status::Gaps | Status::Partly))
        .count();
    let rows_met = parts.iter().filter(|p| p.status == Status::Met).count();
    let (status, text) = if gaps > 0 {
        (
            Status::Gaps,
            format!(
                "{} indexed differently from your rules · {rows_met} of {checked} rules met",
                plural(gaps as usize, "page is", "pages are")
            ),
        )
    } else if partly > 0 {
        (
            Status::Partly,
            "Kept out by robots.txt only: search engines can still index those URLs from links."
                .to_owned(),
        )
    } else if checked > 0 {
        (
            Status::Met,
            "Every page matches your index rules.".to_owned(),
        )
    } else {
        (
            Status::NotChecked,
            "No crawled page matches a rule.".to_owned(),
        )
    };
    out.rules.push(RuleResult {
        id,
        status,
        gaps,
        text,
        urls,
        parts,
    });
}

// ---- URL parameters --------------------------------------------------------------------------

/// The query keys of `u` the policy ignores (the pagination parameter never is).
fn ignored_keys(seo: &SeoRules, u: &Url) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for (k, _) in u.query_pairs() {
        if seo
            .pagination_param
            .as_deref()
            .is_some_and(|p| p.eq_ignore_ascii_case(&k))
        {
            continue;
        }
        if seo.ignore_params.iter().any(|p| glob_ci(p, &k)) && !keys.iter().any(|x| *x == k) {
            keys.push(k.into_owned());
        }
    }
    keys
}

fn params(policy: &SitePolicy, input: &PolicyInput, out: &mut SeoOutcome) {
    let id = RuleId::Params;
    let seo = &policy.seo;
    if seo.ignore_params.is_empty() {
        out.rules
            .push(not_checked(id, "No ignored parameters set."));
        return;
    }
    if input.pages.is_empty() {
        out.rules.push(not_checked(id, NO_CRAWL));
        return;
    }
    let mut carry = 0usize;
    let mut ok = 0usize;
    // Gaps by the parameter named first: (pages canonicalising to themselves, pages whose
    // canonical keeps the parameter).
    let mut by_key: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut urls = Vec::new();
    for (i, p) in input.pages.iter().enumerate() {
        if !p.is_html_ok() {
            continue;
        }
        let keys = ignored_keys(seo, &p.url);
        let Some(first) = keys.first() else { continue };
        carry += 1;
        let bad = match p.indexability {
            Indexability::Indexable => Some(0),
            Indexability::Canonicalised => p
                .fields
                .canonical
                .as_ref()
                .filter(|c| !ignored_keys(seo, c).is_empty())
                .map(|_| 1),
            _ => None,
        };
        match bad {
            None => ok += 1,
            Some(which) => {
                let e = by_key.entry(first.clone()).or_default();
                if which == 0 {
                    e.0 += 1;
                } else {
                    e.1 += 1;
                }
                out.bits[i].set_check(CheckId::PolicyParamCanonical);
                push_url(&mut urls, &p.url);
            }
        }
    }
    if carry == 0 {
        out.rules.push(RuleResult {
            id,
            status: Status::Met,
            gaps: 0,
            text: "No crawled URL carries these parameters.".to_owned(),
            urls: Vec::new(),
            parts: Vec::new(),
        });
        return;
    }
    let bad = carry - ok;
    let head = format!(
        "{} {} these parameters; {}",
        plural(carry, "URL", "URLs"),
        if carry == 1 { "carries" } else { "carry" },
        if bad == 0 {
            if carry == 1 {
                "it canonicalises to the clean URL.".to_owned()
            } else {
                "all canonicalise to the clean URL.".to_owned()
            }
        } else {
            format!("{ok} canonicalise to the clean URL.")
        }
    );
    if bad == 0 {
        out.rules.push(RuleResult {
            id,
            status: Status::Met,
            gaps: 0,
            text: head,
            urls: Vec::new(),
            parts: Vec::new(),
        });
        return;
    }
    let mut details = Vec::new();
    for (key, (selfs, keeps)) in &by_key {
        let n = selfs + keeps;
        out.gaps.push(gap(
            id,
            Severity::Notice,
            format!(
                "{n} ?{key}= {} canonicalise to the clean URL",
                if n == 1 {
                    "page doesn't"
                } else {
                    "pages don't"
                }
            ),
            Some(key.clone()),
        ));
        if *selfs > 0 {
            details.push(format!(
                "{selfs} ?{key}= {} to {}",
                if *selfs == 1 {
                    "page canonicalises"
                } else {
                    "pages canonicalise"
                },
                if *selfs == 1 { "itself" } else { "themselves" }
            ));
        }
        if *keeps > 0 {
            details.push(format!(
                "{keeps} ?{key}= {} the parameter in the canonical",
                if *keeps == 1 {
                    "page keeps"
                } else {
                    "pages keep"
                }
            ));
        }
    }
    out.rules.push(RuleResult {
        id,
        status: Status::Gaps,
        gaps: bad as u32,
        text: format!("{head} {}.", details.join(" · ")),
        urls,
        parts: Vec::new(),
    });
}

// ---- pages that must work --------------------------------------------------------------------

/// Why a page is on the must-work list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MustWhy {
    Home,
    Listed,
    Starred,
    ListedStarred,
}

impl MustWhy {
    pub fn label(self) -> &'static str {
        match self {
            MustWhy::Home => "Home",
            MustWhy::Listed => "Listed",
            MustWhy::Starred => "Starred",
            MustWhy::ListedStarred => "Listed · starred",
        }
    }
}

/// How one must-work page fared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MustWorkRow {
    /// As listed: `/pricing`.
    pub path: String,
    pub url: Url,
    pub why: MustWhy,
    /// `Met` (200 and indexable), `Gaps` (it failed) or `NotChecked` (not reached).
    pub status: Status,
    /// `200, indexable`, `answers 500`, `carries noindex`, `Not reached on this crawl`.
    pub text: String,
    /// The page the verdict is about (the end of any redirects), when it was crawled.
    pub page: Option<usize>,
    /// What it answered: `500`, `noindex`, `timeout`, for the change it records.
    pub state: String,
}

/// The must-work pages in order: the home page, the listed paths, then the starred pages, each
/// once (a listed page that is also starred says so).
pub fn must_work_targets(
    seo: &SeoRules,
    input: &PolicyInput,
    origin: &Url,
) -> Vec<(String, Url, MustWhy)> {
    let mut out: Vec<(String, Url, MustWhy)> = Vec::new();
    let mut seen: HashMap<u64, usize> = HashMap::new();
    let mut add = |path: String, url: Url, why: MustWhy, out: &mut Vec<(String, Url, MustWhy)>| {
        let h = url_hash(&url);
        if let Some(&i) = seen.get(&h) {
            if why == MustWhy::Starred && out[i].2 == MustWhy::Listed {
                out[i].2 = MustWhy::ListedStarred;
            }
            return;
        }
        seen.insert(h, out.len());
        out.push((path, url, why));
    };
    if let Some(home) = normalize(origin, "/") {
        add("/".to_owned(), home, MustWhy::Home, &mut out);
    }
    for p in &seo.must_work {
        if let Some(u) = normalize(origin, p) {
            add(p.clone(), u, MustWhy::Listed, &mut out);
        }
    }
    let mut starred: Vec<Url> = input.starred_urls.to_vec();
    if let Some(hashes) = input.starred {
        for page in input.pages {
            if hashes.contains(&page.url_hash) && !starred.contains(&page.url) {
                starred.push(page.url.clone());
            }
        }
    }
    for u in starred {
        add(path_and_query(&u), u, MustWhy::Starred, &mut out);
    }
    out
}

/// Judges each must-work page against the crawl. Redirects are followed through the crawl's own
/// records: the page is judged where it ends.
pub fn must_work_rows(seo: &SeoRules, input: &PolicyInput) -> Vec<MustWorkRow> {
    let Some(origin) = origin(input) else {
        return Vec::new();
    };
    let pages = input.pages;
    let by_hash: HashMap<u64, usize> = pages
        .iter()
        .enumerate()
        .map(|(i, p)| (url_hash(&p.url), i))
        .collect();
    must_work_targets(seo, input, &origin)
        .into_iter()
        .map(|(path, url, why)| {
            let mut at = by_hash.get(&url_hash(&url)).copied();
            let mut hops = Vec::new();
            for _ in 0..10 {
                let Some(i) = at else { break };
                let p = &pages[i];
                if !(300..400).contains(&p.status) {
                    break;
                }
                match &p.redirect_target {
                    Some(t) => {
                        hops.push(p.status);
                        at = by_hash.get(&url_hash(t)).copied();
                    }
                    None => break,
                }
            }
            let (status, text, state) = match at {
                None => (
                    Status::NotChecked,
                    "Not reached on this crawl.".to_owned(),
                    String::new(),
                ),
                Some(i) => verdict(&pages[i]),
            };
            let text = match (hops.first(), at) {
                (Some(code), Some(i)) if status != Status::NotChecked => {
                    format!("{code} to {}, then {text}", pages[i].url.path())
                }
                _ => text,
            };
            MustWorkRow {
                path,
                url,
                why,
                status,
                text,
                page: at,
                state,
            }
        })
        .collect()
}

fn verdict(p: &PageRecord) -> (Status, String, String) {
    let fail = |text: String, state: &str| (Status::Gaps, text, state.to_owned());
    if p.status == 0 {
        if p.indexability == Indexability::BlockedByRobots {
            return fail("blocked by robots.txt.".to_owned(), "blocked by robots.txt");
        }
        let why = match p.error {
            Some(codoseo_core::page::FetchFailure::Timeout) => "timed out",
            Some(codoseo_core::page::FetchFailure::Connect) => "could not connect",
            Some(
                codoseo_core::page::FetchFailure::RedirectLoop
                | codoseo_core::page::FetchFailure::TooManyRedirects,
            ) => "redirects in a loop",
            _ => "could not be fetched",
        };
        return fail(format!("{why}."), why);
    }
    if !(200..300).contains(&p.status) {
        if (300..400).contains(&p.status) {
            return fail(
                format!("answers {} to a page off the site.", p.status),
                &p.status.to_string(),
            );
        }
        return fail(format!("answers {}.", p.status), &p.status.to_string());
    }
    match p.indexability {
        Indexability::Indexable => (
            Status::Met,
            format!("{}, indexable.", p.status),
            format!("{}", p.status),
        ),
        Indexability::Noindex => fail("carries noindex.".to_owned(), "noindex"),
        Indexability::Canonicalised => fail(
            format!(
                "canonicalises to {}.",
                p.fields
                    .canonical
                    .as_ref()
                    .map_or("another URL", |c| c.as_str())
            ),
            "canonicalised",
        ),
        _ => fail(format!("answers {}.", p.status), &p.status.to_string()),
    }
}

fn must_work(policy: &SitePolicy, input: &PolicyInput, origin: Option<&Url>, out: &mut SeoOutcome) {
    let id = RuleId::MustWork;
    let seo = &policy.seo;
    if seo.must_work.is_empty() {
        out.rules.push(not_checked(id, "No must-work pages set."));
        return;
    }
    if input.pages.is_empty() || origin.is_none() {
        out.rules.push(not_checked(id, NO_CRAWL));
        return;
    }
    let rows = must_work_rows(seo, input);
    let mut urls = Vec::new();
    let mut parts = Vec::new();
    let mut failed = Vec::new();
    let mut met = 0usize;
    for r in &rows {
        match r.status {
            Status::Gaps => {
                if let Some(i) = r.page {
                    out.bits[i].set_check(CheckId::MustWorkFailed);
                }
                push_url(&mut urls, &r.url);
                let text = r.text.trim_end_matches('.');
                out.gaps.push(gap(
                    id,
                    Severity::Critical,
                    format!("{} {text}", r.path),
                    Some(r.path.clone()),
                ));
                failed.push(format!("{} {text}", r.path));
            }
            Status::Met => met += 1,
            _ => {}
        }
        parts.push(RulePart {
            subject: r.path.clone(),
            note: r.why.label().to_owned(),
            status: r.status,
            gaps: u32::from(r.status == Status::Gaps),
            text: r.text.clone(),
        });
    }
    let unreached = rows.len() - met - failed.len();
    let tail = if unreached > 0 {
        format!(
            " {} not reached.",
            plural(unreached, "page was", "pages were")
        )
    } else {
        String::new()
    };
    let (status, text) = if !failed.is_empty() {
        (Status::Gaps, format!("{}.{tail}", failed.join(" · ")))
    } else if met == 0 {
        (
            Status::NotChecked,
            "None of these pages was reached on this crawl.".to_owned(),
        )
    } else {
        (
            Status::Met,
            format!(
                "{} 200 and {} indexable.{tail}",
                if met == 1 {
                    "It answers".to_owned()
                } else {
                    format!("All {met} answer")
                },
                if met == 1 { "is" } else { "are" }
            ),
        )
    };
    out.rules.push(RuleResult {
        id,
        status,
        gaps: failed.len() as u32,
        text,
        urls,
        parts,
    });
}
