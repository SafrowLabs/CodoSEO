//! What is kept of a crawl for later comparison.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::crawl::{RobotsFile, SitemapSummary};
use crate::output::{CrawlOutput, StopReason};
use crate::page::PageRecord;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub origin: Url,
    pub stop: StopReason,
    pub pages: Vec<PageRecord>,
    pub robots: Option<RobotsFile>,
    pub sitemap: SitemapSummary,
}

impl Snapshot {
    /// Everything the diff needs; the link graph is not kept.
    pub fn from_output(out: &CrawlOutput) -> Snapshot {
        Snapshot {
            origin: out.origin.clone(),
            stop: out.stop.clone(),
            pages: out.pages.clone(),
            robots: out.robots.clone(),
            sitemap: out.sitemap.clone(),
        }
    }

    /// Like [`Snapshot::from_output`], but moves the pages out instead of cloning them.
    pub fn from_output_owned(out: CrawlOutput) -> Snapshot {
        Snapshot {
            origin: out.origin,
            stop: out.stop,
            pages: out.pages,
            robots: out.robots,
            sitemap: out.sitemap,
        }
    }
}
