//! The plan rules for a manual crawl, shared by the Run crawl button and the API's `run_crawl`
//! so both queue in the same lane and refuse with the same words.

use codoseo_core::plan::{ManualAllowance, Plan};
use time::Duration;

/// Priority lane for a manual crawl on a paid plan (spec section 10).
pub const PAID_MANUAL_PRIORITY: i16 = 2;
/// Priority lane for a manual crawl on Free (spec section 10).
pub const FREE_MANUAL_PRIORITY: i16 = 4;

/// The queue lane for a manual crawl on `plan`.
pub fn manual_priority(plan: Plan) -> i16 {
    match plan {
        Plan::Free => FREE_MANUAL_PRIORITY,
        Plan::Pro | Plan::Agency | Plan::SelfHosted => PAID_MANUAL_PRIORITY,
    }
}

pub fn plan_name(plan: Plan) -> &'static str {
    match plan {
        Plan::Free => "Free",
        Plan::Pro => "Pro",
        Plan::Agency => "Agency",
        Plan::SelfHosted => "self-hosted",
    }
}

/// `1 manual crawl a week`, `3 manual crawls a day`; `None` when unlimited.
pub fn allowance_phrase(allowance: ManualAllowance) -> Option<String> {
    let (n, per) = match allowance {
        ManualAllowance::PerWeek(n) => (n, "week"),
        ManualAllowance::PerDay(n) => (n, "day"),
        ManualAllowance::Unlimited => return None,
    };
    let s = if n == 1 { "" } else { "s" };
    Some(format!("{n} manual crawl{s} a {per}"))
}

/// The refusal when the allowance is used up, e.g. "Your Free plan includes 1 manual crawl a
/// week. The next one is available in 3 days."
pub fn limit_message(plan: Plan, allowance: ManualAllowance, wait: Duration) -> String {
    let phrase = allowance_phrase(allowance).unwrap_or_else(|| "manual crawls".to_owned());
    format!(
        "Your {} plan includes {phrase}. The next one is available {}.",
        plan_name(plan),
        until(wait)
    )
}

/// `in a minute`, `in 12 minutes`, `in 5 hours`, `in 3 days` (rounded up below a day, to the
/// nearest day above).
pub fn until(wait: Duration) -> String {
    let minutes = (wait.whole_seconds().max(0) + 59) / 60;
    if minutes <= 1 {
        return "in a minute".to_owned();
    }
    if minutes < 60 {
        return format!("in {minutes} minutes");
    }
    let hours = (minutes + 59) / 60;
    if hours < 24 {
        let s = if hours == 1 { "" } else { "s" };
        return format!("in {hours} hour{s}");
    }
    let days = (hours + 12) / 24;
    let s = if days == 1 { "" } else { "s" };
    format!("in {days} day{s}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_lanes() {
        assert_eq!(manual_priority(Plan::Free), 4);
        assert_eq!(manual_priority(Plan::Pro), 2);
        assert_eq!(manual_priority(Plan::Agency), 2);
        assert_eq!(manual_priority(Plan::SelfHosted), 2);
    }

    #[test]
    fn waits() {
        assert_eq!(until(Duration::seconds(-5)), "in a minute");
        assert_eq!(until(Duration::seconds(50)), "in a minute");
        assert_eq!(until(Duration::minutes(12)), "in 12 minutes");
        assert_eq!(
            until(Duration::minutes(59) + Duration::seconds(59)),
            "in 1 hour"
        );
        assert_eq!(
            until(Duration::hours(4) + Duration::minutes(10)),
            "in 5 hours"
        );
        assert_eq!(until(Duration::hours(25)), "in 1 day");
        assert_eq!(until(Duration::days(3) - Duration::seconds(1)), "in 3 days");
        assert_eq!(until(Duration::days(6) + Duration::hours(23)), "in 7 days");
    }

    #[test]
    fn limit_messages() {
        assert_eq!(
            limit_message(
                Plan::Free,
                ManualAllowance::PerWeek(1),
                Duration::days(3) - Duration::seconds(1)
            ),
            "Your Free plan includes 1 manual crawl a week. The next one is available in 3 days."
        );
        assert_eq!(
            limit_message(Plan::Pro, ManualAllowance::PerDay(2), Duration::minutes(30)),
            "Your Pro plan includes 2 manual crawls a day. The next one is available in 30 minutes."
        );
    }
}
