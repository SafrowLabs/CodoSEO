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
fn self_hosted_is_unlimited_with_one_year_default_history() {
    let l = PlanLimits::for_plan(Plan::SelfHosted);
    assert_eq!(l.max_sites, None);
    assert_eq!(l.max_pages, None);
    assert_eq!(l.max_duration, None);
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
