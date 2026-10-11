//! Synthetic AI-agent fetch probes, the pure half: which bots and pages to probe, what a probe
//! fetch looked like, whether a response is a bot-manager challenge, the verdict of one probe
//! against its baseline, and the report stored inside [`AccessReport::probes`].
//!
//! The network half lives in `codoseo-crawler`; it fetches each planned URL once with
//! CodoSEObot's user agent (the baseline) and once per probed bot with that bot's documented user
//! agent, and records each response as a [`ProbeFetch`]. Everything here is plain data and string
//! matching, so it is tested without a server.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::registry::{Bot, Purpose, registry};
use crate::report::{AccessReport, Reason, agent_rules};
use crate::robots::{RobotsTxt, product_token};

pub const PROBE_VERSION: u32 = 1;
/// The most body bytes a probe reads; the body is never kept.
pub const MAX_PROBE_BODY: usize = 256 * 1024;
/// Longest page title kept per fetch, in characters.
pub const MAX_TITLE: usize = 120;
/// Body bytes searched for challenge markers and the title.
const SCAN_BYTES: usize = 64 * 1024;
/// Generic CAPTCHA markers count only on pages smaller than this: a full page carrying a
/// reCAPTCHA form is a page, not a challenge.
const GENERIC_MAX_BYTES: usize = 32 * 1024;

/// The bot manager or CDN that served a challenge or block page.
///
/// Netlify is not listed: no marker of its bot protection could be confirmed against a page
/// Netlify owns, so it would be a guess. A Netlify challenge that shows a CAPTCHA still reads as
/// [`Vendor::Generic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Vendor {
    #[serde(rename = "cloudflare")]
    Cloudflare,
    #[serde(rename = "vercel")]
    Vercel,
    #[serde(rename = "aws_waf")]
    AwsWaf,
    #[serde(rename = "datadome")]
    DataDome,
    #[serde(rename = "human")]
    Human,
    #[serde(rename = "imperva")]
    Imperva,
    #[serde(rename = "akamai")]
    Akamai,
    #[serde(rename = "sucuri")]
    Sucuri,
    /// A CAPTCHA or "verify you are human" page with no vendor marker.
    #[serde(rename = "generic")]
    Generic,
}

impl Vendor {
    /// The fixed lower-case name, as in the saved data and the finding subjects.
    pub fn slug(self) -> &'static str {
        match self {
            Vendor::Cloudflare => "cloudflare",
            Vendor::Vercel => "vercel",
            Vendor::AwsWaf => "aws_waf",
            Vendor::DataDome => "datadome",
            Vendor::Human => "human",
            Vendor::Imperva => "imperva",
            Vendor::Akamai => "akamai",
            Vendor::Sucuri => "sucuri",
            Vendor::Generic => "generic",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Vendor::Cloudflare => "Cloudflare",
            Vendor::Vercel => "Vercel",
            Vendor::AwsWaf => "AWS WAF",
            Vendor::DataDome => "DataDome",
            Vendor::Human => "HUMAN Security",
            Vendor::Imperva => "Imperva",
            Vendor::Akamai => "Akamai",
            Vendor::Sucuri => "Sucuri",
            Vendor::Generic => "CAPTCHA",
        }
    }
}

/// A challenge or block page, and the marker that gave it away (`"cf-mitigated: challenge"`,
/// `"Just a moment..."`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    pub vendor: Vendor,
    pub signal: String,
}

/// Why a fetch got no response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchErrorKind {
    Timeout,
    /// The connection was refused or never established.
    Connect,
    /// The connection was reset or closed before a response.
    Dropped,
    TooManyRedirects,
    /// The SSRF guard refused an address on the way.
    Blocked,
    Other,
}

/// One probe or baseline fetch as the report keeps it: no body, only what the comparison needs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeFetch {
    /// The final response's status; `None` when there was none (see `error`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<FetchErrorKind>,
    /// Where the redirects ended, only when that is not the requested URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub redirects: u8,
    /// Body bytes read, at most [`MAX_PROBE_BODY`].
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub bytes: u32,
    /// The body was longer than [`MAX_PROBE_BODY`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    /// The response said it was HTML.
    #[serde(default, skip_serializing_if = "is_false")]
    pub html: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge: Option<Challenge>,
    /// The first attempt failed (429, 503, a timeout or a connection error) and this is the retry.
    #[serde(default, skip_serializing_if = "is_false")]
    pub retried: bool,
}

