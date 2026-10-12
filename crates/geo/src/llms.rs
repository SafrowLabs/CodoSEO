//! `/llms.txt`: whether a site serves one and how it holds up against the format. Informational
//! only: never scored, never in the health score, never Critical.
//!
//! Two sources set the rules, each where it is the authority:
//!
//! - **Status handling follows Lighthouse's `llms-txt` audit** (Agentic Browsing category, 13.3;
//!   [`LIGHTHOUSE_DOC`], source [`LIGHTHOUSE_SOURCE`]): a 5xx or 429, or a fetch that got no
//!   response, fails; any other 4xx is not applicable, because the file is optional. Lighthouse
//!   takes a 2xx at face value; we also call a 2xx that serves HTML a soft 404 and treat it like a
//!   404 ([`LlmsState::SoftNotFound`]), since an HTML page in place of a Markdown file is the
//!   site's catch-all, not an llms.txt.
//! - **The format follows llmstxt.org** ([`SPEC_URL`]): an optional BOM, then an H1 with the
//!   site's name (the only required part), a `>` blockquote summary, any non-heading details, and
//!   H2 sections of "file lists", items `- [name](url)` optionally followed by `: notes`; an H2
//!   named `Optional` holds links an agent may skip. The spec sets no size or MIME type, only that
//!   the file "stays small enough to fit in context". Lighthouse's own content rules (an H1
//!   anywhere, at least one Markdown link, at least [`MIN_CHARS`] characters) are kept as the
//!   [`LlmsCheckId::Links`] and [`LlmsCheckId::Length`] checks.
//!
//! [`parse`] reads a body, [`links`] lists what it points to and [`assess`] turns a fetch into
//! an [`LlmsTxtReport`]. Whether a site has adopted llms.txt carries over from the previous
//! report ([`carry_adoption`]): only a site that served one before can lose it.

use std::collections::HashSet;

use codoseo_core::output::LlmsTxtFetch;
use codoseo_core::url::normalize;
use serde::{Deserialize, Serialize};

/// The llms.txt proposal.
pub const SPEC_URL: &str = "https://llmstxt.org/";
/// Lighthouse's documentation of its `llms-txt` audit.
pub const LIGHTHOUSE_DOC: &str =
    "https://developer.chrome.com/docs/lighthouse/agentic-browsing/llms-txt";
/// Lighthouse's `llms-txt` audit, whose status and content rules [`assess`] mirrors.
pub const LIGHTHOUSE_SOURCE: &str =
    "https://github.com/GoogleChrome/lighthouse/blob/main/core/audits/agentic/llms-txt.js";

/// A file larger than this gets a size notice: llmstxt.org wants it small enough to fit in context.
pub const NOTICE_BYTES: u64 = 100 * 1024;
/// The crawler reads at most this much of the file.
pub const MAX_BYTES: usize = 512 * 1024;
/// Lighthouse calls a file shorter than this (in characters) "suspiciously short".
pub const MIN_CHARS: usize = 50;
/// Links [`parse`] keeps from one file; the rest are ignored. A real llms.txt lists hundreds.
pub const MAX_LINKS: usize = 10_000;
/// Failing link URLs a report keeps.
const MAX_FAILING_URLS: usize = 10;

/// One Markdown link `[name](url)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmsLink {
    pub name: String,
    pub url: String,
}

/// One H2 section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmsSection {
    pub title: String,
    /// Its file-list items: list entries that start with a link.
    pub links: Vec<LlmsLink>,
    /// Lines of anything else (prose, plain list items, code), headings below H2 left out.
    pub other_lines: u32,
}

/// An llms.txt body as llmstxt.org lays it out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LlmsDoc {
    /// The text of the first H1.
    pub title: Option<String>,
    /// The first H1 is the first thing in the file (after a BOM and blank lines).
    pub h1_first: bool,
    /// The `>` blockquote right after the H1, lines joined by spaces.
    pub summary: Option<String>,
    pub sections: Vec<LlmsSection>,
    /// Every link's URL in file order, inside sections or not, duplicates kept; at most
    /// [`MAX_LINKS`].
    pub urls: Vec<String>,
    /// Characters after the BOM.
    pub chars: usize,
}

