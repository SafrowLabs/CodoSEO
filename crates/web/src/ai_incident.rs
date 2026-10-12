//! What the AI access screen, the REST API and the cloud MCP tools all say about one incident
//! and do to it: its state, the fix, and the messages for a refused action. One place, so the
//! three surfaces cannot drift: the same words, the same snooze choices, the same plan limits.

use codoseo_core::plan::PlanLimits;
use codoseo_geo::findings::Evidence;
use codoseo_geo::incident::Before;
use codoseo_geo::ledger;
use codoseo_geo::remedy::{
    IncidentFacts, Remedy, RemedyContext, before_text, clean, now_text, remedy,
};
use codoseo_geo::report::AccessReport;
use codoseo_store::geo::{
    self, Incident, IncidentActionError, Recheck, RecheckError, RecheckOrigin, RecheckOutcome,
    RecheckQueued, RecheckStatus,
};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::fmt;

/// The snoozes offered, in days.
pub const SNOOZE_DAYS: [i64; 3] = [1, 7, 30];

/// Said for any other number of days.
pub const SNOOZE_CHOICES: &str = "Snooze for 1, 7 or 30 days.";

/// A site string's cap in a recheck note.
const MAX_NOTE: usize = 200;

/// Whether an open incident counts in the badges and panels: not accepted, and not snoozed until
/// later. Mirrors `geo::open_counts`.
pub fn asks_attention(i: &Incident, now: OffsetDateTime) -> bool {
    i.accepted_at.is_none() && i.snoozed_until.is_none_or(|t| t <= now)
}

/// The number of days of a snooze, when it is one of [`SNOOZE_DAYS`].
pub fn snooze_days(days: Option<i64>) -> Result<i64, Refusal> {
    days.filter(|d| SNOOZE_DAYS.contains(d))
        .ok_or_else(|| Refusal::BadRequest(SNOOZE_CHOICES.to_owned()))
}

/// Why an action on an incident was refused, as its owner reads it. The web app and the API
/// each turn it into their own error type.
#[derive(Debug)]
pub enum Refusal {
    NotFound,
    /// 409
    Conflict(String),
    /// 403: the plan's allowance.
    Limit(String),
    /// 400
    BadRequest(String),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for Refusal {
    fn from(e: sqlx::Error) -> Refusal {
        Refusal::Db(e)
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!(
        "{} {}",
        fmt::thousands(n as i64),
        if n == 1 { one } else { many }
    )
}

/// Why accepting or snoozing was refused.
impl From<IncidentActionError> for Refusal {
    fn from(e: IncidentActionError) -> Refusal {
        use codoseo_geo::findings::FindingKind;
        match e {
            IncidentActionError::NotFound => Refusal::NotFound,
            IncidentActionError::Resolved => {
                Refusal::Conflict("That issue is already resolved.".to_owned())
            }
            IncidentActionError::NotAcceptable(FindingKind::RobotsUnavailable) => {
                Refusal::BadRequest(
                    "A failing robots.txt can't be accepted: it can only be snoozed while you fix it."
                        .to_owned(),
                )
            }
            IncidentActionError::NotAcceptable(_) => Refusal::BadRequest(
                "This issue is a choice through your intent: use Mark intended instead."
                    .to_owned(),
            ),
            IncidentActionError::SnoozeInPast => {
                Refusal::BadRequest("A snooze has to end in the future.".to_owned())
            }
            IncidentActionError::Db(e) => Refusal::Db(e),
        }
    }
}

/// Queues a manual recheck against the daily allowance of `limits` (the site owner's plan).
pub async fn request_recheck(
    pool: &PgPool,
    site_id: Uuid,
    incident_id: i64,
    account_id: Uuid,
    limits: &PlanLimits,
) -> Result<RecheckQueued, Refusal> {
    match geo::request_recheck(pool, site_id, incident_id, account_id, limits).await {
        Ok(q) => Ok(q),
        Err(RecheckError::Busy) => {
            let latest = geo::latest_rechecks(pool, site_id).await?;
            Err(Refusal::Conflict(match latest.get(&incident_id) {
                Some(r) if r.origin == RecheckOrigin::Followup => {
                    format!("Already verifying — due at {}.", fmt::datetime(r.due_at))
                }
                _ => "A recheck of this issue is already queued.".to_owned(),
            }))
        }
        Err(RecheckError::NotFound) => Err(Refusal::NotFound),
        Err(RecheckError::Resolved) => Err(Refusal::Conflict(
            "That issue is already resolved.".to_owned(),
        )),
        Err(RecheckError::Cooldown(at)) => Err(Refusal::Conflict(format!(
            "Rechecked moments ago. Try again at {}.",
            fmt::datetime(at)
        ))),
        Err(RecheckError::LimitReached(Some(at))) => Err(Refusal::Limit(format!(
            "{} per day on your plan — next at {}.",
            plural(
                usize::from(limits.ai_rechecks_per_day.unwrap_or(0)),
                "recheck",
                "rechecks"
            ),
            fmt::datetime(at)
        ))),
        Err(RecheckError::LimitReached(None)) => Err(Refusal::Limit(
            "Your plan has no rechecks. The next crawl will check this issue.".to_owned(),
        )),
        Err(RecheckError::CrawlActive) => Err(Refusal::Conflict(
            "A crawl is running; it will judge this.".to_owned(),
        )),
        Err(RecheckError::Db(e)) => Err(Refusal::Db(e)),
    }
}

// ---- what is going on with an incident --------------------------------------------------------

/// A recheck under way, a recovery being verified, or one waiting for the next crawl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The fix was seen and a follow-up recheck, due at this time, confirms it.
    Verifying { due_at: OffsetDateTime },
    /// A manual recheck is queued or running (`recovered`: after a fix was seen).
    Rechecking { running: bool, recovered: bool },
    /// The fix was seen, but nothing is scheduled to confirm it: the next crawl will.
    Waiting,
}

/// What an incident is waiting on, if anything. `latest` is its newest recheck.
pub fn phase(i: &Incident, latest: Option<&Recheck>) -> Option<Phase> {
    let active =
        latest.filter(|r| matches!(r.status, RecheckStatus::Queued | RecheckStatus::Running));
    if i.recovered_at.is_some() && i.verified_at.is_none() {
        return Some(match active {
            Some(r) if r.origin == RecheckOrigin::Followup => Phase::Verifying { due_at: r.due_at },
            Some(r) => Phase::Rechecking {
                running: r.status == RecheckStatus::Running,
                recovered: true,
            },
            // The follow-up failed, a crawl took over, or three in a row were inconclusive.
            None => Phase::Waiting,
        });
    }
    active.map(|r| Phase::Rechecking {
        running: r.status == RecheckStatus::Running,
        recovered: false,
    })
}

/// The last finished recheck of an incident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastRecheck {
    /// `clean`, `still_failing`, `inconclusive`, `failed` (it never ran on our side) or
    /// `superseded` (a crawl judged instead).
    pub outcome: &'static str,
    pub at: Option<OffsetDateTime>,
    /// Why an inconclusive one was, cleaned as untrusted site data.
    pub notes: Vec<String>,
}

