//! What the AI access screen, the REST API and the cloud MCP tools all say about one incident
//! and do to it: its state, the fix, and the messages for a refused action. One place, so the
//! three surfaces cannot drift: the same words, the same snooze choices, the same plan limits.

use codoseo_core::check::Severity;
use codoseo_core::plan::PlanLimits;
use codoseo_geo::findings::{Evidence, Grade};
use codoseo_geo::incident::Before;
use codoseo_geo::ledger;
use codoseo_geo::remedy::{
    IncidentFacts, Remedy, RemedyContext, before_text, clean, now_text, remedy,
};
use codoseo_geo::report::AccessReport;
use codoseo_mcp::cloud::types::AiIncidentState;
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
                    format!("Already verifying — due at {}.", fmt::until(r.due_at))
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

/// What the owner and CodoSEO are doing about an open incident. The owner's decisions win over
/// what CodoSEO is doing about it.
pub fn state(i: &Incident, latest: Option<&Recheck>, now: OffsetDateTime) -> AiIncidentState {
    if i.accepted_at.is_some() {
        return AiIncidentState::Accepted;
    }
    if i.snoozed_until.is_some_and(|t| t > now) {
        return AiIncidentState::Snoozed;
    }
    match phase(i, latest) {
        None => AiIncidentState::Open,
        Some(Phase::Verifying { .. }) => AiIncidentState::Verifying,
        Some(Phase::Waiting) => AiIncidentState::WaitingForCrawl,
        Some(Phase::Rechecking { running: true, .. }) => AiIncidentState::RecheckRunning,
        Some(Phase::Rechecking { .. }) => AiIncidentState::RecheckQueued,
    }
}

/// The last finished recheck of an incident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastRecheck {
    /// `clean`, `still_failing`, `inconclusive`, `failed` (it could not run, or the site did not answer) or
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
    /// What the last crawl saw (the evidence).
    pub now_line: String,
    /// What to call `now_line`: "Now", or "At last crawl" once a recheck saw the problem gone
    /// (the incident is recovered and being verified), so the card never says "Now" about what
    /// the fix already changed.
    pub now_label: &'static str,
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
    let recovered = i.recovered_at.is_some();
    let after = if recovered {
        format!("At the last crawl: {now_line} (a recheck has since seen it fixed)")
    } else {
        now_line.clone()
    };
    let facts = IncidentFacts {
        severity: i.severity,
        first_seen: fmt::datetime(i.opened_at),
        last_seen: fmt::datetime(i.last_seen_at),
        affected: affected_urls(ev),
        before: before_line.clone(),
        after: Some(after),
    };
    Fix {
        remedy,
        facts,
        before_line,
        now_line,
        now_label: if recovered { "At last crawl" } else { "Now" },
    }
}

// ---- the AI access panel on the overview -------------------------------------------------------

/// Incidents per severity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BySeverity {
    pub critical: u32,
    pub warning: u32,
    pub notice: u32,
}

impl BySeverity {
    pub fn total(&self) -> u32 {
        self.critical + self.warning + self.notice
    }

    fn add(&mut self, s: Severity) {
        match s {
            Severity::Critical => self.critical += 1,
            Severity::Warning => self.warning += 1,
            Severity::Notice => self.notice += 1,
        }
    }
}

/// What the overview's AI access panel says about a site's open incidents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PanelCounts {
    /// Open and asking for attention: neither accepted nor snoozed. As the sidebar counts.
    pub open: BySeverity,
    pub accepted: BySeverity,
    /// Snoozed until later (and not accepted).
    pub snoozed: BySeverity,
    /// The weakest evidence among the open ones.
    pub weakest_grade: Option<Grade>,
    /// Fixes seen and not yet confirmed, of the incidents that ask for attention.
    pub verifying: u32,
}

/// Counts `open` incidents for the panel. `latest` is each incident's newest recheck.
pub fn panel_counts(
    open: &[Incident],
    latest: &std::collections::HashMap<i64, Recheck>,
    now: OffsetDateTime,
) -> PanelCounts {
    let mut c = PanelCounts::default();
    for i in open {
        if i.accepted_at.is_some() {
            c.accepted.add(i.severity);
            continue;
        }
        if !asks_attention(i, now) {
            c.snoozed.add(i.severity);
            continue;
        }
        c.open.add(i.severity);
        let grade = ledger::for_kind(i.kind).grade;
        if c.weakest_grade.is_none_or(|g| grade.rank() < g.rank()) {
            c.weakest_grade = Some(grade);
        }
    }
    // Like the open counts, only the incidents that ask for attention: an accepted or snoozed
    // one is set aside, whatever its follow-up is doing.
    c.verifying = open
        .iter()
        .filter(|i| asks_attention(i, now))
        .filter(|i| matches!(phase(i, latest.get(&i.id)), Some(Phase::Verifying { .. })))
        .count() as u32;
    c
}

