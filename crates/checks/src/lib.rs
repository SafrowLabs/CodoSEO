//! The CodoSEO check registry and the checks themselves.
//!
//! [`CHECKS`] describes all 44 checks (severity, category, scope, title). Per-page checks
//! live in [`page`] and are run with [`page_issues`]. [`run_checks`] runs everything over a
//! finished crawl and returns the report.

mod inlinks;
pub mod page;
mod registry;
mod score;
mod site;
mod summary;

use codoseo_core::check::CheckId;
use codoseo_core::output::CrawlOutput;
use codoseo_core::page::PageRecord;
use codoseo_core::report::CrawlReport;

pub use inlinks::MAX_INLINK_SAMPLES;
pub use page::page_issues;
pub use registry::{CHECKS, Category, CheckDef, Scope, def};
pub use score::health_score;

/// Runs the page checks only, replacing the page's issue bits. For `codoseo check` and
/// callers that have one page and no crawl.
pub fn check_page(page: &mut PageRecord) {
    page.issues = page_issues(page);
}

/// Runs every check over a finished crawl. Sets `inlinks` and the issue bits on every page
/// (replacing what was there, so running it again gives the same result) and returns the
/// report.
pub fn run_checks(out: &mut CrawlOutput) -> CrawlReport {
    inlinks::set_inlinks(out);
    for page in &mut out.pages {
        check_page(page);
    }
    let site_bits = site::site_issues(out);
    for (page, bits) in out.pages.iter_mut().zip(site_bits) {
        page.issues.0 |= bits.0;
    }
    let site_wide = site::site_wide_issues(out);

    let mut affected = [0u32; 64];
    for page in &out.pages {
        for bit in page.issues.iter() {
            affected[usize::from(bit)] += 1;
        }
    }
    let counts: Vec<(CheckId, u32)> = CheckId::ALL
        .into_iter()
        .filter_map(|id| {
            let n = match def(id).scope {
                Scope::SiteWide => u32::from(site_wide.contains(&id)),
                _ => affected[usize::from(id.bit())],
            };
            (n > 0).then_some((id, n))
        })
        .collect();

    CrawlReport {
        health_score: health_score(&counts, out.pages.len() as u32),
        checks_passed: (CHECKS.len() - counts.len()) as u16,
        checks_total: CHECKS.len() as u16,
        inlink_samples: inlinks::sample_inlinks(out),
        summary: summary::summarise(out),
        counts,
    }
}

/// A 4xx or 5xx answer, or a fetch that failed with an error (a robots.txt block is not one).
fn is_broken(page: &PageRecord) -> bool {
    (page.status == 0 && page.error.is_some()) || (400..600).contains(&page.status)
}

fn is_redirect(page: &PageRecord) -> bool {
    (300..400).contains(&page.status)
}

/// A page whose inlinks are always worth reporting: it redirects, errors or never answered.
fn is_problem_target(page: &PageRecord) -> bool {
    is_broken(page) || is_redirect(page)
}