/// The newest recheck as a result; `None` while it is queued or running.
pub fn last_recheck(r: &Recheck) -> Option<LastRecheck> {
    let outcome = match (r.status, r.outcome) {
        (RecheckStatus::Done, Some(RecheckOutcome::Clean)) => "clean",
        (RecheckStatus::Done, Some(RecheckOutcome::StillFailing)) => "still_failing",
        (RecheckStatus::Done, Some(RecheckOutcome::Inconclusive)) => "inconclusive",
        (RecheckStatus::Failed, _) => "failed",
        (RecheckStatus::Superseded, _) => "superseded",
        _ => return None,
    };
    let notes = if outcome == "inconclusive" {
        r.observations
            .as_ref()
            .and_then(|o| o.get("notes"))
            .and_then(|n| n.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|n| clean(n, MAX_NOTE))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    Some(LastRecheck {
        outcome,
        at: r.finished_at,
        notes,
    })
}

// ---- the fix ----------------------------------------------------------------------------------

/// An incident's remedy and the facts the ticket and the agent prompt are written from.
pub struct Fix {
    pub remedy: Remedy,
    pub facts: IncidentFacts,
    pub before_line: Option<String>,
    pub now_line: String,
}

/// The URLs an incident affects, once each, in the order its evidence lists them.
pub fn affected_urls(ev: &Evidence) -> Vec<String> {
    ev.urls
        .sample
        .iter()
        .chain(ev.content.iter().map(|c| &c.url))
        .chain(ev.probes.iter().flat_map(|p| &p.urls))
        .fold(Vec::new(), |mut out, u| {
            if !out.contains(u) {
                out.push(u.clone());
            }
            out
        })
}

/// The remedy of `i` for the site at `origin`, in the context of the latest `report`.
pub fn fix(origin: &str, report: Option<&AccessReport>, i: &Incident, ev: &Evidence) -> Fix {
    let before: Option<Before> = i
        .before
        .as_ref()
        .and_then(|b| serde_json::from_value(b.clone()).ok());
    let ctx = RemedyContext {
        origin,
        report,
        before: before.as_ref(),
        lag: ledger::lag(i.kind, ev),
    };
    let remedy = remedy(i.kind, &i.subject, ev, &ctx);
    let before_line = before.as_ref().and_then(|b| before_text(i.kind, b));
    let now_line = now_text(i.kind, ev);
    let facts = IncidentFacts {
        severity: i.severity,
        first_seen: fmt::datetime(i.opened_at),
        last_seen: fmt::datetime(i.last_seen_at),
        affected: affected_urls(ev),
        before: before_line.clone(),
        after: Some(now_line.clone()),
    };
    Fix {
        remedy,
        facts,
        before_line,
        now_line,
    }
}