impl LlmsDoc {
    /// The H2 section named `Optional` (any case).
    pub fn optional(&self) -> Option<&LlmsSection> {
        self.sections
            .iter()
            .find(|s| s.title.eq_ignore_ascii_case("optional"))
    }
}

/// Reads an llms.txt body. Lines end in LF or CRLF; a leading BOM is skipped; nothing inside a
/// fenced code block counts as a heading, quote or link.
pub fn parse(body: &str) -> LlmsDoc {
    let text = body.strip_prefix('\u{feff}').unwrap_or(body);
    let mut doc = LlmsDoc {
        chars: text.chars().count(),
        ..LlmsDoc::default()
    };
    let mut seen = false;
    let mut in_fence = false;
    // Only blank lines since the H1, so a blockquote here is its summary.
    let mut want_summary = false;
    let mut in_summary = false;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r').trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            in_fence = !in_fence;
            seen = true;
            want_summary = false;
            in_summary = false;
            if let Some(s) = doc.sections.last_mut() {
                s.other_lines += 1;
            }
            continue;
        }
        if line.is_empty() {
            in_summary = false;
            continue;
        }
        if in_fence {
            if let Some(s) = doc.sections.last_mut() {
                s.other_lines += 1;
            }
            continue;
        }
        let was_seen = seen;
        seen = true;
        if let Some((level, text)) = heading(line) {
            want_summary = false;
            in_summary = false;
            match level {
                1 if doc.title.is_none() => {
                    doc.title = Some(text.to_owned());
                    doc.h1_first = !was_seen;
                    want_summary = true;
                }
                2 => doc.sections.push(LlmsSection {
                    title: text.to_owned(),
                    links: Vec::new(),
                    other_lines: 0,
                }),
                // A second H1 is out of place; H3 and below subdivide a section.
                1 => {
                    if let Some(s) = doc.sections.last_mut() {
                        s.other_lines += 1;
                    }
                }
                _ => {}
            }
            continue;
        }
        let found = inline_links(line, MAX_LINKS - doc.urls.len());
        doc.urls.extend(found.into_iter().map(|l| l.url));
        if let Some(quote) = line.strip_prefix('>')
            && (want_summary || in_summary)
        {
            let quote = quote.trim();
            match &mut doc.summary {
                Some(s) if !quote.is_empty() => {
                    s.push(' ');
                    s.push_str(quote);
                }
                Some(_) => {}
                None => doc.summary = Some(quote.to_owned()),
            }
            want_summary = false;
            in_summary = true;
            continue;
        }
        want_summary = false;
        in_summary = false;
        let Some(section) = doc.sections.last_mut() else {
            continue;
        };
        match list_link(line) {
            Some(link) => section.links.push(link),
            None => section.other_lines += 1,
        }
    }
    doc
}

/// The level and text of an ATX heading (`#` to `######`, then whitespace).
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &line[level..];
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim();
    (!text.is_empty()).then_some((level, text))
}

/// A list item (`-`, `*` or `+`) that starts with a link: a file-list entry.
fn list_link(line: &str) -> Option<LlmsLink> {
    let rest = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))?
        .trim_start();
    if !rest.starts_with('[') {
        return None;
    }
    let (link, _) = link_at(rest, &Closers::of(rest), 0)?;
    Some(link)
}

/// Every `[name](url)` on one line, images included (`![alt](src)` is a link too for Lighthouse),
/// at most `limit`. Linear in the line's length: brackets are matched once up front.
fn inline_links(line: &str, limit: usize) -> Vec<LlmsLink> {
    let mut out = Vec::new();
    if limit == 0 || !line.contains('[') {
        return out;
    }
    let closers = Closers::of(line);
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && out.len() < limit {
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        match link_at(line, &closers, i) {
            Some((link, end)) => {
                out.push(link);
                i = end;
            }
            None => i += 1,
        }
    }
    out
}

