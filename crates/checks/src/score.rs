//! The 0-100 health score, and what fixing one check would give back.

use codoseo_core::check::{CheckId, Severity};

use crate::registry::{Scope, def};

/// What a failing check costs at most, by severity.
fn cap(severity: Severity) -> f64 {
    match severity {
        Severity::Critical => 15.0,
        Severity::Warning => 5.0,
        Severity::Notice => 1.0,
    }
}

/// What one failing check costs: `cap x (0.25 + 0.75 x share)`.
fn cost(id: CheckId, count: u32, pages: u32) -> f64 {
    let d = def(id);
    let share = match d.scope {
        Scope::SiteWide => 1.0,
        _ => (f64::from(count) / f64::from(pages.max(1))).min(1.0),
    };
    cap(d.severity) * (0.25 + 0.75 * share)
}

/// The score before rounding: 100 minus every cost, never below 0.
fn raw_score(counts: &[(CheckId, u32)], pages: u32) -> f64 {
    let penalty: f64 = counts.iter().map(|&(id, n)| cost(id, n, pages)).sum();
    (100.0 - penalty).max(0.0)
}

/// 100 minus the cost of every failing check, never below 0. A check costs
/// `cap x (0.25 + 0.75 x share)`: even one failure costs a quarter of the cap, and a check
/// failing on every page costs all of it. `share` is affected pages over `pages`, and 1 for
/// site-wide checks. `counts` holds each failing check with its affected pages.
pub fn health_score(counts: &[(CheckId, u32)], pages: u32) -> u8 {
    raw_score(counts, pages).round() as u8
}

/// For each failing check in `counts`, in the same order, the score points fixing it alone
/// would give back: the unrounded score without it minus the unrounded score with everything.
/// Usually the check's own cost; less when the score sits at the floor of 0, since a fix only
/// counts once the other costs leave room above it.
pub fn points_regained(counts: &[(CheckId, u32)], pages: u32) -> Vec<(CheckId, f64)> {
    let now = raw_score(counts, pages);
    counts
        .iter()
        .enumerate()
        .map(|(i, &(id, _))| {
            let rest: Vec<(CheckId, u32)> = counts
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, &c)| c)
                .collect();
            (id, raw_score(&rest, pages) - now)
        })
        .collect()
}
