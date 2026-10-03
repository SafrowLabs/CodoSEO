//! Inlink counts and the inlink samples kept in the report.

use std::collections::HashSet;

use codoseo_core::output::CrawlOutput;
use codoseo_core::report::InlinkSample;

use crate::is_problem_target;

/// Inlinks kept per target page, besides every inlink to a problem page.
pub const MAX_INLINK_SAMPLES: usize = 20;

/// Sets `inlinks` on every page: the number of different pages linking to it. Several
/// links from one page count once, and a page linking to itself doesn't count.
pub(crate) fn set_inlinks(out: &mut CrawlOutput) {
    for page in &mut out.pages {
        page.inlinks = 0;
    }
    let mut seen: HashSet<u64> = HashSet::with_capacity(out.links.edges.len());
    for e in &out.links.edges {
        if e.from != e.to && seen.insert(u64::from(e.from) << 32 | u64::from(e.to)) {
            out.pages[e.to as usize].inlinks += 1;
        }
    }
}

/// The first [`MAX_INLINK_SAMPLES`] links to each page in edge order, plus every link to a
/// page that redirects, errors or could not be fetched.
pub(crate) fn sample_inlinks(out: &CrawlOutput) -> Vec<InlinkSample> {
    let mut kept = vec![0usize; out.pages.len()];
    let mut samples = Vec::new();
    for e in &out.links.edges {
        if e.from == e.to {
            continue;
        }
        let target = e.to as usize;
        if kept[target] < MAX_INLINK_SAMPLES || is_problem_target(&out.pages[target]) {
            kept[target] += 1;
            samples.push(InlinkSample {
                target: e.to,
                source: e.from,
                anchor: out.links.anchor(e).to_string(),
                nofollow: e.nofollow,
            });
        }
    }
    samples
}
