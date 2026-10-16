//! Thresholds and exceptions: how a policy changes what the checks flag and what the health score
//! counts.
//!
//! - The thresholds replace the numbers of the length, thin, slow and redirect checks
//!   ([`run_checks_with`] on a new crawl, [`recheck`] on a stored one).
//! - An exception keeps a check's issue on the pages it matches (the explorer marks them
//!   "Excepted") but leaves them out of that check's count in the score.
//! - A page the index rules keep out of search that does carry `noindex` (the rule is met) leaves
//!   the score entirely, from the page count too.
//!
//! [`scored_counts`] is that score's input; [`preview`] compares two policies on a stored crawl,
//! as the policy screen's "When you save" box and the `preview_policy` tool say it.

use std::collections::{HashMap, HashSet};

use codoseo_checks::{Scope, def, health_score, threshold_issues, threshold_mask};
use codoseo_core::check::{CheckId, IssueBits};
use codoseo_core::output::CrawlOutput;
use codoseo_core::page::{Indexability, PageRecord};
use codoseo_core::report::{CrawlReport, ExcludedCounts};
use serde::Serialize;
use url::Url;

use crate::evaluate::PolicyInput;
use crate::seo::{declared_checks, index_rows, path_and_query};
use crate::{Exception, IndexStance, SeoRules, SitePolicy, apply_to_crawl, policy_bits};

/// Runs every check over a finished crawl under `policy`: its thresholds, then its SEO rules
/// (the policy checks) and its exceptions. The default policy gives exactly the report
/// [`codoseo_checks::run_checks`] gives.
pub fn run_checks_with(
    policy: &SitePolicy,
    out: &mut CrawlOutput,
    starred: &HashSet<u64>,
    starred_urls: &[Url],
) -> CrawlReport {
    let mut report = codoseo_checks::run_checks_with(out, &policy.thresholds());
    apply_to_crawl(policy, out, &mut report, starred, starred_urls);
    report
}

/// The exception of `seo` that covers check `id` on `url`, the first listed.
pub fn exception_for<'a>(seo: &'a SeoRules, id: CheckId, url: &Url) -> Option<&'a Exception> {
    if seo.exceptions.is_empty() {
        return None;
    }
    let pq = path_and_query(url);
    seo.exceptions
        .iter()
        .find(|e| e.check == id && e.scope.matches(&pq))
}

/// The stance the index rules give a page in row `row` (from [`index_rows`]).
fn stance_of(seo: &SeoRules, row: Option<usize>) -> IndexStance {
    row.map_or(seo.index_default, |i| seo.index[i].stance)
}

/// Whether `page` is one the policy keeps out of search and it is kept out (it carries
/// `noindex`): it leaves the health score.
pub fn keep_out_met(seo: &SeoRules, page: &PageRecord) -> bool {
    page.indexability == Indexability::Noindex
        && stance_of(seo, index_rows(seo, std::slice::from_ref(page))[0]) == IndexStance::KeepOut
}

fn keeps_out(seo: &SeoRules) -> bool {
    seo.index_default == IndexStance::KeepOut
        || seo.index.iter().any(|r| r.stance == IndexStance::KeepOut)
}

/// What the policy leaves out of the score of `pages` (their issue bits as they are).
pub fn excluded(seo: &SeoRules, pages: &[PageRecord]) -> ExcludedCounts {
    if seo.exceptions.is_empty() && !keeps_out(seo) {
        return ExcludedCounts::default();
    }
    let rows = if keeps_out(seo) {
        index_rows(seo, pages)
    } else {
        vec![None; pages.len()]
    };
    let mut out = [0u32; 64];
    let mut whole = 0;
    for (page, row) in pages.iter().zip(rows) {
        if page.issues.0 == 0 && page.indexability != Indexability::Noindex {
            continue;
        }
        if page.indexability == Indexability::Noindex && stance_of(seo, row) == IndexStance::KeepOut
        {
            whole += 1;
            for bit in page.issues.iter() {
                out[usize::from(bit)] += 1;
            }
            continue;
        }
        for bit in page.issues.iter() {
            if let Some(id) = CheckId::from_bit(bit)
                && exception_for(seo, id, &page.url).is_some()
            {
                out[usize::from(bit)] += 1;
            }
        }
    }
    ExcludedCounts {
        pages: whole,
        counts: CheckId::ALL
            .into_iter()
            .filter(|id| def(*id).scope != Scope::SiteWide)
            .filter_map(|id| {
                let n = out[usize::from(id.bit())];
                (n > 0).then_some((id, n))
            })
            .collect(),
    }
}

