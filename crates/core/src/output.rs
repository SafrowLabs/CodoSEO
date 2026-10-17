//! What a crawl produces: pages, the link graph and why it stopped.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::check::{CheckId, IssueBits};
use crate::crawl::{RobotsFile, SitemapSummary};
use crate::page::{FetchFailure, Indexability, PageRecord};
use crate::sitemap::SitemapFile;

/// One internal link between two crawled pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    /// Index into `CrawlOutput::pages`.
    pub from: u32,
    /// Index into `CrawlOutput::pages`.
    pub to: u32,
    /// Index into `LinkGraph::anchors`.
    pub anchor: u32,
    pub nofollow: bool,
}

/// Links between crawled pages, with anchor text interned so repeated navigation
/// links don't repeat their text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkGraph {
    pub edges: Vec<Edge>,
    pub anchors: Vec<String>,
}

impl LinkGraph {
    pub fn anchor(&self, e: &Edge) -> &str {
        &self.anchors[e.anchor as usize]
    }
}

/// Why a crawl ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "reason")]
pub enum StopReason {
    Completed,
    PageLimit,
    TimeLimit,
    Unreachable(String),
    Blocked(String),
    RobotsBlocked,
}

impl StopReason {
    /// The whole site was crawled.
    pub fn is_complete(&self) -> bool {
        matches!(self, StopReason::Completed)
    }

    /// The crawl got going and produced pages, even if it stopped early.
    pub fn crawl_ran(&self) -> bool {
        matches!(
            self,
            StopReason::Completed | StopReason::PageLimit | StopReason::TimeLimit
        )
    }
}

/// Start of a crawl's `failure_reason` when the site never answered (`StopReason::Unreachable`).
pub const UNREACHABLE_REASON_PREFIX: &str = "site unreachable";
/// Start of a crawl's `failure_reason` when the site refused our crawler (`StopReason::Blocked`).
pub const BLOCKED_REASON_PREFIX: &str = "site blocked our crawler";

/// What the crawler's stop reason says when robots.txt itself failed (5xx or 429), followed by
/// the status: `robots.txt returned HTTP 503`.
pub const ROBOTS_FAILED_REASON: &str = "robots.txt returned HTTP";

/// True when a crawl's `failure_reason` says the site was at fault because its robots.txt
/// answered 5xx or 429: the AI access incident speaks for that failure.
pub fn robots_failure(reason: &str) -> bool {
    SiteFault::from_failure_reason(reason).is_some() && reason.contains(ROBOTS_FAILED_REASON)
}

/// Whose fault a failed crawl was, read back from its stored `failure_reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteFault {
    /// The site didn't answer.
    Unreachable,
    /// The site answered with errors or a challenge page.
    Blocked,
}

impl SiteFault {
    /// `Some` only for the reasons the crawler's stop messages produce; internal failures
    /// (database errors, memory budget, ...) are ours, not the site's, and give `None`.
    pub fn from_failure_reason(reason: &str) -> Option<SiteFault> {
        if reason.starts_with(UNREACHABLE_REASON_PREFIX) {
            Some(SiteFault::Unreachable)
        } else if reason.starts_with(BLOCKED_REASON_PREFIX) {
            Some(SiteFault::Blocked)
        } else {
            None
        }
    }
}

/// A snapshot of a running crawl, for progress display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub pages_done: u32,
    pub queued: u32,
    pub failures: u32,
    pub depth: u16,
    pub elapsed_ms: u64,
}

/// How many of a running crawl's newest pages [`LiveProgress`] keeps.
pub const RECENT_PAGES: usize = 20;

/// A running crawl as the worker stores it between heartbeats: its [`Progress`], the error
/// count and its newest pages, for the live crawl views. Flattened, so a reader that only wants
/// the plain `Progress` reads the same JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveProgress {
    #[serde(flatten)]
    pub progress: Progress,
    /// Pages that answered 4xx or 5xx, or gave no response at all. Pages robots.txt keeps the
    /// crawler out of are not errors.
    #[serde(default)]
    pub errors: u32,
    /// The newest pages, newest first, at most [`RECENT_PAGES`].
    #[serde(default)]
    pub recent: Vec<RecentPage>,
    /// The stage the crawl is in. Rows written before stages existed read as `Pages`.
    #[serde(default)]
    pub phase: LivePhase,
    /// What the site files read before the pages gave, as far as the crawl has got.
    #[serde(default)]
    pub files: LiveFiles,
    /// The SEO score of the pages checked so far, from the page checks alone: the checks that
    /// need the whole crawl only count once it is over. `None` before the first page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seo_estimate: Option<u8>,
    /// Running totals over every page checked so far.
    #[serde(default)]
    pub tally: LiveTally,
}

