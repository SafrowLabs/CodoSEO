mod common;

use codoseo_checks::{health_score, run_checks};
use codoseo_core::check::CheckId;
use common::{chain_edges, clean_site, out};
use proptest::prelude::*;

#[test]
fn clean_site_scores_100() {
    let mut o = out(clean_site(), &chain_edges());
    let r = run_checks(&mut o);
    assert_eq!(r.health_score, 100);
    assert_eq!(r.checks_passed, 44);
    assert!(r.counts.is_empty());
}

#[test]
fn one_5xx_in_1000_pages_costs_about_a_quarter_of_15() {
    assert_eq!(health_score(&[(CheckId::Http5xx, 1)], 1000), 96);
}

#[test]
fn every_page_failing_every_check_is_zero() {
    let all: Vec<_> = CheckId::ALL.iter().map(|c| (*c, 10)).collect();
    assert_eq!(health_score(&all, 10), 0);
}

#[test]
fn no_failures_scores_100() {
    assert_eq!(health_score(&[], 0), 100);
}

#[test]
fn site_wide_checks_always_use_a_full_share() {
    // Notice cap 1: 1 x (0.25 + 0.75) = 1 point, however many pages there are
    assert_eq!(health_score(&[(CheckId::SitemapMissing, 1)], 1000), 99);
    // Critical cap 15, share 1
    assert_eq!(health_score(&[(CheckId::RobotsBlocksSite, 1)], 1000), 85);
}

#[test]
fn share_is_capped_at_one() {
    assert_eq!(
        health_score(&[(CheckId::Http5xx, 50)], 10),
        health_score(&[(CheckId::Http5xx, 10)], 10)
    );
}

proptest! {
    #[test]
    fn score_stays_in_range(
        counts in proptest::collection::vec((0u8..44, 0u32..100_000), 0..44),
        pages in 0u32..100_000,
    ) {
        let c: Vec<_> = counts
            .into_iter()
            .map(|(b, n)| (CheckId::from_bit(b).unwrap(), n))
            .collect();
        let s = health_score(&c, pages);
        prop_assert!(s <= 100);
    }
}
