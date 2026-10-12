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
    /// Pages fetched as the AI search and user-fetch bots after each monitored crawl (the
    /// synthetic fetch probes); 0 means no probes.
    pub probe_urls: u8,
    /// Wall time for one probe run: a page is probed only when what is left covers it.
    pub probe_budget: Duration,
    /// Requests for one probe run, baseline, retries and confirmations included.
    pub probe_max_requests: u16,
    /// Links from the site's `llms.txt` that get a status check of their own per crawl.
    pub llms_link_checks: u16,
    /// Manual AI access rechecks an account may ask for in 24 hours (follow-ups the system
    /// queues don't count); `None` means unlimited.
    pub ai_rechecks_per_day: Option<u16>,
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
                probe_urls: 1,
                probe_budget: Duration::from_secs(45),
                probe_max_requests: 30,
                llms_link_checks: 5,
                ai_rechecks_per_day: Some(3),
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
                probe_urls: 3,
                probe_budget: Duration::from_secs(90),
                probe_max_requests: 70,
                llms_link_checks: 20,
                ai_rechecks_per_day: Some(20),
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
                probe_urls: 5,
                probe_budget: Duration::from_secs(120),
                probe_max_requests: 120,
                llms_link_checks: 20,
                ai_rechecks_per_day: Some(60),
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
                // `CODOSEO_PROBE_URLS` can change it, up to 10 pages.
                probe_urls: 5,
                probe_budget: Duration::from_secs(180),
                probe_max_requests: 160,
                llms_link_checks: 20,
                ai_rechecks_per_day: None,
            },
        }
    }

    /// The no-signup audit: one 100-page crawl, nothing else (no synthetic probes).
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
            // A no-signup audit never sends a request as an AI bot.
            probe_urls: 0,
            probe_budget: Duration::ZERO,
            probe_max_requests: 0,
            // No site signals, so no llms.txt.
            llms_link_checks: 0,
            ai_rechecks_per_day: Some(0),
        }
    }
}
