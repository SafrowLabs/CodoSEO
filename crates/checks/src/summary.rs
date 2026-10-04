//! The crawl summary shown at the top of a report.

use codoseo_core::output::CrawlOutput;
use codoseo_core::page::{Indexability, PageRecord};
use codoseo_core::report::{CrawlSummary, StatusCounts};

/// Depth buckets: 0 to 9, then 10 and deeper.
const DEPTH_BUCKETS: usize = 11;

pub(crate) fn summarise(out: &CrawlOutput) -> CrawlSummary {
    let mut status = StatusCounts::default();
    let mut depth = [0u32; DEPTH_BUCKETS];
    let mut indexable = 0;
    let mut no_depth = 0;
    let mut responses = 0u64;
    let mut response_ms = 0u64;

    for p in &out.pages {
        if p.indexability == Indexability::Indexable {
            indexable += 1;
        }
        count_status(&mut status, p);
        match p.depth {
            Some(d) => depth[usize::from(d).min(DEPTH_BUCKETS - 1)] += 1,
            None => no_depth += 1,
        }
        if p.status != 0 {
            responses += 1;
            response_ms += u64::from(p.response_ms);
        }
    }

    let used = depth.iter().rposition(|&n| n > 0).map_or(0, |i| i + 1);
    CrawlSummary {
        pages: out.pages.len() as u32,
        indexable,
        status,
        depth: depth[..used].to_vec(),
        no_depth,
        avg_response_ms: response_ms.checked_div(responses).unwrap_or(0) as u32,
    }
}

fn count_status(counts: &mut StatusCounts, p: &PageRecord) {
    if p.indexability == Indexability::BlockedByRobots {
        counts.blocked += 1;
        return;
    }
    match p.status {
        200..=299 => counts.ok += 1,
        300..=399 => counts.redirect += 1,
        400..=499 => counts.client_error += 1,
        500..=599 => counts.server_error += 1,
        0 if p.error.is_some() => counts.failed += 1,
        _ => {}
    }
}