fn is_zero_u8(n: &u8) -> bool {
    *n == 0
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Header markers: (vendor, header name, the values that count or `None` for any, and whether
/// the header counts on a 2xx response). DataDome and Imperva add their headers to traffic they let
/// through as well, so on their own those mean a challenge only on an error response.
const HEADER_MARKERS: &[(Vendor, &str, Option<&[&str]>, bool)] = &[
    (
        Vendor::Cloudflare,
        "cf-mitigated",
        Some(&["challenge"]),
        true,
    ),
    (
        Vendor::Vercel,
        "x-vercel-mitigated",
        Some(&["challenge"]),
        true,
    ),
    (
        Vendor::AwsWaf,
        "x-amzn-waf-action",
        Some(&["challenge", "captcha"]),
        true,
    ),
    (Vendor::DataDome, "x-datadome", None, false),
    (Vendor::DataDome, "x-dd-b", None, false),
    (Vendor::Imperva, "x-iinfo", None, false),
    (Vendor::Sucuri, "x-sucuri-block", None, true),
];

/// Body markers by vendor, each `(signal, alternatives)`: the signal is reported, any alternative
/// (lower-cased, as some vendors write the page with character references) matches. Searched in
/// this order, after the headers and before the generic ones.
const BODY_MARKERS: &[(Vendor, &str, &[&str])] = &[
    (
        Vendor::Cloudflare,
        "Just a moment...",
        &["just a moment..."],
    ),
    (Vendor::Cloudflare, "_cf_chl_opt", &["_cf_chl_opt"]),
    (
        Vendor::Cloudflare,
        "Attention Required! | Cloudflare",
        &["attention required! | cloudflare"],
    ),
    (
        Vendor::Cloudflare,
        "cf-error-details",
        &["cf-error-details"],
    ),
    (Vendor::Cloudflare, "Error 1020", &["error 1020"]),
    (Vendor::Cloudflare, "Error 1010", &["error 1010"]),
    (
        Vendor::Vercel,
        "Vercel Security Checkpoint",
        &["vercel security checkpoint"],
    ),
    (
        Vendor::DataDome,
        "captcha-delivery.com",
        &["captcha-delivery.com"],
    ),
    (Vendor::Human, "_pxCaptcha", &["_pxcaptcha"]),
    (Vendor::Human, "px-captcha", &["px-captcha"]),
    (Vendor::Human, "/_px/", &["/_px/"]),
    (Vendor::Human, "Press & Hold", &["press & hold"]),
    (Vendor::Human, "Press &amp; Hold", &["press &amp; hold"]),
    (
        Vendor::Imperva,
        "_Incapsula_Resource",
        &["_incapsula_resource"],
    ),
    (
        Vendor::Imperva,
        "Incapsula incident ID",
        &["incapsula incident id"],
    ),
    (
        Vendor::Akamai,
        "errors.edgesuite.net",
        &["errors.edgesuite.net", "errors&#46;edgesuite&#46;net"],
    ),
    (
        Vendor::Sucuri,
        "Sucuri WebSite Firewall - Access Denied",
        &["sucuri website firewall - access denied"],
    ),
];

/// Generic markers, counted only on a small page. `challenges.cloudflare.com` and `cf-chl` are
/// Cloudflare Turnstile's script host and widget ids: an ordinary page with a Turnstile form
/// carries them (and a prerendered copy of it carries the widget), while Cloudflare's own
/// challenge page is caught by its title, `_cf_chl_opt` and `cf-mitigated`.
const GENERIC_MARKERS: &[(&str, &str)] = &[
    ("g-recaptcha", "g-recaptcha"),
    ("hcaptcha.com", "hcaptcha.com"),
    ("cf-turnstile", "cf-turnstile"),
    ("challenges.cloudflare.com", "challenges.cloudflare.com"),
    ("cf-chl", "cf-chl"),
    ("verify you are human", "verify you are human"),
    (
        "enable JavaScript and cookies",
        "enable javascript and cookies",
    ),
];

/// Whether a response is a bot-manager challenge or block page, and whose. Headers are checked
/// first, then the first 64 KiB of the body, ignoring ASCII case. Generic CAPTCHA markers count
/// only on a page under 32 KiB.
pub fn detect_challenge(
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
) -> Option<Challenge> {
    let ok = (200..300).contains(&status);
    let found = |vendor: Vendor, signal: String| Some(Challenge { vendor, signal });
    for (vendor, name, values, on_ok) in HEADER_MARKERS {
        if ok && !on_ok {
            continue;
        }
        for (k, v) in headers {
            if !k.trim().eq_ignore_ascii_case(name) {
                continue;
            }
            match values {
                None => return found(*vendor, (*name).to_owned()),
                Some(values) => {
                    let v = v.trim().to_ascii_lowercase();
                    if values.contains(&v.as_str()) {
                        return found(*vendor, format!("{name}: {v}"));
                    }
                }
            }
        }
    }

    let text = body[..body.len().min(SCAN_BYTES)].to_ascii_lowercase();
    let has = |needle: &str| contains(&text, needle.as_bytes());
    let akamai_server = headers.iter().any(|(k, v)| {
        k.trim().eq_ignore_ascii_case("server") && v.trim().eq_ignore_ascii_case("AkamaiGHost")
    });
    if akamai_server && has("access denied") && (has("reference #") || has("reference&#32;&#35;")) {
        return found(Vendor::Akamai, "AkamaiGHost: Access Denied".to_owned());
    }
    for (vendor, signal, needles) in BODY_MARKERS {
        if needles.iter().any(|n| has(n)) {
            return found(*vendor, (*signal).to_owned());
        }
    }

    if body.len() < GENERIC_MAX_BYTES {
        for (signal, needle) in GENERIC_MARKERS {
            if has(needle) {
                return found(Vendor::Generic, (*signal).to_owned());
            }
        }
        if page_title(body).is_some_and(|t| t.to_ascii_lowercase().contains("captcha")) {
            return found(Vendor::Generic, "<title> captcha".to_owned());
        }
    }
    None
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || hay.windows(needle.len()).any(|w| w == needle)
}

/// The page's `<title>` text from the first 64 KiB, with the common character references decoded,
/// whitespace collapsed and cut to [`MAX_TITLE`] characters; `None` when there is none or it is
/// blank. A light scan, not an HTML parse.
pub fn page_title(body: &[u8]) -> Option<String> {
    let body = &body[..body.len().min(SCAN_BYTES)];
    let lower = body.to_ascii_lowercase();
    let mut from = 0;
    let open = loop {
        let at = from + find(&lower[from..], b"<title")?;
        let next = lower.get(at + 6).copied();
        if matches!(next, Some(b'>' | b'/') | Some(b' ' | b'\t' | b'\n' | b'\r')) {
            break at;
        }
        from = at + 6;
    };
    let start = open + find(&lower[open..], b">")? + 1;
    let end = start + find(&lower[start..], b"</title")?;
    let raw = String::from_utf8_lossy(&body[start..end]);
    let decoded = raw
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&");
    let title: String = decoded
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_TITLE)
        .collect();
    (!title.is_empty()).then_some(title)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---- Who and what is probed -----------------------------------------------------------------

/// Search engines verified by reverse DNS: bot managers refuse a request that claims to be one
/// from another address, so probing them would always be a false positive.
pub const NEVER_PROBE: [&str; 3] = ["Googlebot", "Bingbot", "Applebot"];

/// Why a crawling registry bot is not probed. Control tokens never make requests and are not
/// listed at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Its operator prints no full user agent, and one is never made up.
    NoUserAgent,
    /// One of [`NEVER_PROBE`].
    VerifiedSearchEngine,
    /// Not a search or user-triggered fetcher: training and ads crawlers, and agents, which
    /// authenticate with signatures a copy of their user agent can't carry.
    NotProbedPurpose,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedBot {
    pub token: String,
    pub reason: SkipReason,
}

/// A bot the probes impersonate, with its user agent exactly as its operator prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeBot {
    pub token: String,
    pub user_agent: String,
}

/// Whether a registry bot is the kind the probes impersonate: a crawling search or user-fetch bot
/// that is not a verified search engine. Whether its operator documents a user agent is a separate
/// question (see [`candidates`]); the registry lint allows a `user_agent` only on these.
pub fn is_probe_candidate(bot: &Bot) -> bool {
    skip_kind(bot).is_none()
}

fn skip_kind(bot: &Bot) -> Option<SkipReason> {
    if !matches!(bot.purpose, Purpose::Search | Purpose::UserFetch) || !bot.crawls {
        Some(SkipReason::NotProbedPurpose)
    } else if NEVER_PROBE
        .iter()
        .any(|n| n.eq_ignore_ascii_case(&bot.token))
    {
        Some(SkipReason::VerifiedSearchEngine)
    } else {
        None
    }
}

/// The bots to probe, in registry order, and every other crawling registry bot with the reason it
/// is not probed.
pub fn candidates() -> (Vec<ProbeBot>, Vec<SkippedBot>) {
    let mut bots = Vec::new();
    let mut skipped = Vec::new();
    for bot in registry().bots.iter().filter(|b| b.crawls) {
        let reason = match (skip_kind(bot), &bot.user_agent) {
            (Some(reason), _) => reason,
            (None, Some(ua)) => {
                bots.push(ProbeBot {
                    token: bot.token.clone(),
                    user_agent: ua.clone(),
                });
                continue;
            }
            (None, None) => SkipReason::NoUserAgent,
        };
        skipped.push(SkippedBot {
            token: bot.token.clone(),
            reason,
        });
    }
    (bots, skipped)
}

/// What one probe run fetches.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbePlan {
    /// Indexes into `AccessReport::important`: the home page, then starred pages, then the most
    /// linked, HTML 200 pages only.
    pub urls: Vec<u16>,
    pub bots: Vec<ProbeBot>,
    pub skipped_bots: Vec<SkippedBot>,
    /// The longest `Crawl-delay` among CodoSEObot and the probed bots, in seconds; not capped here.
    pub crawl_delay: Option<f64>,
}

