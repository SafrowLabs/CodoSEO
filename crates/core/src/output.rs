//! What a crawl produces: pages, the link graph and why it stopped.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::crawl::{RobotsFile, SitemapSummary};
use crate::page::PageRecord;

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

/// A small file fetched from a well-known path, kept as served.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WellKnownFile {
    pub status: u16,
    /// Empty unless the status was 2xx.
    pub body: String,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrawlOutput {
    /// The settled start address: the origin pages are internal to.
    pub origin: Url,
    pub pages: Vec<PageRecord>,
    pub links: LinkGraph,
    pub robots: Option<RobotsFile>,
    pub sitemap: SitemapSummary,
    pub stop: StopReason,
    pub duration_ms: u64,
    #[serde(default)]
    pub signals: SiteSignals,
}
