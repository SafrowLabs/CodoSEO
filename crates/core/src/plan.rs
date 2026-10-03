//! Plans and their limits. `None` means unlimited.

use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plan {
    Free,
    Pro,
    Agency,
    SelfHosted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Schedule {
    Weekly,
    Daily,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualAllowance {
    PerWeek(u32),
    PerDay(u32),
    Unlimited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanLimits {
    pub max_sites: Option<u32>,
    pub max_pages: Option<u32>,
    pub max_duration: Option<Duration>,
    /// The most frequent schedule allowed; `None` means no scheduled crawls.
    pub fastest_schedule: Option<Schedule>,
    pub manual_crawls: ManualAllowance,
    pub history_days: Option<u32>,
    pub api_calls_per_day: Option<u32>,
    pub email_alerts_only: bool,
}

const HOUR: u64 = 3600;

impl PlanLimits {
    pub fn for_plan(plan: Plan) -> PlanLimits {
        match plan {
            Plan::Free => PlanLimits {
                max_sites: Some(1),
                max_pages: Some(500),
                max_duration: Some(Duration::from_secs(600)),
                fastest_schedule: Some(Schedule::Weekly),
                manual_crawls: ManualAllowance::PerWeek(1),
                history_days: Some(30),
                api_calls_per_day: Some(100),
                email_alerts_only: true,
            },
            Plan::Pro => PlanLimits {
                max_sites: Some(5),
                max_pages: Some(10_000),
                max_duration: Some(Duration::from_secs(2 * HOUR)),
                fastest_schedule: Some(Schedule::Daily),
                manual_crawls: ManualAllowance::PerDay(1),
                history_days: Some(365),
                api_calls_per_day: Some(2_000),
                email_alerts_only: false,
            },
            Plan::Agency => PlanLimits {
                max_sites: Some(25),
                max_pages: Some(50_000),
                max_duration: Some(Duration::from_secs(6 * HOUR)),
                fastest_schedule: Some(Schedule::Daily),
                manual_crawls: ManualAllowance::Unlimited,
                history_days: Some(365),
                api_calls_per_day: Some(10_000),
                email_alerts_only: false,
            },
            // No plan caps, but finite defaults so a calendar or faceted search
            // can't crawl forever. Self-hosters can raise them in config.
            Plan::SelfHosted => PlanLimits {
                max_sites: None,
                max_pages: Some(100_000),
                max_duration: Some(Duration::from_secs(24 * HOUR)),
                fastest_schedule: Some(Schedule::Daily),
                manual_crawls: ManualAllowance::Unlimited,
                history_days: Some(365),
                api_calls_per_day: None,
                email_alerts_only: false,
            },
        }
    }

    /// The no-signup audit: one 100-page crawl, nothing else.
    pub fn quick_audit() -> PlanLimits {
        PlanLimits {
            max_sites: Some(0),
            max_pages: Some(100),
            max_duration: Some(Duration::from_secs(120)),
            fastest_schedule: None,
            manual_crawls: ManualAllowance::PerWeek(0),
            history_days: Some(7),
            api_calls_per_day: Some(0),
            email_alerts_only: true,
        }
    }
}
