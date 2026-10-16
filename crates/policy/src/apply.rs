//! The policy pass over a finished crawl: after `run_checks`, the SEO rules' gaps become the
//! policy check bits on the pages, the report counts them like any other check, and the score
//! leaves out what the exceptions and the kept-out pages take out of it.

use std::collections::HashSet;

use codoseo_core::check::{CheckId, IssueBits};
use codoseo_core::output::CrawlOutput;
use codoseo_core::report::CrawlReport;
use url::Url;

use crate::SitePolicy;
use crate::evaluate::PolicyInput;
use crate::seo::{declared_checks, evaluate_seo};

/// The policy check bits for each page of `input`, in page order.
pub fn policy_bits(policy: &SitePolicy, input: &PolicyInput) -> Vec<IssueBits> {
    evaluate_seo(policy, input).bits
}

fn policy_mask() -> u64 {
    CheckId::POLICY.iter().fold(0, |m, c| m | (1u64 << c.bit()))
}

/// Runs the policy's SEO rules over a crawl `run_checks` has just checked (under the policy's
/// thresholds, [`crate::run_checks_with`]): replaces the policy check bits on every page, adds the
/// failing ones to `report.counts`, counts the declared rules' checks in `checks_total` and
/// rescores, leaving out of the score what the policy excludes ([`crate::scored_counts`]).
/// Running it again gives the same result, and the default policy (no SEO rules) changes
/// nothing.
pub fn apply_to_crawl(
    policy: &SitePolicy,
    out: &mut CrawlOutput,
    report: &mut CrawlReport,
    starred: &HashSet<u64>,
    starred_urls: &[Url],
) {
    let mask = policy_mask();
    // Nothing declared and nothing to clear: the report stays exactly as `run_checks` made it.
    if declared_checks(&policy.seo).is_empty()
        && policy.seo.exceptions.is_empty()
        && out.pages.iter().all(|p| p.issues.0 & mask == 0)
    {
        return;
    }
    let bits = {
        let input = PolicyInput {
            pages: &out.pages,
            origin: Some(&out.origin),
            starred: Some(starred),
            starred_urls,
            inlinks: &report.inlink_samples,
            inlinks_capped: false,
            access: None,
        };
        policy_bits(policy, &input)
    };
    let mut affected = [0u32; 64];
    for (page, b) in out.pages.iter_mut().zip(&bits) {
        page.issues.0 = (page.issues.0 & !mask) | (b.0 & mask);
        for bit in b.iter() {
            affected[usize::from(bit)] += 1;
        }
    }
    report.counts.retain(|(id, _)| !id.is_policy());
    for id in CheckId::POLICY {
        let n = affected[usize::from(id.bit())];
        if n > 0 {
            report.counts.push((id, n));
        }
    }
    report.counts.sort_by_key(|(id, _)| *id);
    report.checks_total = crate::score::checks_total(policy);
    crate::score::score_report(policy, &out.pages, report);
}