/// The health score's input under `policy`: the failing checks' `counts` less what the policy
/// leaves out, the pages the score is drawn over, and what was left out.
pub fn scored_counts(
    policy: &SitePolicy,
    pages: &[PageRecord],
    counts: &[(CheckId, u32)],
) -> (Vec<(CheckId, u32)>, u32, ExcludedCounts) {
    let excluded = excluded(&policy.seo, pages);
    let scored = counts
        .iter()
        .filter_map(|&(id, n)| {
            let left = n.saturating_sub(excluded.of(id));
            (left > 0).then_some((id, left))
        })
        .collect();
    let total = (pages.len() as u32).saturating_sub(excluded.pages);
    (scored, total, excluded)
}

/// Sets `report`'s excluded counts, health score and checks passed from its counts under
/// `policy`, over `pages` (whose bits the counts were drawn from).
pub(crate) fn score_report(policy: &SitePolicy, pages: &[PageRecord], report: &mut CrawlReport) {
    let (scored, total, excluded) = scored_counts(policy, pages, &report.counts);
    report.health_score = health_score(&scored, total);
    report.checks_passed = report.checks_total.saturating_sub(scored.len() as u16);
    report.excluded = excluded;
}

/// The checks a crawl counts under `policy`: the base checks its thresholds run and the policy
/// checks its SEO rules declare.
pub fn checks_total(policy: &SitePolicy) -> u16 {
    (policy.thresholds().base_checks() + declared_checks(&policy.seo).len()) as u16
}

fn policy_mask() -> u64 {
    CheckId::POLICY.iter().fold(0, |m, c| m | (1u64 << c.bit()))
}

/// The issue bits of a stored crawl's pages under `policy`: the threshold checks run again on the
/// stored fields, the policy checks set again by its SEO rules, and every other bit (the checks
/// that need the whole crawl among them) kept as stored.
pub fn recheck(policy: &SitePolicy, input: &PolicyInput) -> Vec<IssueBits> {
    let t = policy.thresholds();
    let keep = !(threshold_mask() | policy_mask());
    let declared = !declared_checks(&policy.seo).is_empty();
    let pbits = if declared {
        policy_bits(policy, input)
    } else {
        Vec::new()
    };
    input
        .pages
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut b = IssueBits((p.issues.0 & keep) | threshold_issues(p, &t).0);
            if let Some(extra) = pbits.get(i) {
                b.0 |= extra.0 & policy_mask();
            }
            b
        })
        .collect()
}

/// A stored crawl's report numbers under a policy, from its pages' bits ([`recheck`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rescored {
    /// Every failing check with its pages, in `CheckId` order.
    pub counts: Vec<(CheckId, u32)>,
    pub excluded: ExcludedCounts,
    pub health_score: u8,
    pub checks_passed: u16,
    pub checks_total: u16,
}

/// The report numbers of a stored crawl whose pages now have `bits`, under `policy`. The
/// threshold and policy checks are counted again from the bits; the other checks keep their
/// `stored` counts (the site-wide ones have no page bit to count).
pub fn rescore_stored(
    policy: &SitePolicy,
    pages: &[PageRecord],
    bits: &[IssueBits],
    stored: &[(CheckId, u32)],
) -> Rescored {
    let recount = threshold_mask() | policy_mask();
    let mut affected = [0u32; 64];
    for b in bits {
        for bit in b.iter() {
            affected[usize::from(bit)] += 1;
        }
    }
    let stored: HashMap<CheckId, u32> = stored.iter().copied().collect();
    let counts: Vec<(CheckId, u32)> = CheckId::ALL
        .into_iter()
        .filter_map(|id| {
            let n = if recount & (1u64 << id.bit()) != 0 {
                affected[usize::from(id.bit())]
            } else {
                stored.get(&id).copied().unwrap_or(0)
            };
            (n > 0).then_some((id, n))
        })
        .collect();
    let rebuilt: Vec<PageRecord>;
    let pages = if pages.iter().zip(bits).all(|(p, b)| p.issues == *b) {
        pages
    } else {
        rebuilt = pages
            .iter()
            .zip(bits)
            .map(|(p, b)| PageRecord {
                issues: *b,
                ..p.clone()
            })
            .collect();
        &rebuilt
    };
    let (scored, total, excluded) = scored_counts(policy, pages, &counts);
    let checks_total = checks_total(policy);
    Rescored {
        health_score: health_score(&scored, total),
        checks_passed: checks_total.saturating_sub(scored.len() as u16),
        checks_total,
        counts,
        excluded,
    }
}

