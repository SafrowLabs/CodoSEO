mod common;

use codoseo_checks::{health_score, points_regained, run_checks};
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

#[test]
fn a_fix_gives_back_what_the_check_costs() {
    // Critical on 1 of 1000 pages: 15 x (0.25 + 0.75 x 0.001); site-wide notice: 1.
    let counts = [(CheckId::Http5xx, 1), (CheckId::SitemapMissing, 1)];
    let gains = points_regained(&counts, 1000);
    assert_eq!(gains.len(), 2);
    assert_eq!(gains[0].0, CheckId::Http5xx);
    assert!((gains[0].1 - 3.76125).abs() < 1e-9, "{gains:?}");
    assert_eq!(gains[1].0, CheckId::SitemapMissing);
    assert!((gains[1].1 - 1.0).abs() < 1e-9, "{gains:?}");
    // More affected pages, more points back.
    let wide = points_regained(&[(CheckId::Http5xx, 500)], 1000);
    assert!(wide[0].1 > gains[0].1);
    assert!(points_regained(&[], 10).is_empty());
}

#[test]
fn at_the_floor_a_fix_only_counts_what_rises_above_zero() {
    // Every check on every page: far past 100 points of cost, so one fix alone gives nothing back.
    let all: Vec<_> = CheckId::ALL.iter().map(|c| (*c, 10)).collect();
    assert!(points_regained(&all, 10).iter().all(|&(_, g)| g == 0.0));
}

proptest! {
    #[test]
    fn gains_add_up_to_the_score_lost_when_above_the_floor(
        counts in proptest::collection::btree_map(0u8..44, 1u32..1_000, 0..6),
        pages in 1u32..1_000,
    ) {
        let c: Vec<_> = counts
            .into_iter()
            .map(|(b, n)| (CheckId::from_bit(b).unwrap(), n))
            .collect();
        let gains: f64 = points_regained(&c, pages).iter().map(|g| g.1).sum();
        prop_assert!(gains >= 0.0);
        // Six checks cost at most 90 points, so the score never hits the floor: the gains are
        // exactly the points lost.
        let lost = 100.0 - f64::from(health_score(&c, pages));
        prop_assert!((gains - lost).abs() <= 0.5 + 1e-9, "{gains} vs {lost}");
    }

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