/// Running totals over every checked page of a running crawl, for the live views' cards,
/// filters and issue list, so they need not read every page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveTally {
    /// Each page check failing somewhere, with the pages it fails on, in check order.
    #[serde(default)]
    pub checks: Vec<(CheckId, u32)>,
    /// Pages failing at least one page check.
    #[serde(default)]
    pub with_issues: u32,
    /// Pages at least one answer engine can't use.
    #[serde(default)]
    pub ai_blocked: u32,
    /// Pages the answer engines were judged on (robots.txt gave verdicts).
    #[serde(default)]
    pub ai_judged: u32,
    /// Response times of the pages that answered, summed, and how many answered.
    #[serde(default)]
    pub response_ms_total: u64,
    #[serde(default)]
    pub answered: u32,
}

impl LiveProgress {
    /// The progress with the error count and newest pages of `pages`, the crawl's records so
    /// far in the order they were recorded.
    pub fn new(progress: Progress, pages: &[PageRecord]) -> LiveProgress {
        let errors = pages.iter().filter(|p| is_error(p)).count();
        let recent = pages
            .iter()
            .enumerate()
            .rev()
            .take(RECENT_PAGES)
            .map(|(i, p)| RecentPage {
                n: u32::try_from(i + 1).unwrap_or(u32::MAX),
                url: p.url.to_string(),
                status: p.status,
                error: p.error,
                robots: p.indexability == Indexability::BlockedByRobots,
            })
            .collect();
        LiveProgress {
            progress,
            errors: u32::try_from(errors).unwrap_or(u32::MAX),
            recent,
            phase: LivePhase::Pages,
            files: LiveFiles::default(),
            seo_estimate: None,
            tally: LiveTally::default(),
        }
    }
}

/// The stage a running crawl is in, in the order a crawl goes through them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LivePhase {
    /// Fetching robots.txt.
    Robots,
    /// Reading the sitemaps.
    Sitemap,
    /// Fetching `/llms.txt` and the other site files AI products read.
    LlmsTxt,
    /// Fetching and checking pages.
    #[default]
    Pages,
    /// Fetching key pages as the AI bots do, to see whether the site treats them like a
    /// visitor. Only for monitored crawls with the probes on.
    Bots,
    /// The pages are in: the checks that need the whole crawl, the AI access report and the
    /// final scores.
    Scoring,
}

/// What the site files read before the pages gave. `None` until the crawl has read the file,
/// and for good when it never does (a crawl that stopped first, or a quick audit that skips
/// `llms.txt`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveFiles {
    /// robots.txt's status; 0 when it gave no response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub robots: Option<u16>,
    /// URLs the sitemaps list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sitemap_urls: Option<u32>,
    /// `/llms.txt`'s status; 0 when it gave no response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llms_txt: Option<u16>,
}

/// One checked page of a running crawl, as the live views list it. The worker keeps one per
/// recorded page, so the views can show and filter every page, not only the newest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveRow {
    /// Its place in the crawl: 1 for the first page recorded.
    pub n: u32,
    pub url: String,
    /// 0 when there was no response, or robots.txt kept the crawler out.
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<FetchFailure>,
    #[serde(default)]
    pub response_ms: u32,
    pub indexability: Indexability,
    /// What the page checks found on the page alone; the checks that need the whole crawl are
    /// left to the report.
    #[serde(default)]
    pub issues: IssueBits,
    /// The AI products that can't use the page, as bits over the answer engines in their
    /// registry order: robots.txt blocks their crawler, or the page's own controls keep it out.
    #[serde(default)]
    pub ai_blocked: u16,
    /// How many answer engines were judged; 0 when robots.txt gave no verdicts.
    #[serde(default)]
    pub ai_of: u8,
    /// The page fetched as each probed AI bot, once that is done; empty for the pages that are
    /// not probed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probes: Vec<LiveProbe>,
}