/// What saving a policy would do to the latest crawl, before it is saved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SavePreview {
    /// Issues (a check on a page) counted now that the new policy no longer counts: no longer
    /// flagged by a threshold, excepted, or on a page kept out as asked.
    pub muted: u32,
    /// Issues the new policy counts that are not counted now, opened at once on the latest crawl.
    pub opened: u32,
    /// The latest crawl's health score, now and under the new policy.
    pub health: (u8, u8),
    /// Its GEO score, now and under the new policy; `None` without an AI access report.
    pub geo: Option<(u8, u8)>,
}

/// The issues counted in the score of a crawl whose pages have `bits`, under `seo`.
fn counted(seo: &SeoRules, pages: &[PageRecord], bits: &[IssueBits]) -> Vec<u64> {
    let rows = if keeps_out(seo) {
        index_rows(seo, pages)
    } else {
        vec![None; pages.len()]
    };
    pages
        .iter()
        .zip(bits)
        .zip(rows)
        .map(|((p, b), row)| {
            if p.indexability == Indexability::Noindex
                && stance_of(seo, row) == IndexStance::KeepOut
            {
                return 0;
            }
            let mut kept = b.0;
            for bit in b.iter() {
                if let Some(id) = CheckId::from_bit(bit)
                    && exception_for(seo, id, &p.url).is_some()
                {
                    kept &= !(1u64 << bit);
                }
            }
            kept
        })
        .collect()
}

/// What saving `new` in place of `old` would change on the stored crawl `input` (its pages with
/// their stored bits), whose report counted `stored` failing checks: the issues muted and opened
/// and the health and GEO scores before and after. Pure: the save runs the same code and writes
/// the result.
pub fn preview(
    old: &SitePolicy,
    new: &SitePolicy,
    input: &PolicyInput,
    stored: &[(CheckId, u32)],
) -> SavePreview {
    let before_bits = recheck(old, input);
    let after_bits = recheck(new, input);
    let before = rescore_stored(old, input.pages, &before_bits, stored);
    let after = rescore_stored(new, input.pages, &after_bits, stored);
    let a = counted(&old.seo, input.pages, &before_bits);
    let b = counted(&new.seo, input.pages, &after_bits);
    let (mut muted, mut opened) = (0, 0);
    for (x, y) in a.iter().zip(&b) {
        muted += (x & !y).count_ones();
        opened += (y & !x).count_ones();
    }
    let pages = input.pages.len() as u32;
    let geo = input.access.and_then(|report| {
        let g = |p: &SitePolicy, counts: &[(CheckId, u32)]| {
            codoseo_geo::score::assess(report, &p.ai.intent, counts, pages).map(|s| s.score)
        };
        Some((g(old, &before.counts)?, g(new, &after.counts)?))
    });
    SavePreview {
        muted,
        opened,
        health: (before.health_score, after.health_score),
        geo,
    }
}

/// For each threshold row of the policy screen: how many pages of the stored crawl `pages` the
/// check flags under `policy`'s thresholds and under the defaults.
pub fn threshold_flags(policy: &SitePolicy, pages: &[PageRecord]) -> Vec<(CheckId, u32, u32)> {
    let t = policy.thresholds();
    let d = codoseo_checks::Thresholds::DEFAULT;
    let mut now = [0u32; 64];
    let mut default = [0u32; 64];
    for p in pages {
        for bit in threshold_issues(p, &t).iter() {
            now[usize::from(bit)] += 1;
        }
        for bit in threshold_issues(p, &d).iter() {
            default[usize::from(bit)] += 1;
        }
    }
    codoseo_checks::THRESHOLD_CHECK_IDS
        .iter()
        .map(|id| {
            let b = usize::from(id.bit());
            (*id, now[b], default[b])
        })
        .collect()
}

/// For each exception of `seo`, in order: the pages of the crawl (with `bits`, from
/// [`recheck`]) it leaves out of its check's count.
pub fn exception_matches(seo: &SeoRules, pages: &[PageRecord], bits: &[IssueBits]) -> Vec<u32> {
    let paths: Vec<String> = if seo.exceptions.is_empty() {
        Vec::new()
    } else {
        pages.iter().map(|p| path_and_query(&p.url)).collect()
    };
    seo.exceptions
        .iter()
        .map(|e| {
            paths
                .iter()
                .zip(bits)
                .filter(|(pq, b)| b.has_check(e.check) && e.scope.matches(pq))
                .count() as u32
        })
        .collect()
}