// ---- exports ----------------------------------------------------------------------------------

/// How far back an export lists resolved incidents.
pub const EXPORT_RESOLVED_DAYS: i32 = 90;

/// The columns of an incident export, in order.
pub const EXPORT_COLUMNS: [&str; 13] = [
    "kind",
    "subject",
    "severity",
    "grade",
    "title",
    "state",
    "first_seen",
    "last_seen",
    "resolved_at",
    "resolved_by",
    "verified_at",
    "affected_urls",
    "sources",
];

/// One incident of an export.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IncidentRecord {
    pub kind: String,
    pub subject: String,
    pub severity: Severity,
    pub grade: Grade,
    pub title: String,
    /// `resolved`, or what [`state`] says of an open one.
    pub state: String,
    #[serde(with = "time::serde::rfc3339")]
    pub first_seen: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub resolved_at: Option<OffsetDateTime>,
    /// `crawl`, `recheck` or `intent`.
    pub resolved_by: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub verified_at: Option<OffsetDateTime>,
    pub affected_urls: Vec<String>,
    pub sources: Vec<String>,
}

/// The site's open incidents and those resolved in the last [`EXPORT_RESOLVED_DAYS`] days.
pub async fn export_records(
    pool: &PgPool,
    site_id: Uuid,
    now: OffsetDateTime,
) -> Result<Vec<IncidentRecord>, sqlx::Error> {
    let incidents = geo::export_incidents(pool, site_id, EXPORT_RESOLVED_DAYS).await?;
    let latest = geo::latest_rechecks(pool, site_id).await?;
    Ok(incidents
        .into_iter()
        .map(|i| {
            let ev: Evidence = serde_json::from_value(i.evidence.clone()).unwrap_or_default();
            let state = match i.resolved_at {
                Some(_) => "resolved".to_owned(),
                None => serde_json::to_value(state(&i, latest.get(&i.id), now))
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
            };
            IncidentRecord {
                kind: i.kind.slug().to_owned(),
                subject: i.subject,
                severity: i.severity,
                grade: ledger::for_kind(i.kind).grade,
                title: i.title,
                state,
                first_seen: i.opened_at,
                last_seen: i.last_seen_at,
                resolved_at: i.resolved_at,
                resolved_by: i.resolved_by,
                verified_at: i.verified_at,
                affected_urls: affected_urls(&ev),
                sources: ledger::finding_sources(i.kind, &ev),
            }
        })
        .collect())
}

fn severity_slug(s: Severity) -> &'static str {
    match s {
        Severity::Critical => "critical",
        Severity::Warning => "warning",
        Severity::Notice => "notice",
    }
}