/// The probe plan for a report, up to `max_urls` pages. `None` when robots.txt gave no verdicts,
/// `max_urls` is 0 or no important page is an HTML 200. `robots` is the parsed file the report was
/// built from (`None` when it is missing), read only for the crawl delay.
pub fn plan_probes(
    report: &AccessReport,
    robots: Option<&RobotsTxt>,
    max_urls: usize,
) -> Option<ProbePlan> {
    if !report.has_verdicts() || max_urls == 0 {
        return None;
    }
    let mut urls: Vec<(u8, u16)> = report
        .important
        .iter()
        .enumerate()
        .filter(|(_, u)| u.html_ok)
        .map(|(i, u)| {
            let rank = match u.reason {
                Reason::Home => 0,
                Reason::Starred => 1,
                Reason::MostLinked => 2,
            };
            (rank, i as u16)
        })
        .collect();
    // Stable: within a rank the report's order (inlinks, then URL) stays.
    urls.sort_by_key(|(rank, _)| *rank);
    let urls: Vec<u16> = urls.into_iter().take(max_urls).map(|(_, i)| i).collect();
    if urls.is_empty() {
        return None;
    }
    let (bots, skipped_bots) = candidates();
    let baseline = product_token(codoseo_core::crawl::USER_AGENT);
    let crawl_delay = robots.and_then(|robots| {
        std::iter::once(baseline.as_str())
            .chain(bots.iter().map(|b| b.token.as_str()))
            .filter_map(|token| agent_rules(robots, token).crawl_delay())
            .max_by(f64::total_cmp)
    });
    Some(ProbePlan {
        urls,
        bots,
        skipped_bots,
        crawl_delay,
    })
}

