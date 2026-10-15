//! The evidence ledger: for every finding kind and every check that shows a grade without opening
//! an incident, what it rests on, how strong that is ([`Grade`]), where it is documented and what
//! it cannot tell. The findings, the web app and docs/ai-access.md all read their grades and
//! sources from here (the docs through a test that keeps its table in step).
//!
//! An entry's `sources` are the citations it always carries. A finding adds the pages its
//! evidence points to ([`finding_sources`]): each bot's operator page as the registry cites it,
//! each engine's page-control documentation and, for a challenge, the bot manager's own docs.
//!
//! [`lag`] says how long after a fix the operators behind a finding take to notice it, where they
//! publish that, and grades the claim X where they don't.

use std::collections::BTreeSet;

use crate::content::NON_RENDERING_SOURCE;
use crate::eligibility::engines;
use crate::findings::{Evidence, FindingKind, Grade, ROBOTS_DOC};
use crate::llms::{LIGHTHOUSE_DOC, LIGHTHOUSE_SOURCE, SPEC_URL as LLMS_SPEC_URL};
use crate::markdown::MARKDOWN_FOR_AGENTS_DOC;
use crate::probe::Vendor;
use crate::registry::registry;
use crate::signals::CONTENT_SIGNALS_URL;

/// The robots.txt standard.
pub const RFC_9309: &str = "https://www.rfc-editor.org/rfc/rfc9309";
/// Cloudflare's Content Signals Policy, which defines `Content-Signal`.
pub const CONTENT_SIGNAL_DOC: &str = "https://blog.cloudflare.com/content-signals-policy/";
/// The IETF AIPREF draft that attaches `Content-Usage` to robots.txt and HTTP.
pub const CONTENT_USAGE_DOC: &str = "https://datatracker.ietf.org/doc/draft-ietf-aipref-attach/";
/// The W3C community group's final TDMRep report.
pub const TDMREP_DOC: &str =
    "https://www.w3.org/community/reports/tdmrep/CG-FINAL-tdmrep-20240510/";
/// OpenAI's crawler page: "it can take ~24 hours from a site's robots.txt update for our systems
/// to adjust".
pub const OPENAI_LAG_DOC: &str = "https://developers.openai.com/api/docs/bots";
/// DuckDuckGo's DuckAssistBot page: "the change will take effect after 72 hours".
pub const DUCKDUCKGO_LAG_DOC: &str =
    "https://duckduckgo.com/duckduckgo-help-pages/results/duckassistbot";

/// Where each bot manager documents its bot settings, cited by `FetchBlocked` findings for its
/// challenges. Only pages the vendor owns and that were confirmed to exist; Imperva is left out
/// because its documentation moved and no stable page could be confirmed.
pub const VENDOR_DOCS: &[(Vendor, &str)] = &[
    (
        Vendor::Cloudflare,
        "https://developers.cloudflare.com/ai-crawl-control/",
    ),
    (
        Vendor::Cloudflare,
        "https://developers.cloudflare.com/bots/",
    ),
    (Vendor::Vercel, "https://vercel.com/docs/bot-management"),
    (
        Vendor::AwsWaf,
        "https://docs.aws.amazon.com/waf/latest/developerguide/waf-bot-control.html",
    ),
    (Vendor::DataDome, "https://docs.datadome.co/docs/"),
    (
        Vendor::Human,
        "https://docs.humansecurity.com/applications-and-accounts/docs/getting-started-with-bot-defender",
    ),
    (
        Vendor::Akamai,
        "https://techdocs.akamai.com/cloud-security/docs/about-bots",
    ),
    (Vendor::Sucuri, "https://docs.sucuri.net/website-firewall/"),
];

/// What one finding kind or check rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerEntry {
    /// The finding kind's slug, or the check's name.
    pub key: &'static str,
    pub grade: Grade,
    /// What the claim rests on, in a sentence.
    pub rests_on: &'static str,
    /// Citations the entry always carries; a finding adds those its evidence points to.
    pub sources: &'static [&'static str],
    /// What it cannot tell.
    pub limits: &'static str,
    /// The grade and what it means here, as the web app shows it next to the grade.
    pub explain: &'static str,
}

/// The grade-A wording for findings that rest on the operators' own documentation.
const OPERATOR_DOCUMENTED: &str = "Evidence grade A: documented by the operator";