/// Where each `[` and `(` on a line closes, nesting balanced: one pass with a stack each, so no
/// opening bracket is ever scanned for twice (a line of `[[[[` or `[a](` stays linear).
struct Closers {
    square: Vec<usize>,
    round: Vec<usize>,
}

impl Closers {
    const NONE: usize = usize::MAX;

    fn of(line: &str) -> Closers {
        let bytes = line.as_bytes();
        let mut square = vec![Self::NONE; bytes.len()];
        let mut round = vec![Self::NONE; bytes.len()];
        let mut squares = Vec::new();
        let mut rounds = Vec::new();
        for (i, &b) in bytes.iter().enumerate() {
            match b {
                b'[' => squares.push(i),
                b']' => {
                    if let Some(open) = squares.pop() {
                        square[open] = i;
                    }
                }
                b'(' => rounds.push(i),
                b')' => {
                    if let Some(open) = rounds.pop() {
                        round[open] = i;
                    }
                }
                _ => {}
            }
        }
        Closers { square, round }
    }
}

/// The link `[name](url)` whose `[` is at byte `at` of `s`, and the byte just past it. The URL is
/// the destination without a `"title"` or angle brackets; nested brackets and parentheses are
/// balanced.
fn link_at(s: &str, closers: &Closers, at: usize) -> Option<(LlmsLink, usize)> {
    let bytes = s.as_bytes();
    if bytes.get(at) != Some(&b'[') {
        return None;
    }
    let close = closers.square[at];
    if close == Closers::NONE || bytes.get(close + 1) != Some(&b'(') {
        return None;
    }
    let end = closers.round[close + 1];
    if end == Closers::NONE {
        return None;
    }
    let start = close + 2;
    let dest = s[start..end].trim();
    let url = match dest.strip_prefix('<') {
        Some(inner) => inner.split('>').next().unwrap_or(""),
        None => dest.split_whitespace().next().unwrap_or(""),
    };
    if url.is_empty() {
        return None;
    }
    Some((
        LlmsLink {
            name: s[at + 1..close].trim().to_owned(),
            url: url.to_owned(),
        },
        end + 1,
    ))
}

/// The URLs worth checking, in file order without duplicates: absolute `http(s)` URLs and
/// relative references (to resolve against the file's own URL). Fragment-only links and other
/// schemes (`mailto:`, `javascript:`) are left out.
pub fn links(doc: &LlmsDoc) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for url in doc.urls.iter().take(MAX_LINKS) {
        if url.starts_with('#') || !seen.insert(url.as_str()) {
            continue;
        }
        if let Some(colon) = url.find(':') {
            let scheme = &url[..colon];
            let is_scheme = !scheme.is_empty()
                && !scheme.contains(['/', '?', '#'])
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
            if is_scheme
                && !scheme.eq_ignore_ascii_case("http")
                && !scheme.eq_ignore_ascii_case("https")
            {
                continue;
            }
        }
        out.push(url.clone());
    }
    out
}

/// What fetching `/llms.txt` gave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmsState {
    /// A 4xx other than 429: the site has no llms.txt. Not applicable, as in Lighthouse.
    Absent,
    /// A 2xx that serves an HTML page: the site's catch-all, read as a 404.
    SoftNotFound,
    /// A 2xx with a file.
    Available,
    /// A 5xx, a 429, another unexpected status, or no response at all.
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmsCheckId {
    /// An H1 with the site's name comes first (llmstxt.org); Lighthouse wants an H1 anywhere.
    H1,
    /// A `>` blockquote summary follows the H1 (llmstxt.org).
    Summary,
    /// At least one H2 section is a file list of `- [name](url)` items (llmstxt.org).
    Sections,
    /// The file has at least one Markdown link (Lighthouse).
    Links,
    /// The file has at least [`MIN_CHARS`] characters (Lighthouse).
    Length,
    /// The file is at most [`NOTICE_BYTES`] and was read whole.
    Size,
    /// It is served as `text/plain` or `text/markdown`.
    Mime,
    /// Every checked link answered 2xx or 3xx.
    LinksResolve,
}