/// Robots.txt disallows `token` from the important URL `url_idx`, per the report's verdicts: the
/// pair is not requested.
pub fn robots_skips(report: &AccessReport, url_idx: u16, token: &str) -> bool {
    report
        .bot(token)
        .is_some_and(|b| b.blocked.iter().any(|x| x.url == url_idx))
}

// ---- Verdicts -------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InconclusiveReason {
    /// CodoSEObot itself got no 2xx HTML page, so there is nothing to compare with.
    BaselineFailed,
    BaselineChallenged,
    /// The probe got no response (a timeout, too many redirects, the SSRF guard).
    ProbeError,
    /// The confirming re-fetch disagreed with the first.
    Unstable,
    /// A worse result the request cap or the time budget left no room to confirm with a second
    /// fetch. Grade B means confirmed, so a single fetch is not reported.
    Unconfirmed,
}

/// What one probe says about one bot on one page, compared with the baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Verdict {
    Same,
    /// An error status, or `None` when the connection was dropped or refused on both tries.
    Blocked {
        status: Option<u16>,
    },
    Challenged {
        vendor: Vendor,
    },
    /// Redirected somewhere the baseline was not.
    Diverted {
        to: String,
    },
    /// A 2xx page with another title that is much smaller or has no title: the misleading 200.
    Degraded,
    Throttled,
    Inconclusive {
        reason: InconclusiveReason,
    },
    /// Robots.txt disallows the bot here, so it was not requested.
    SkippedRobots,
}

