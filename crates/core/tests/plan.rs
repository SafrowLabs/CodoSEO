use std::time::Duration;

use codoseo_core::plan::{ManualAllowance, Plan, PlanLimits, Schedule};

const HOUR: u64 = 3600;

#[test]
fn free_plan_matches_spec() {
    let l = PlanLimits::for_plan(Plan::Free);
    assert_eq!(l.max_sites, Some(1));
    assert_eq!(l.max_pages, Some(500));
    assert_eq!(l.max_duration, Some(Duration::from_secs(600)));
    assert_eq!(l.fastest_schedule, Some(Schedule::Weekly));
    assert_eq!(l.manual_crawls, ManualAllowance::PerWeek(1));
    assert_eq!(l.history_days, Some(30));
    assert_eq!(l.api_calls_per_day, Some(100));
    assert!(l.email_alerts_only);
}

#[test]
fn pro_plan_matches_spec() {
    let l = PlanLimits::for_plan(Plan::Pro);
    assert_eq!(l.max_sites, Some(5));
    assert_eq!(l.max_pages, Some(10_000));
    assert_eq!(l.max_duration, Some(Duration::from_secs(2 * HOUR)));
    assert_eq!(l.fastest_schedule, Some(Schedule::Daily));
    assert_eq!(l.manual_crawls, ManualAllowance::PerDay(1));
    assert_eq!(l.history_days, Some(365));
    assert_eq!(l.api_calls_per_day, Some(2_000));
    assert!(!l.email_alerts_only);
}

#[test]
fn agency_plan_matches_spec() {
    let l = PlanLimits::for_plan(Plan::Agency);
    assert_eq!(l.max_sites, Some(25));
    assert_eq!(l.max_pages, Some(50_000));
    assert_eq!(l.max_duration, Some(Duration::from_secs(6 * HOUR)));
    assert_eq!(l.fastest_schedule, Some(Schedule::Daily));
    assert_eq!(l.manual_crawls, ManualAllowance::Unlimited);
    assert_eq!(l.history_days, Some(365));
    assert_eq!(l.api_calls_per_day, Some(10_000));
    assert!(!l.email_alerts_only);
}

#[test]
fn self_hosted_has_no_plan_caps_but_finite_safety_defaults() {
    let l = PlanLimits::for_plan(Plan::SelfHosted);
    assert_eq!(l.max_sites, None);
    assert_eq!(l.max_pages, Some(100_000));
    assert_eq!(l.max_duration, Some(Duration::from_secs(24 * HOUR)));
    assert_eq!(l.fastest_schedule, Some(Schedule::Daily));
    assert_eq!(l.manual_crawls, ManualAllowance::Unlimited);
    assert_eq!(l.history_days, Some(365));
    assert_eq!(l.api_calls_per_day, None);
    assert!(!l.email_alerts_only);
}

#[test]
fn quick_audit_is_100_pages_in_2_minutes_and_nothing_else() {
    let l = PlanLimits::quick_audit();
    assert_eq!(l.max_pages, Some(100));
    assert_eq!(l.max_duration, Some(Duration::from_secs(120)));
    assert_eq!(l.max_sites, Some(0));
    assert_eq!(l.fastest_schedule, None);
    assert_eq!(l.manual_crawls, ManualAllowance::PerWeek(0));
    assert_eq!(l.history_days, Some(7));
    assert_eq!(l.api_calls_per_day, Some(0));
}

#[test]
fn llms_txt_link_checks_are_5_on_free_and_20_elsewhere() {
    assert_eq!(PlanLimits::quick_audit().llms_link_checks, 0);
    assert_eq!(PlanLimits::for_plan(Plan::Free).llms_link_checks, 5);
    for plan in [Plan::Pro, Plan::Agency, Plan::SelfHosted] {
        assert_eq!(PlanLimits::for_plan(plan).llms_link_checks, 20);
    }
}

#[test]
fn synthetic_probe_caps_match_the_b1_table() {
    let secs = Duration::from_secs;
    for (limits, urls, budget, max_requests) in [
        (PlanLimits::quick_audit(), 0, secs(0), 0),
        (PlanLimits::for_plan(Plan::Free), 1, secs(45), 30),
        (PlanLimits::for_plan(Plan::Pro), 3, secs(90), 70),
        (PlanLimits::for_plan(Plan::Agency), 5, secs(120), 120),
        (PlanLimits::for_plan(Plan::SelfHosted), 5, secs(180), 160),
    ] {
        assert_eq!(limits.probe_urls, urls);
        assert_eq!(limits.probe_budget, budget);
        assert_eq!(limits.probe_max_requests, max_requests);
    }
}

#[test]
fn manual_ai_rechecks_per_day_follow_the_plan() {
    assert_eq!(
        PlanLimits::for_plan(Plan::Free).ai_rechecks_per_day,
        Some(3)
    );
    assert_eq!(
        PlanLimits::for_plan(Plan::Pro).ai_rechecks_per_day,
        Some(20)
    );
    assert_eq!(
        PlanLimits::for_plan(Plan::Agency).ai_rechecks_per_day,
        Some(60)
    );
    assert_eq!(
        PlanLimits::for_plan(Plan::SelfHosted).ai_rechecks_per_day,
        None
    );
    // A no-signup audit has no incidents to recheck.
    assert_eq!(PlanLimits::quick_audit().ai_rechecks_per_day, Some(0));
}
