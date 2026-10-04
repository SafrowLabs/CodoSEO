//! The 0-100 health score.

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

/// 100 minus the cost of every failing check, never below 0. A check costs
/// `cap x (0.25 + 0.75 x share)`: even one failure costs a quarter of the cap, and a check
/// failing on every page costs all of it. `share` is affected pages over `pages`, and 1 for
/// site-wide checks. `counts` holds each failing check with its affected pages.
pub fn health_score(counts: &[(CheckId, u32)], pages: u32) -> u8 {
    let total = f64::from(pages.max(1));
    let penalty: f64 = counts
        .iter()
        .map(|&(id, count)| {
            let d = def(id);
            let share = match d.scope {
                Scope::SiteWide => 1.0,
                _ => (f64::from(count) / total).min(1.0),
            };
            cap(d.severity) * (0.25 + 0.75 * share)
        })
        .sum();
    (100.0 - penalty).max(0.0).round() as u8
}