impl Verdict {
    /// The site treats the bot worse than CodoSEObot.
    pub fn is_positive(&self) -> bool {
        self.cause_slug().is_some()
    }

    /// The finding subject a positive verdict groups under; `None` for the others.
    pub fn cause_slug(&self) -> Option<&'static str> {
        Some(match self {
            Verdict::Challenged { vendor } => match vendor {
                Vendor::Cloudflare => "challenge_cloudflare",
                Vendor::Vercel => "challenge_vercel",
                Vendor::AwsWaf => "challenge_aws_waf",
                Vendor::DataDome => "challenge_datadome",
                Vendor::Human => "challenge_human",
                Vendor::Imperva => "challenge_imperva",
                Vendor::Akamai => "challenge_akamai",
                Vendor::Sucuri => "challenge_sucuri",
                Vendor::Generic => "challenge_generic",
            },
            Verdict::Blocked { status } => match status {
                None => "blocked_dropped",
                Some(401) => "blocked_401",
                Some(403) => "blocked_403",
                Some(404) => "blocked_404",
                Some(451) => "blocked_451",
                Some(s) if *s >= 500 => "blocked_5xx",
                Some(_) => "blocked_4xx",
            },
            Verdict::Diverted { .. } => "diverted",
            Verdict::Degraded => "degraded",
            Verdict::Throttled => "throttled",
            Verdict::Same | Verdict::Inconclusive { .. } | Verdict::SkippedRobots => return None,
        })
    }
}

impl ProbeFetch {
    /// A 2xx HTML response.
    pub fn is_html_ok(&self) -> bool {
        self.error.is_none() && self.html && self.status.is_some_and(|s| (200..300).contains(&s))
    }
}

/// The verdict for one probe of `requested` against the baseline fetched the same way with
/// CodoSEObot's user agent. The first rule that matches wins:
///
/// 1. The baseline is challenged, or is not a 2xx HTML page: inconclusive. A generic CAPTCHA
///    marker on a 2xx baseline does not count as a challenge: small pages with a contact form
///    carry one, and the probe is compared against it instead (rule 3).
/// 2. The probe got no response: a connection refused or dropped on both tries is a block, any
///    other failure is inconclusive.
/// 3. The probe is a challenge page and the baseline is not: challenged, whatever the status. A
///    generic marker the baseline also carries is the page's own and is passed over, so a 403,
///    redirect or smaller page behind it is still caught by the rules below.
/// 4. 429: throttled.
/// 5. Any other status from 400: blocked.
/// 6. It ended at another URL than the baseline, ignoring the fragment, a trailing slash and an
///    http→https upgrade: diverted.
/// 7. Both 2xx, the titles differ (ignoring case and spacing), and the probe is under half the
///    baseline's size or has no title where the baseline has one: degraded.
/// 8. Otherwise the same; the same title at another size is dynamic content or prerendering.
pub fn classify(requested: &str, baseline: &ProbeFetch, probe: &ProbeFetch) -> Verdict {
    let base_ok = baseline.status.is_some_and(|s| (200..300).contains(&s));
    let base_generic = baseline
        .challenge
        .as_ref()
        .is_some_and(|c| c.vendor == Vendor::Generic);
    if baseline.challenge.is_some() && !(base_generic && base_ok) {
        return Verdict::Inconclusive {
            reason: InconclusiveReason::BaselineChallenged,
        };
    }
    if !baseline.is_html_ok() {
        return Verdict::Inconclusive {
            reason: InconclusiveReason::BaselineFailed,
        };
    }
    let Some(status) = probe.status else {
        return match probe.error {
            Some(FetchErrorKind::Connect | FetchErrorKind::Dropped) if probe.retried => {
                Verdict::Blocked { status: None }
            }
            _ => Verdict::Inconclusive {
                reason: InconclusiveReason::ProbeError,
            },
        };
    };
    if let Some(c) = &probe.challenge
        && !(c.vendor == Vendor::Generic && base_generic)
    {
        return Verdict::Challenged { vendor: c.vendor };
    }
    if status == 429 {
        return Verdict::Throttled;
    }
    if status >= 400 {
        return Verdict::Blocked {
            status: Some(status),
        };
    }
    let base_url = baseline.final_url.as_deref().unwrap_or(requested);
    let probe_url = probe.final_url.as_deref().unwrap_or(requested);
    if url_key(base_url) != url_key(probe_url) {
        return Verdict::Diverted {
            to: probe_url.to_owned(),
        };
    }
    if (200..300).contains(&status) {
        let base_title = norm_title(baseline.title.as_deref());
        let probe_title = norm_title(probe.title.as_deref());
        let smaller = u64::from(probe.bytes) * 2 < u64::from(baseline.bytes);
        let lost_title = probe_title.is_none() && base_title.is_some();
        if base_title != probe_title && (smaller || lost_title) {
            return Verdict::Degraded;
        }
    }
    Verdict::Same
}