/// A page fetched as one AI bot, compared with our own fetch of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveProbe {
    /// The bot's robots.txt token.
    pub token: String,
    /// `same` when the site treated the bot like us; the finding's cause (`blocked_403`,
    /// `challenge_cloudflare`, ...) when it treated it worse; `skipped_robots` when robots.txt
    /// keeps the bot out, so it was not requested; `inconclusive` otherwise.
    pub outcome: String,
    /// The status the bot got; `None` when it was not requested or got no response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

/// A page counted by [`LiveProgress::errors`].
fn is_error(p: &PageRecord) -> bool {
    p.status >= 400 || (p.status == 0 && p.indexability != Indexability::BlockedByRobots)
}

/// One of a running crawl's newest pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentPage {
    /// Its place in the crawl: 1 for the first page recorded.
    pub n: u32,
    pub url: String,
    /// 0 when there was no response, or robots.txt kept the crawler out.
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<FetchFailure>,
    /// robots.txt keeps the crawler out of it, so it was never fetched.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub robots: bool,
}

/// A small file fetched from a well-known path, kept as served.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WellKnownFile {
    pub status: u16,
    /// Empty unless the status was 2xx.
    pub body: String,
}

/// `/llms.txt` as served, with the status of the links it lists.
///
/// A request that got no response (timeout, connection failure, broken redirect) is kept too, as
/// `status` 0 with `error` set, so a site whose file stopped answering is told apart from one where
/// the file was never fetched (`SiteSignals::llms_txt` is `None`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmsTxtFetch {
    /// The final status after redirects; 0 when no response arrived (see `error`).
    pub status: u16,
    /// Why no response arrived; `None` whenever there is a status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    /// Size of the whole response, which can exceed what `body` keeps.
    #[serde(default)]
    pub bytes: u64,
    /// `body` is cut short of the response.
    #[serde(default)]
    pub truncated: bool,
    /// Empty unless the status was 2xx.
    #[serde(default)]
    pub body: String,
    /// The URL the file was served from after redirects; relative links resolve against it.
    /// `None` without a response (and in older reports).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    /// Checked links from the file, in file order.
    #[serde(default)]
    pub links: Vec<LinkStatus>,
    /// Links on other origins that were not checked.
    #[serde(default)]
    pub external_unchecked: u16,
}

/// The outcome for one link listed in `llms.txt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkStatus {
    pub url: String,
    /// `None` when the request failed before a response.
    #[serde(default)]
    pub status: Option<u16>,
    /// The status comes from the crawl, not a separate request.
    #[serde(default)]
    pub from_crawl: bool,
}

/// Site-wide signals the crawl read outside the page list: what the site declares
/// about AI use in response headers and well-known files.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteSignals {
    /// `content-signal`, `content-usage`, `tdm-reservation` and `tdm-policy` headers of
    /// the settled start page: lower-cased names, values capped at 1 KiB.
    #[serde(default)]
    pub home_headers: Vec<(String, String)>,
    /// `/.well-known/tdmrep.json`; `None` when it was not fetched or the fetch failed.
    #[serde(default)]
    pub tdmrep: Option<WellKnownFile>,
    /// `/llms.txt`; `None` when it was not fetched (signals off, robots.txt disallows it, the crawl
    /// stopped early or ran out of time). A fetch that got no response is `Some`, with status 0.
    #[serde(default)]
    pub llms_txt: Option<LlmsTxtFetch>,
    /// Why `/llms.txt` was not fetched, when the crawl meant to and didn't; `None` when it was
    /// fetched, site signals were off, the crawl stopped before it (and in older crawls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llms_txt_skipped: Option<LlmsSkip>,
    /// The home page asked for with `Accept: text/markdown`; `None` when it was not asked (signals
    /// off, robots.txt disallows the home page, the crawl stopped early, and in older crawls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<MarkdownProbe>,
    /// The site's icon, for showing the site in the app. Not part of any report: it is stored
    /// on its own and never serialized with the signals.
    #[serde(skip)]
    pub icon: Option<SiteIcon>,
}