static LEDGER: [LedgerEntry; 16] = [
    // ---- findings ----
    LedgerEntry {
        key: "robots_unavailable",
        grade: Grade::A,
        rests_on: "Google's robots.txt documentation: a 5xx or 429 robots.txt makes the whole site off limits, and bots that follow the same rules do the same.",
        sources: &[ROBOTS_DOC],
        limits: "Other operators don't all say what they do with a failing robots.txt; Google's rule is the one documented.",
        explain: OPERATOR_DOCUMENTED,
    },
    LedgerEntry {
        key: "bots_blocked",
        grade: Grade::A,
        rests_on: "The site's robots.txt read the way the operators describe, against each bot's token on its operator's page.",
        sources: &[],
        limits: "Says what robots.txt allows, not whether the bot visits; a bot whose operator doesn't promise to follow robots.txt may ignore it.",
        explain: OPERATOR_DOCUMENTED,
    },
    LedgerEntry {
        key: "bots_not_blocked",
        grade: Grade::A,
        rests_on: "The site's robots.txt read the way the operators describe, against each bot's token on its operator's page.",
        sources: &[],
        limits: "Says what robots.txt allows, not whether the bot visits.",
        explain: OPERATOR_DOCUMENTED,
    },
    LedgerEntry {
        key: "answers_restricted",
        grade: Grade::A,
        rests_on: "The page-level controls (robots meta, X-Robots-Tag, data-nosnippet) each engine documents, and only for the engines that document them.",
        sources: &[],
        limits: "Says what the page asks for, not what an engine's answers show.",
        explain: OPERATOR_DOCUMENTED,
    },
    LedgerEntry {
        key: "fetch_blocked",
        grade: Grade::B,
        rests_on: "CodoSEO's synthetic fetch with each bot's documented user agent, compared with CodoSEObot's own fetch of the same page and confirmed by a second one.",
        sources: &[],
        limits: "The requests come from CodoSEO's address, not the bots': a firewall that trusts the real bots' IP ranges may treat them differently.",
        explain: "Evidence grade B: observed by CodoSEO's synthetic check, compared with CodoSEObot's own fetch and confirmed by a second one. Grade A is what the operator documents.",
    },
    LedgerEntry {
        key: "content_missing",
        grade: Grade::AMinus,
        rests_on: "The important pages' HTML as served, against Vercel and MERJ's published measurement that GPTBot, ClaudeBot and PerplexityBot don't run JavaScript.",
        sources: &[NON_RENDERING_SOURCE],
        limits: "A third-party measurement, not the operators' own word; their search and user-triggered fetchers are assumed to fetch the same way.",
        explain: "Evidence grade A-: read from your pages' HTML as served, with Vercel and MERJ's published measurement that these AI crawlers don't run JavaScript. Grade A is what the operator documents.",
    },
    LedgerEntry {
        key: "llms_txt_unavailable",
        grade: Grade::A,
        rests_on: "What /llms.txt answered, judged by Lighthouse's documented llms.txt check, for a site that served one before.",
        sources: &[LLMS_SPEC_URL, LIGHTHOUSE_DOC],
        limits: "Informational: no operator documents depending on llms.txt to crawl, so it never affects the score.",
        explain: "Evidence grade A: what your /llms.txt answered, judged by Lighthouse's documented llms.txt check. llms.txt is informational and never affects your score.",
    },
    LedgerEntry {
        key: "content_signals_missing",
        grade: Grade::A,
        rests_on: "The site's robots.txt read as the Content Signals Policy defines its `Content-Signal` line: search, ai-input and ai-train, each yes or no.",
        sources: &[CONTENT_SIGNALS_URL, CONTENT_SIGNAL_DOC],
        limits: "Declared, not enforced: nothing makes a bot honour the signals. Advisory: it never alerts.",
        explain: "Evidence grade A: your robots.txt, read as the Content Signals Policy defines it. Declared, not enforced.",
    },
    LedgerEntry {
        key: "markdown_unavailable",
        grade: Grade::A,
        rests_on: "What the home page answered to `Accept: text/markdown`, and the Markdown alternate it advertises, as Cloudflare documents Markdown for Agents.",
        sources: &[MARKDOWN_FOR_AGENTS_DOC],
        limits: "Only the home page is asked. No operator documents preferring Markdown when crawling; it saves agents tokens. Advisory: it never alerts.",
        explain: "Evidence grade A: what your home page answered when asked for Markdown. Advisory: no operator documents depending on it.",
    },
    // ---- checks that open no incident ----
    LedgerEntry {
        key: "robots_verdict",
        grade: Grade::A,
        rests_on: "Each bot's robots.txt group and longest-match rule as RFC 9309 and the operators describe them, with the token on the operator's page the registry cites.",
        sources: &[RFC_9309, ROBOTS_DOC],
        limits: "Says what robots.txt allows, not whether the bot visits or obeys.",
        explain: "Evidence grade A: robots.txt read the way the operators document it",
    },
    LedgerEntry {
        key: "eligibility",
        grade: Grade::A,
        rests_on: "The page-level controls each engine's operator documents, counted only for the engines that honour them.",
        sources: &[
            "https://developers.google.com/search/docs/appearance/ai-features",
            "https://blogs.bing.com/webmaster/september-2023/Announcing-new-options-for-webmasters-to-control-usage-of-their-content-in-Bing-Chat",
            "https://support.apple.com/en-us/119829",
            "https://developer.amazon.com/amazonbot",
            "https://developers.openai.com/api/docs/bots",
            "https://support.claude.com/en/articles/8896518-does-anthropic-crawl-data-from-the-web-and-how-can-site-owners-block-the-crawler",
            "https://docs.perplexity.ai/guides/bots",
            "https://developers.facebook.com/docs/sharing/webmasters/web-crawlers",
            "https://duckduckgo.com/duckduckgo-help-pages/results/duckassistbot",
            "https://docs.mistral.ai/robots",
        ],
        limits: "Engines that document no page-level control are always eligible: robots.txt is their only lever.",
        explain: "Evidence grade A: the page-level controls each engine documents",
    },
    LedgerEntry {
        key: "declared_preferences",
        grade: Grade::A,
        rests_on: "Content-Signal, Content-Usage and TDMRep as the site states them, parsed as their specifications define.",
        sources: &[CONTENT_SIGNAL_DOC, CONTENT_USAGE_DOC, TDMREP_DOC],
        limits: "Declared, not enforced: nothing makes a bot honour them, and CodoSEO does not claim any does.",
        explain: "Evidence grade A: declared, not enforced. What your site states, read as the specifications define it",
    },
    LedgerEntry {
        key: "synthetic_probe",
        grade: Grade::B,
        rests_on: "A request with each bot's user agent exactly as its operator prints it, compared with CodoSEObot's fetch of the same page at the same moment.",
        sources: &[],
        limits: "The requests come from CodoSEO's address, not the bots'.",
        explain: "Evidence grade B: observed by CodoSEO's synthetic check, compared with CodoSEObot's own fetch. Grade A is what the operator documents.",
    },
    LedgerEntry {
        key: "content_verdict",
        grade: Grade::AMinus,
        rests_on: "Each important page's HTML as served, before any JavaScript runs, against Vercel and MERJ's published measurement.",
        sources: &[NON_RENDERING_SOURCE],
        limits: "\"Likely shell\" is a word-count and markup heuristic over that HTML.",
        explain: "Evidence grade A-: read from your pages' HTML as served, with a published measurement that these AI crawlers don't run JavaScript.",
    },
    LedgerEntry {
        key: "llms_format",
        grade: Grade::A,
        rests_on: "The format llmstxt.org proposes, and Lighthouse's llms.txt audit for status handling, length and links.",
        sources: &[LLMS_SPEC_URL, LIGHTHOUSE_DOC, LIGHTHOUSE_SOURCE],
        limits: "Informational: llms.txt is a proposal no operator documents depending on.",
        explain: "Evidence grade A: the llmstxt.org format and Lighthouse's documented llms.txt check",
    },
    LedgerEntry {
        key: "llms_size_and_type",
        grade: Grade::C,
        rests_on: "CodoSEO's reading of llmstxt.org's \"small enough to fit in context\" as 100 KiB, and of a Markdown file as text/plain or text/markdown.",
        sources: &[LLMS_SPEC_URL],
        limits: "The proposal sets no size or MIME type; both thresholds are CodoSEO's.",
        explain: "Evidence grade C: inferred by CodoSEO from the llmstxt.org proposal, which sets no number",
    },
];