impl LlmsCheckId {
    /// A short name for tables and lists.
    pub fn label(self) -> &'static str {
        match self {
            LlmsCheckId::H1 => "Starts with an H1",
            LlmsCheckId::Summary => "Summary under the H1",
            LlmsCheckId::Sections => "Sections of links",
            LlmsCheckId::Links => "Has links",
            LlmsCheckId::Length => "Long enough",
            LlmsCheckId::Size => "Size",
            LlmsCheckId::Mime => "Served as text",
            LlmsCheckId::LinksResolve => "Links resolve",
        }
    }
}

impl LlmsState {
    /// `Available`, `Not found`, ... in words.
    pub fn label(self) -> &'static str {
        match self {
            LlmsState::Absent => "No llms.txt",
            LlmsState::SoftNotFound => "No llms.txt (soft 404)",
            LlmsState::Available => "Available",
            LlmsState::Unavailable => "Unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmsCheck {
    pub id: LlmsCheckId,
    pub ok: bool,
    /// What was found, in a sentence.
    pub detail: String,
}

/// How the links listed in the file fared.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkSummary {
    /// Links with a status: from the crawl or a request of their own.
    pub checked: u16,
    /// Checked links that answered 4xx or 5xx, or not at all.
    pub failing: u16,
    /// Links past the check limit or on another origin.
    pub not_checked: u16,
    /// Up to ten failing links, in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failing_urls: Vec<String>,
}

/// The llms.txt part of an access report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmsTxtReport {
    pub state: LlmsState,
    /// The final status; 0 when no response arrived.
    pub status: u16,
    /// Why no response arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The site serves, or served before this failure, an llms.txt: see [`carry_adoption`].
    pub adopted: bool,
    /// The file's H1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// An `## Optional` section lists links agents may skip.
    #[serde(default)]
    pub optional_section: bool,
    /// Empty unless `state` is `Available`.
    #[serde(default)]
    pub checks: Vec<LlmsCheck>,
    #[serde(default)]
    pub link_summary: LinkSummary,
}

impl LlmsTxtReport {
    pub fn check(&self, id: LlmsCheckId) -> Option<&LlmsCheck> {
        self.checks.iter().find(|c| c.id == id)
    }
}

impl LlmsTxtReport {
    /// One plain line about the file, for the CLI and the MCP server. Informational: a missing
    /// file is optional and nothing to fix.
    pub fn summary(&self) -> String {
        match self.state {
            LlmsState::Absent => format!(
                "No llms.txt (HTTP {}): it is optional, so there is nothing to fix.",
                self.status
            ),
            LlmsState::SoftNotFound => format!(
                "No llms.txt: /llms.txt answered HTTP {} with a web page instead of a file. It is optional, so there is nothing to fix.",
                self.status
            ),
            LlmsState::Unavailable => match (&self.error, self.status) {
                (Some(e), _) => format!("/llms.txt could not be fetched ({e})."),
                (None, 0) => "/llms.txt could not be fetched.".to_owned(),
                (None, s) => format!("/llms.txt answered HTTP {s}."),
            },
            LlmsState::Available => {
                let failed = self.checks.iter().filter(|c| !c.ok).count();
                let l = &self.link_summary;
                let mut text = if failed == 0 {
                    "llms.txt is available and passes every check.".to_owned()
                } else {
                    format!(
                        "llms.txt is available; {failed} of {} checks don't pass.",
                        self.checks.len()
                    )
                };
                text.push_str(&format!(
                    " Links: {} checked, {} failing, {} not checked.",
                    l.checked, l.failing, l.not_checked
                ));
                text
            }
        }
    }
}

/// The state a fetch puts the file in, by Lighthouse's status rules plus soft-404 detection.
pub fn state_of(fetch: &LlmsTxtFetch) -> LlmsState {
    match fetch.status {
        _ if fetch.error.is_some() => LlmsState::Unavailable,
        429 => LlmsState::Unavailable,
        400..=499 => LlmsState::Absent,
        200..=299 if serves_html(fetch) => LlmsState::SoftNotFound,
        200..=299 => LlmsState::Available,
        _ => LlmsState::Unavailable,
    }
}

