//! Synthetic AI-agent fetch probes, the pure half: what a probe fetch looked like, and whether a
//! response is a bot-manager challenge.
//!
//! The network half lives in `codoseo-crawler`; it fetches each important URL once with
//! CodoSEObot's user agent (the baseline) and once per probed bot with that bot's documented user
//! agent, and records each response as a [`ProbeFetch`]. Everything here is plain data and string
//! matching, so it is tested without a server.

use serde::{Deserialize, Serialize};

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
    (Vendor::Cloudflare, "cf-chl", &["cf-chl"]),
    (Vendor::Cloudflare, "_cf_chl_opt", &["_cf_chl_opt"]),
    (
        Vendor::Cloudflare,
        "challenges.cloudflare.com",
        &["challenges.cloudflare.com"],
    ),
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

/// Generic markers, counted only on a small page.
const GENERIC_MARKERS: &[(&str, &str)] = &[
    ("g-recaptcha", "g-recaptcha"),
    ("hcaptcha.com", "hcaptcha.com"),
    ("cf-turnstile", "cf-turnstile"),
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
