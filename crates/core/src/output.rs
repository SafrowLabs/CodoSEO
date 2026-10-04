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

/// A snapshot of a running crawl, for progress display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub pages_done: u32,
    pub queued: u32,
    pub failures: u32,
    pub depth: u16,
    pub elapsed_ms: u64,
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
}