/// A URL for comparison: no fragment, https for http, no trailing slash on the path. Unparseable
/// URLs compare as written.
fn url_key(raw: &str) -> String {
    let Ok(mut u) = Url::parse(raw) else {
        return raw.to_owned();
    };
    u.set_fragment(None);
    if u.scheme() == "http" {
        let _ = u.set_scheme("https");
    }
    let path = u.path().trim_end_matches('/').to_owned();
    format!(
        "{}://{}:{}{}?{}",
        u.scheme(),
        u.host_str().unwrap_or(""),
        u.port_or_known_default().unwrap_or(0),
        path,
        u.query().unwrap_or("")
    )
}

fn norm_title(t: Option<&str>) -> Option<String> {
    t.map(|t| {
        t.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    })
    .filter(|t| !t.is_empty())
}

// ---- The report -----------------------------------------------------------------------------

/// One bot's probe of one page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub token: String,
    /// The probe as fetched (the confirming re-fetch, when there was one); `None` when it was not
    /// requested: robots.txt disallows it, or the baseline gave nothing to compare with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch: Option<ProbeFetch>,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbedUrl {
    /// Index into `AccessReport::important`.
    pub url: u16,
    pub baseline: ProbeFetch,
    #[serde(default)]
    pub results: Vec<ProbeResult>,
}

/// Why a probe run stopped before its last planned URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStop {
    Budget,
    RequestCap,
    Cancelled,
}

/// The synthetic fetch probes of one crawl, stored in [`AccessReport::probes`]. Synthetic: the
/// requests came from CodoSEO's address with the bots' user agents, not from the bots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeReport {
    pub version: u32,
    /// Always true; written so every reader of the JSON sees it.
    pub synthetic: bool,
    /// The `From` header every probe and baseline carried.
    pub from: String,
    /// The probed tokens, in plan order.
    #[serde(default)]
    pub bots: Vec<String>,
    #[serde(default)]
    pub skipped_bots: Vec<SkippedBot>,
    /// The URLs probed, in plan order; a URL is probed for every bot or not at all.
    #[serde(default)]
    pub urls: Vec<ProbedUrl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<ProbeStop>,
    /// Requests sent, retries and confirmations included.
    #[serde(default)]
    pub requests: u16,
}

impl ProbeReport {
    /// An empty report for `plan`, before any URL is probed.
    pub fn new(from: &str, plan: &ProbePlan) -> ProbeReport {
        ProbeReport {
            version: PROBE_VERSION,
            synthetic: true,
            from: from.to_owned(),
            bots: plan.bots.iter().map(|b| b.token.clone()).collect(),
            skipped_bots: plan.skipped_bots.clone(),
            urls: Vec::new(),
            stop: None,
            requests: 0,
        }
    }

    /// At least one probe got a real verdict (neither inconclusive nor skipped by robots.txt), so
    /// the report can judge whether the site blocks AI bots.
    pub fn conclusive(&self) -> bool {
        self.urls.iter().flat_map(|u| &u.results).any(|r| {
            !matches!(
                r.verdict,
                Verdict::Inconclusive { .. } | Verdict::SkippedRobots
            )
        })
    }

    /// The probed bots this run can speak for: every page they were sent to (or skipped by
    /// robots.txt) got a real verdict. A bot inconclusive on any page (the baseline failed or
    /// was challenged, no response, a re-fetch that disagreed) is not judged, so an open
    /// incident keeps what it last knew about it.
    pub fn judged_bots(&self) -> Vec<String> {
        self.bots
            .iter()
            .filter(|token| {
                let mut results = self
                    .urls
                    .iter()
                    .flat_map(|u| &u.results)
                    .filter(|r| &&r.token == token)
                    .peekable();
                results.peek().is_some()
                    && results.all(|r| !matches!(r.verdict, Verdict::Inconclusive { .. }))
            })
            .cloned()
            .collect()
    }
}