/// Every entry, findings first.
pub fn ledger() -> &'static [LedgerEntry] {
    &LEDGER
}

/// The entry for a finding kind's slug or a check's name.
pub fn entry(key: &str) -> Option<&'static LedgerEntry> {
    LEDGER.iter().find(|e| e.key == key)
}

/// The entry of a finding kind.
pub fn for_kind(kind: FindingKind) -> &'static LedgerEntry {
    entry(kind.slug()).expect("every finding kind has a ledger entry")
}

/// Where a finding's claim is documented: its kind's fixed sources, then each bot's operator page
/// (as the registry cites it), each engine's documentation and, for a challenge, its vendor's bot
/// docs, each once.
pub fn finding_sources(kind: FindingKind, ev: &Evidence) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let mut push = |u: &str| {
        if !urls.iter().any(|x| x == u) {
            urls.push(u.to_owned());
        }
    };
    for s in for_kind(kind).sources {
        push(s);
    }
    for b in &ev.bots {
        if let Some(bot) = registry().bot(&b.token) {
            push(&bot.source_url);
        }
    }
    for e in &ev.engines {
        if let Some(engine) = engines().iter().find(|x| x.id == e.engine) {
            push(engine.source_url);
        }
    }
    for p in &ev.probes {
        if let Some(bot) = registry().bot(&p.token) {
            push(&bot.source_url);
        }
    }
    let vendors: BTreeSet<&str> = ev
        .probes
        .iter()
        .filter_map(|p| p.vendor)
        .map(|v| v.slug())
        .collect();
    for (vendor, url) in VENDOR_DOCS {
        if vendors.contains(vendor.slug()) {
            push(url);
        }
    }
    urls
}

