//! The result of running the checks over a crawl.

use serde::{Deserialize, Deserializer, Serialize};

use crate::check::CheckId;

/// An internal link to a page that has a problem, kept as a sample for the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlinkSample {
    /// Index of the page being linked to.
    pub target: u32,
    /// Index of the page holding the link.
    pub source: u32,
    pub anchor: String,
    pub nofollow: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusCounts {
    pub ok: u32,
    pub redirect: u32,
    pub client_error: u32,
    pub server_error: u32,
    /// No response: timeouts, connection errors, redirect loops.
    pub failed: u32,
    pub blocked: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrawlSummary {
    pub pages: u32,
    pub indexable: u32,
    pub status: StatusCounts,
    /// Pages per click depth; index = clicks, the last bucket is 10 and deeper.
    pub depth: Vec<u32>,
    /// Pages with no depth (found only in sitemaps).
    pub no_depth: u32,
    pub avg_response_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrawlReport {
    pub health_score: u8,
    pub checks_passed: u16,
    pub checks_total: u16,
    /// Failing checks only, in `CheckId` order, with the number of affected pages
    /// (1 for site-wide checks). When read back, entries for checks this version doesn't
    /// know (added by a later one) are skipped, so newer audits still load.
    #[serde(deserialize_with = "known_counts")]
    pub counts: Vec<(CheckId, u32)>,
    pub inlink_samples: Vec<InlinkSample>,
    pub summary: CrawlSummary,
}

/// Reads `counts`, dropping entries whose check slug is unknown.
fn known_counts<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<(CheckId, u32)>, D::Error> {
    let raw = Vec::<(String, u32)>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|(slug, n)| Some((CheckId::from_slug(&slug)?, n)))
        .collect())
}