/// A site's icon as fetched: a small raster image whose type was read from its own bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteIcon {
    /// `image/png`, `image/x-icon`, `image/gif`, `image/jpeg` or `image/webp`.
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

/// One answer of a Markdown request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkdownFetch {
    /// The URL asked for.
    pub url: String,
    /// The final status after redirects; 0 when no response arrived (see `error`).
    pub status: u16,
    /// Why no response arrived; `None` whenever there is a status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Bytes of body read that are not whitespace (up to the cap): 0 is an empty body.
    #[serde(default)]
    pub body_bytes: u64,
}

/// Where a page advertised its Markdown version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlternateSource {
    /// `<link rel="alternate" type="text/markdown" href="…">` in the HTML.
    LinkTag,
    /// A `Link: <…>; rel="alternate"; type="text/markdown"` response header.
    LinkHeader,
}

/// The home page asked for as Markdown: the content-negotiated answer and, when that was not
/// Markdown, the Markdown alternate the page advertised and what it answered.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkdownProbe {
    /// The home page with `Accept: text/markdown, text/html;q=0.9`.
    pub negotiated: MarkdownFetch,
    /// The advertised alternate's URL and where it was advertised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternate: Option<(String, AlternateSource)>,
    /// What the alternate answered; `None` when it was not requested (none advertised, the
    /// negotiated answer was Markdown already, it is on another origin or robots.txt disallows it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternate_fetch: Option<MarkdownFetch>,
}

/// Why a crawl did not fetch `/llms.txt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmsSkip {
    /// robots.txt disallows CodoSEObot on `/llms.txt`, or on where it redirects.
    RobotsDisallowed,
    /// The address (or a redirect's) is one CodoSEO may not request.
    Refused,
    /// The crawl's time ran out first.
    Deadline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrawlOutput {
    /// The settled start address: the origin pages are internal to.
    pub origin: Url,
    pub pages: Vec<PageRecord>,
    pub links: LinkGraph,
    pub robots: Option<RobotsFile>,
    pub sitemap: SitemapSummary,
    /// Every sitemap file discovery tried, in fetch order (see [`crate::sitemap`]).
    #[serde(default)]
    pub sitemap_files: Vec<SitemapFile>,
    pub stop: StopReason,
    pub duration_ms: u64,
    #[serde(default)]
    pub signals: SiteSignals,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress() -> Progress {
        Progress {
            pages_done: 3,
            queued: 9,
            failures: 1,
            depth: 2,
            elapsed_ms: 4200,
        }
    }

    #[test]
    fn live_progress_is_read_as_plain_progress_and_back() {
        let live = LiveProgress {
            progress: progress(),
            errors: 1,
            recent: vec![RecentPage {
                n: 3,
                url: "https://e.com/gone".to_owned(),
                status: 404,
                error: None,
                robots: false,
            }],
            phase: LivePhase::LlmsTxt,
            files: LiveFiles {
                robots: Some(200),
                sitemap_urls: Some(452),
                llms_txt: None,
            },
            seo_estimate: Some(84),
            tally: LiveTally::default(),
        };
        let json = serde_json::to_value(&live).unwrap();
        assert_eq!(json["phase"], "llms_txt");
        assert_eq!(json["files"]["sitemap_urls"], 452);
        assert!(json["files"].get("llms_txt").is_none());
        assert_eq!(json["pages_done"], 3);
        assert_eq!(json["recent"][0]["status"], 404);
        assert!(json["recent"][0].get("robots").is_none());
        let plain: Progress = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(plain, progress());
        assert_eq!(serde_json::from_value::<LiveProgress>(json).unwrap(), live);

        // A heartbeat written before the newest pages were kept reads with none.
        let old = serde_json::to_value(progress()).unwrap();
        let read: LiveProgress = serde_json::from_value(old).unwrap();
        assert_eq!((read.errors, read.recent.len()), (0, 0));
        // One written before stages were kept reads as listing pages, with no files known.
        assert_eq!(read.phase, LivePhase::Pages);
        assert_eq!(read.files, LiveFiles::default());
        assert_eq!(read.seo_estimate, None);
    }
}