// ---- lag -------------------------------------------------------------------------------------

/// How long after a fix the operators take to see it, and how sure that is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lag {
    pub hours: u32,
    /// Where the operator says so; `None` when nobody publishes it.
    pub source: Option<&'static str>,
    pub grade: Grade,
}

/// An operator that publishes how long a robots.txt change takes to reach its systems.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperatorLag {
    /// As the registry's `operator` field spells it.
    pub operator: &'static str,
    pub hours: u32,
    pub source: &'static str,
    pub grade: Grade,
}

/// The operators that publish their robots.txt lag.
pub const OPERATOR_LAGS: &[OperatorLag] = &[
    // "it can take ~24 hours from a site's robots.txt update for our systems to adjust" (said of
    // search results).
    OperatorLag {
        operator: "OpenAI",
        hours: 24,
        source: OPENAI_LAG_DOC,
        grade: Grade::A,
    },
    // "the change will take effect after 72 hours".
    OperatorLag {
        operator: "DuckDuckGo",
        hours: 72,
        source: DUCKDUCKGO_LAG_DOC,
        grade: Grade::A,
    },
    // "Google generally caches the contents of robots.txt file for up to 24 hours".
    OperatorLag {
        operator: "Google",
        hours: 24,
        source: ROBOTS_DOC,
        grade: Grade::A,
    },
];

/// The lag assumed for an operator that publishes none: a day, like the ones that do, unverified.
const UNPUBLISHED_LAG: Lag = Lag {
    hours: 24,
    source: None,
    grade: Grade::X,
};

/// For kinds no operator lag applies to: CodoSEO's own debounce before it confirms a fix.
const DEBOUNCE: Lag = Lag {
    hours: 1,
    source: None,
    grade: Grade::X,
};

fn operator_lag(operator: &str) -> Lag {
    OPERATOR_LAGS
        .iter()
        .find(|l| l.operator == operator)
        .map_or(UNPUBLISHED_LAG, |l| Lag {
            hours: l.hours,
            source: Some(l.source),
            grade: l.grade,
        })
}

/// How long after a fix to the finding's cause its operators take to see it: for robots.txt
/// findings, the longest lag among its bots' operators (on a tie, the weakest grade, since the
/// estimate rests on its least documented part); for a broken robots.txt, Google's cache; for the
/// rest, which no operator's robots.txt cache governs, CodoSEO's own one-hour debounce.
pub fn lag(kind: FindingKind, ev: &Evidence) -> Lag {
    match kind {
        FindingKind::BotsBlocked | FindingKind::BotsNotBlocked => ev
            .bots
            .iter()
            .map(|b| {
                let operator = registry()
                    .bot(&b.token)
                    .map_or(b.operator.as_str(), |bot| bot.operator.as_str());
                operator_lag(operator)
            })
            .reduce(|a, b| {
                if (b.hours, a.grade.rank()) > (a.hours, b.grade.rank()) {
                    b
                } else {
                    a
                }
            })
            .unwrap_or(UNPUBLISHED_LAG),
        FindingKind::RobotsUnavailable => operator_lag("Google"),
        FindingKind::AnswersRestricted
        | FindingKind::FetchBlocked
        | FindingKind::ContentMissing
        | FindingKind::LlmsTxtUnavailable
        | FindingKind::ContentSignalsMissing
        | FindingKind::MarkdownUnavailable => DEBOUNCE,
    }
}