/// The body is an HTML document, or it is served as HTML and has no Markdown heading. A real
/// llms.txt mis-served as `text/html` stays a file, and fails only the MIME check.
fn serves_html(fetch: &LlmsTxtFetch) -> bool {
    let body = fetch.body.strip_prefix('\u{feff}').unwrap_or(&fetch.body);
    let start: String = body.trim_start().chars().take(16).collect();
    let start = start.to_ascii_lowercase();
    if ["<!doctype html", "<html", "<head", "<body"]
        .iter()
        .any(|p| start.starts_with(p))
    {
        return true;
    }
    let mime = mime_of(fetch.content_type.as_deref());
    let html = matches!(mime.as_str(), "text/html" | "application/xhtml+xml");
    html && !body.lines().any(|l| heading(l.trim()).is_some())
}

fn mime_of(content_type: Option<&str>) -> String {
    content_type
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// Whether the site has adopted llms.txt given the previous report: a file now means yes; a
/// failure now keeps what the previous report said; a 4xx or soft 404 means no (the owner removed
/// it, or never had one). Without a previous report (a first crawl, a local run) a failure is not
/// adoption.
pub fn carry_adoption(prev: Option<&LlmsTxtReport>, now: &LlmsTxtReport) -> bool {
    adoption(now.state, prev.is_some_and(|p| p.adopted))
}

fn adoption(state: LlmsState, prev_adopted: bool) -> bool {
    match state {
        LlmsState::Available => true,
        LlmsState::Unavailable => prev_adopted,
        LlmsState::Absent | LlmsState::SoftNotFound => false,
    }
}

/// The report for one fetch. `prev_adopted` is the previous report's `adopted` (false without
/// one); it only matters when the file is unavailable now.
pub fn assess(fetch: &LlmsTxtFetch, prev_adopted: bool) -> LlmsTxtReport {
    let state = state_of(fetch);
    let mut report = LlmsTxtReport {
        state,
        status: fetch.status,
        error: fetch.error.clone(),
        adopted: adoption(state, prev_adopted),
        title: None,
        optional_section: false,
        checks: Vec::new(),
        link_summary: LinkSummary::default(),
    };
    if state != LlmsState::Available {
        return report;
    }
    let doc = parse(&fetch.body);
    report.title = doc.title.clone();
    report.optional_section = doc.optional().is_some();
    report.link_summary = link_summary(fetch, listed(fetch, &doc));
    report.checks = checks(fetch, &doc, &report.link_summary);
    report
}

/// How many distinct pages the file links to, as the crawler counts them: each link resolved
/// against the file's final URL and normalized, so `/a`, `a` and `https://site/a` are one. Without
/// a final URL (older fetches) the links are counted as written.
fn listed(fetch: &LlmsTxtFetch, doc: &LlmsDoc) -> usize {
    let hrefs = links(doc);
    let Some(base) = fetch
        .final_url
        .as_deref()
        .and_then(|u| url::Url::parse(u).ok())
    else {
        return hrefs.len();
    };
    hrefs
        .iter()
        .filter_map(|h| normalize(&base, h))
        .map(|u| u.to_string())
        .collect::<HashSet<_>>()
        .len()
}

fn link_summary(fetch: &LlmsTxtFetch, listed: usize) -> LinkSummary {
    let failing: Vec<&str> = fetch
        .links
        .iter()
        .filter(|l| l.status.is_none_or(|s| s >= 400))
        .map(|l| l.url.as_str())
        .collect();
    let checked = fetch.links.len();
    LinkSummary {
        checked: clamp(checked),
        failing: clamp(failing.len()),
        not_checked: clamp(listed.saturating_sub(checked)),
        failing_urls: failing
            .iter()
            .take(MAX_FAILING_URLS)
            .map(|u| (*u).to_owned())
            .collect(),
    }
}

fn clamp(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

fn check(id: LlmsCheckId, ok: bool, detail: String) -> LlmsCheck {
    LlmsCheck { id, ok, detail }
}

fn checks(fetch: &LlmsTxtFetch, doc: &LlmsDoc, links: &LinkSummary) -> Vec<LlmsCheck> {
    use LlmsCheckId as Id;
    let mut out = Vec::new();

    out.push(match (&doc.title, doc.h1_first) {
        (Some(t), true) => check(Id::H1, true, format!("Starts with the H1 \"{t}\".")),
        (Some(t), false) => check(
            Id::H1,
            false,
            format!("The H1 \"{t}\" is not the first line; llms.txt starts with it."),
        ),
        (None, _) => check(
            Id::H1,
            false,
            "No H1: llms.txt starts with `# Site name`, its only required part.".to_owned(),
        ),
    });

    out.push(match &doc.summary {
        Some(_) => check(
            Id::Summary,
            true,
            "A `>` summary follows the H1.".to_owned(),
        ),
        None => check(
            Id::Summary,
            false,
            "No `>` blockquote summary right after the H1.".to_owned(),
        ),
    });

    let lists = doc.sections.iter().filter(|s| !s.links.is_empty()).count();
    let bare: Vec<&str> = doc
        .sections
        .iter()
        .filter(|s| s.links.is_empty())
        .map(|s| s.title.as_str())
        .collect();
    out.push(if lists == 0 {
        let detail = if doc.sections.is_empty() {
            "No H2 sections: links go in lists of `- [name](url)` under `## ` headings.".to_owned()
        } else {
            "No H2 section lists links as `- [name](url)` items.".to_owned()
        };
        check(Id::Sections, false, detail)
    } else {
        let mut detail = format!(
            "{lists} H2 section{} {} links as `- [name](url)` items",
            if lists == 1 { "" } else { "s" },
            if lists == 1 { "lists" } else { "list" },
        );
        if !bare.is_empty() {
            detail.push_str(&format!("; without links: {}", bare.join(", ")));
        }
        detail.push('.');
        check(Id::Sections, true, detail)
    });

    let n = doc.urls.len();
    out.push(if n == 0 {
        check(
            Id::Links,
            false,
            "No Markdown links: write them as `[name](url)`.".to_owned(),
        )
    } else {
        check(
            Id::Links,
            true,
            format!("{n} Markdown link{}.", if n == 1 { "" } else { "s" }),
        )
    });

    out.push(if doc.chars < MIN_CHARS {
        check(
            Id::Length,
            false,
            format!(
                "Only {} characters: suspiciously short (under {MIN_CHARS}).",
                doc.chars
            ),
        )
    } else {
        check(Id::Length, true, format!("{} characters.", doc.chars))
    });

    let kib = fetch.bytes.div_ceil(1024);
    out.push(if fetch.truncated {
        check(
            Id::Size,
            false,
            format!(
                "{kib} KiB: only the first {} KiB were read. Keep it small enough to fit in context.",
                MAX_BYTES / 1024
            ),
        )
    } else if fetch.bytes > NOTICE_BYTES {
        check(
            Id::Size,
            false,
            format!(
                "{kib} KiB: over {} KiB, too large to fit in many context windows.",
                NOTICE_BYTES / 1024
            ),
        )
    } else {
        check(Id::Size, true, format!("{kib} KiB."))
    });

    let mime = mime_of(fetch.content_type.as_deref());
    out.push(match mime.as_str() {
        "text/plain" | "text/markdown" | "text/x-markdown" => {
            check(Id::Mime, true, format!("Served as {mime}."))
        }
        "" => check(
            Id::Mime,
            false,
            "No Content-Type: serve it as text/plain or text/markdown.".to_owned(),
        ),
        _ => check(
            Id::Mime,
            false,
            format!("Served as {mime}: serve it as text/plain or text/markdown."),
        ),
    });

    out.push(if links.checked == 0 {
        check(Id::LinksResolve, true, "No links were checked.".to_owned())
    } else if links.failing == 0 {
        check(
            Id::LinksResolve,
            true,
            format!("All {} checked links answer.", links.checked),
        )
    } else {
        check(
            Id::LinksResolve,
            false,
            format!("{} of {} checked links fail.", links.failing, links.checked),
        )
    });
    out
}