/// The records as CSV in [`EXPORT_COLUMNS`] order. Every text cell goes through `text`, which
/// keeps a spreadsheet from running one that starts like a formula: titles and URLs came from
/// the crawled site. URLs and sources are joined by a space.
pub fn to_csv(
    records: &[IncidentRecord],
    text: impl Fn(Option<&str>) -> String,
) -> Result<String, String> {
    let failed = |e: csv::Error| format!("could not write the incidents CSV: {e}");
    let mut w = csv::Writer::from_writer(Vec::new());
    w.write_record(EXPORT_COLUMNS).map_err(failed)?;
    let when = |t: Option<OffsetDateTime>| {
        t.and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_default()
    };
    for r in records {
        w.write_record([
            text(Some(&r.kind)),
            text(Some(&r.subject)),
            severity_slug(r.severity).to_owned(),
            r.grade.label().to_owned(),
            text(Some(&r.title)),
            r.state.clone(),
            when(Some(r.first_seen)),
            when(Some(r.last_seen)),
            when(r.resolved_at),
            text(r.resolved_by.as_deref()),
            when(r.verified_at),
            text(Some(&r.affected_urls.join(" "))),
            text(Some(&r.sources.join(" "))),
        ])
        .map_err(failed)?;
    }
    let bytes = w
        .into_inner()
        .map_err(|e| format!("could not write the incidents CSV: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("the incidents CSV is not UTF-8: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_geo::findings::FindingKind;
    use std::collections::HashMap;

    fn incident(id: i64, kind: FindingKind, severity: Severity) -> Incident {
        let at = OffsetDateTime::UNIX_EPOCH;
        Incident {
            id,
            kind,
            subject: String::new(),
            severity,
            title: String::new(),
            summary: String::new(),
            evidence: serde_json::json!({}),
            opened_crawl_id: None,
            opened_at: at,
            last_seen_at: at,
            resolved_at: None,
            resolution: None,
            quiet: false,
            before: None,
            accepted_at: None,
            accepted_by: None,
            snoozed_until: None,
            recovered_at: None,
            verified_at: None,
            resolved_by: None,
        }
    }

    #[test]
    fn a_recovered_incident_says_its_now_is_the_last_crawls() {
        let mut i = incident(1, FindingKind::LlmsTxtUnavailable, Severity::Notice);
        i.evidence = serde_json::json!({ "llms_status": 503 });
        i.before = Some(serde_json::json!({ "llms": { "status": 200, "state": "available" } }));
        let ev: Evidence = serde_json::from_value(i.evidence.clone()).unwrap();
        let open = fix("https://example.com/", None, &i, &ev);
        assert_eq!(open.now_label, "Now");
        assert_eq!(open.facts.after.as_deref(), Some(open.now_line.as_str()));

        // A recheck saw it fixed: what the last crawl saw is not "now" any more.
        i.recovered_at = Some(OffsetDateTime::now_utc());
        let verifying = fix("https://example.com/", None, &i, &ev);
        assert_eq!(verifying.now_label, "At last crawl");
        assert_eq!(verifying.now_line, open.now_line);
        let after = verifying.facts.after.clone().unwrap();
        assert!(after.starts_with("At the last crawl: "), "{after}");
        assert!(
            after.contains("a recheck has since seen it fixed"),
            "{after}"
        );
        let task = codoseo_geo::remedy::developer_task(&verifying.remedy, &verifying.facts);
        assert!(task.contains("- Now: At the last crawl: "), "{task}");
    }

    #[test]
    fn the_panel_counts_what_the_sidebar_counts_and_sets_the_rest_aside() {
        let now = OffsetDateTime::now_utc();
        let mut accepted = incident(2, FindingKind::LlmsTxtUnavailable, Severity::Notice);
        accepted.accepted_at = Some(now);
        let mut snoozed = incident(3, FindingKind::BotsBlocked, Severity::Critical);
        snoozed.snoozed_until = Some(now + time::Duration::days(1));
        let mut lapsed = incident(4, FindingKind::ContentMissing, Severity::Warning);
        lapsed.snoozed_until = Some(now - time::Duration::days(1));
        let open = vec![
            incident(1, FindingKind::FetchBlocked, Severity::Critical),
            accepted,
            snoozed,
            lapsed,
        ];
        let c = panel_counts(&open, &HashMap::new(), now);
        assert_eq!((c.open.critical, c.open.warning, c.open.notice), (1, 1, 0));
        assert_eq!(c.accepted.notice, 1);
        assert_eq!(c.snoozed.critical, 1);
        assert_eq!(c.verifying, 0);
        // fetch_blocked is graded B, content_missing A-: the weakest of the open ones is B.
        assert_eq!(c.weakest_grade, Some(Grade::B));
    }

    #[test]
    fn only_fixes_of_incidents_that_ask_attention_are_being_verified() {
        let now = OffsetDateTime::now_utc();
        let followup = |id: i64| Recheck {
            id: id * 10,
            site_id: uuid::Uuid::nil(),
            incident_id: id,
            origin: RecheckOrigin::Followup,
            requested_by: None,
            status: RecheckStatus::Queued,
            due_at: now + time::Duration::hours(1),
            started_at: None,
            finished_at: None,
            outcome: None,
            failure: None,
            observations: None,
            created_at: now,
        };
        let recovered = |id: i64| {
            let mut i = incident(id, FindingKind::FetchBlocked, Severity::Warning);
            i.recovered_at = Some(now);
            i
        };
        let mut accepted = recovered(2);
        accepted.accepted_at = Some(now);
        let mut snoozed = recovered(3);
        snoozed.snoozed_until = Some(now + time::Duration::days(1));
        let open = vec![recovered(1), accepted, snoozed];
        let latest: HashMap<i64, Recheck> = (1..=3).map(|id| (id, followup(id))).collect();
        assert_eq!(panel_counts(&open, &latest, now).verifying, 1);
    }

    #[test]
    fn no_open_incident_means_no_grade() {
        let c = panel_counts(&[], &HashMap::new(), OffsetDateTime::now_utc());
        assert_eq!(c, PanelCounts::default());
    }
}
