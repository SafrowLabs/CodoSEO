//! AI access in the database: the owner's intent per site, the access report of each crawl, and
//! the incidents the findings open and resolve.
//!
//! A crawl's report and findings are pure data ([`codoseo_geo`]); this module is where they meet
//! what is already open. [`apply_geo`] is the one place that does it, in the caller's
//! transaction: `finalize` calls it for a finished crawl and [`record_failed_crawl`] for one that
//! failed for good with a failing robots.txt, so the two never disagree. Both, and
//! [`reevaluate_quietly`], lock the site row first and draw the findings under the intent they
//! read there, so a crawl and an intent change never undo each other.
//!
//! The first report that judges a kind of finding is that kind's *baseline*: the incidents it
//! opens are `quiet` and write no change rows, so switching the feature on (or a site whose first
//! crawls failed) does not fire an alert storm. A change of intent ([`reevaluate_quietly`]) is
//! quiet the same way.
//!
//! The owner can [`accept`] an incident (it stays listed, writes no change rows and leaves the
//! badges) or [`snooze`] it (its change rows are still recorded, already marked alerted, so the
//! changes page and the digest stay truthful but nothing alerts at once, until the snooze ends on
//! its own).

use std::collections::{BTreeSet, HashMap, HashSet};

use codoseo_core::change::{Change, ChangeKind};
use codoseo_core::check::Severity;
use codoseo_core::plan::PlanLimits;
use codoseo_geo::Intent;
use codoseo_geo::eligibility::DirectiveSlug;
use codoseo_geo::findings::{
    Evidence, Finding, FindingKind, evaluated_kinds, findings, hold_unjudged_content,
    hold_unjudged_probes,
};
use codoseo_geo::incident::{OpenIncident, Transition, before_of, reconcile};
use codoseo_geo::llms::LlmsTxtReport;
use codoseo_geo::report::{AccessReport, Declared};
use codoseo_geo::robots::Pair;
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::dbenum::enum_slug;
use crate::finalize::{enqueue_alert_job, insert_changes};
use crate::jobs::{JobKind, enqueue_at_in};

/// Reports kept per site: the newest two, like the per-URL data of a crawl, plus older ones the
/// newest two can't stand in for (see [`reports_to_keep`]).
const KEEP_REPORTS: usize = 2;
/// Every kind of finding, for the kinds a run opens quietly.
const ALL_KINDS: [FindingKind; 7] = [
    FindingKind::RobotsUnavailable,
    FindingKind::BotsBlocked,
    FindingKind::BotsNotBlocked,
    FindingKind::AnswersRestricted,
    FindingKind::FetchBlocked,
    FindingKind::ContentMissing,
    FindingKind::LlmsTxtUnavailable,
];
/// Longest `before_value`/`after_value` a change row gets, in characters.
const MAX_CHANGE_TEXT: usize = 300;
/// Most per-bot overrides an intent may hold.
const MAX_BOT_OVERRIDES: usize = 200;
/// Longest bot token an intent may hold.
const MAX_TOKEN_LEN: usize = 100;

/// Everything the store needs from one crawl's AI access evaluation: its report. The findings
/// are drawn from it when it is recorded, under the intent the site has then.
#[derive(Debug, Clone)]
pub struct GeoInput {
    pub report: AccessReport,
}

impl GeoInput {
    pub fn new(report: AccessReport) -> GeoInput {
        GeoInput { report }
    }
}

/// The newest report of a site, whatever became of its crawl.
#[derive(Debug, Clone)]
pub struct LatestReport {
    pub crawl_id: Uuid,
    /// The crawl's status (`done`, `failed`, `queued` while it waits for its retry, ...).
    pub crawl_status: String,
    pub created_at: OffsetDateTime,
    pub report: AccessReport,
}

/// One incident as the UI shows it.
#[derive(Debug, Clone)]
pub struct Incident {
    pub id: i64,
    pub kind: FindingKind,
    pub subject: String,
    pub severity: Severity,
    pub title: String,
    pub summary: String,
    /// The finding's structured evidence (`codoseo_geo::findings::Evidence`).
    pub evidence: Value,
    pub opened_crawl_id: Option<Uuid>,
    pub opened_at: OffsetDateTime,
    pub last_seen_at: OffsetDateTime,
    pub resolved_at: Option<OffsetDateTime>,
    /// `fixed` (a crawl no longer found it) or `intent` (the owner's intent made it moot).
    pub resolution: Option<String>,
    /// Opened without an alert: part of the site's baseline or of an intent change.
    pub quiet: bool,
    /// What was true before the incident opened (`codoseo_geo::incident::Before`), from the
    /// newest earlier report that judged its kind; `None` for a baseline incident, one an intent
    /// change opened (see [`reevaluate_quietly`]) and those opened before it was kept.
    pub before: Option<Value>,
    /// When the owner accepted the finding as it is, and which account did.
    pub accepted_at: Option<OffsetDateTime>,
    pub accepted_by: Option<Uuid>,
    /// The incident raises no reminders until then.
    pub snoozed_until: Option<OffsetDateTime>,
    /// When a check first saw the problem gone, and when that was confirmed.
    pub recovered_at: Option<OffsetDateTime>,
    pub verified_at: Option<OffsetDateTime>,
    /// What resolved it: `crawl`, `recheck` or `intent`.
    pub resolved_by: Option<String>,
}

#[derive(sqlx::FromRow)]
struct IncidentRow {
    id: i64,
    kind: String,
    subject: String,
    severity: String,
    title: String,
    summary: String,
    evidence: Value,
    opened_crawl_id: Option<Uuid>,
    opened_at: OffsetDateTime,
    last_seen_at: OffsetDateTime,
    resolved_at: Option<OffsetDateTime>,
    resolution: Option<String>,
    quiet: bool,
    before: Option<Value>,
    accepted_at: Option<OffsetDateTime>,
    accepted_by: Option<Uuid>,
    snoozed_until: Option<OffsetDateTime>,
    recovered_at: Option<OffsetDateTime>,
    verified_at: Option<OffsetDateTime>,
    resolved_by: Option<String>,
}

impl IncidentRow {
    fn into_incident(self) -> Option<Incident> {
        Some(Incident {
            id: self.id,
            kind: FindingKind::from_slug(&self.kind)?,
            subject: self.subject,
            severity: parse_severity(&self.severity)?,
            title: self.title,
            summary: self.summary,
            evidence: self.evidence,
            opened_crawl_id: self.opened_crawl_id,
            opened_at: self.opened_at,
            last_seen_at: self.last_seen_at,
            resolved_at: self.resolved_at,
            resolution: self.resolution,
            quiet: self.quiet,
            before: self.before,
            accepted_at: self.accepted_at,
            accepted_by: self.accepted_by,
            snoozed_until: self.snoozed_until,
            recovered_at: self.recovered_at,
            verified_at: self.verified_at,
            resolved_by: self.resolved_by,
        })
    }
}

const INCIDENT_COLUMNS: &str = "id, kind, subject, severity::text AS severity, title, summary, \
     evidence, opened_crawl_id, opened_at, last_seen_at, resolved_at, resolution, quiet, \
     before, accepted_at, accepted_by, snoozed_until, recovered_at, verified_at, resolved_by";

fn parse_severity(s: &str) -> Option<Severity> {
    serde_json::from_value(Value::String(s.to_owned())).ok()
}

/// How a recheck came about. Mirrors `ai_rechecks.origin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecheckOrigin {
    /// The owner asked for it.
    Manual,
    /// Queued by the system after a repair.
    Followup,
}

impl RecheckOrigin {
    pub fn slug(self) -> &'static str {
        match self {
            RecheckOrigin::Manual => "manual",
            RecheckOrigin::Followup => "followup",
        }
    }

    pub fn from_slug(slug: &str) -> Option<RecheckOrigin> {
        match slug {
            "manual" => Some(RecheckOrigin::Manual),
            "followup" => Some(RecheckOrigin::Followup),
            _ => None,
        }
    }
}

/// Where a recheck is. Mirrors `ai_rechecks.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecheckStatus {
    Queued,
    Running,
    Done,
    /// Replaced by a newer recheck or made moot before it ran.
    Superseded,
    Failed,
}

impl RecheckStatus {
    pub fn slug(self) -> &'static str {
        match self {
            RecheckStatus::Queued => "queued",
            RecheckStatus::Running => "running",
            RecheckStatus::Done => "done",
            RecheckStatus::Superseded => "superseded",
            RecheckStatus::Failed => "failed",
        }
    }

    pub fn from_slug(slug: &str) -> Option<RecheckStatus> {
        match slug {
            "queued" => Some(RecheckStatus::Queued),
            "running" => Some(RecheckStatus::Running),
            "done" => Some(RecheckStatus::Done),
            "superseded" => Some(RecheckStatus::Superseded),
            "failed" => Some(RecheckStatus::Failed),
            _ => None,
        }
    }
}

/// What a finished recheck found. Mirrors `ai_rechecks.outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecheckOutcome {
    Clean,
    StillFailing,
    Inconclusive,
}

impl RecheckOutcome {
    pub fn slug(self) -> &'static str {
        match self {
            RecheckOutcome::Clean => "clean",
            RecheckOutcome::StillFailing => "still_failing",
            RecheckOutcome::Inconclusive => "inconclusive",
        }
    }

    pub fn from_slug(slug: &str) -> Option<RecheckOutcome> {
        match slug {
            "clean" => Some(RecheckOutcome::Clean),
            "still_failing" => Some(RecheckOutcome::StillFailing),
            "inconclusive" => Some(RecheckOutcome::Inconclusive),
            _ => None,
        }
    }
}

/// One row of `ai_rechecks`.
#[derive(Debug, Clone)]
pub struct Recheck {
    pub id: i64,
    pub site_id: Uuid,
    pub incident_id: i64,
    pub origin: RecheckOrigin,
    pub requested_by: Option<Uuid>,
    pub status: RecheckStatus,
    pub due_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    pub outcome: Option<RecheckOutcome>,
    pub observations: Option<Value>,
    pub created_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct RecheckRow {
    id: i64,
    site_id: Uuid,
    incident_id: i64,
    origin: String,
    requested_by: Option<Uuid>,
    status: String,
    due_at: OffsetDateTime,
    started_at: Option<OffsetDateTime>,
    finished_at: Option<OffsetDateTime>,
    outcome: Option<String>,
    observations: Option<Value>,
    created_at: OffsetDateTime,
}

impl RecheckRow {
    fn into_recheck(self) -> Option<Recheck> {
        Some(Recheck {
            id: self.id,
            site_id: self.site_id,
            incident_id: self.incident_id,
            origin: RecheckOrigin::from_slug(&self.origin)?,
            requested_by: self.requested_by,
            status: RecheckStatus::from_slug(&self.status)?,
            due_at: self.due_at,
            started_at: self.started_at,
            finished_at: self.finished_at,
            outcome: match self.outcome {
                Some(o) => Some(RecheckOutcome::from_slug(&o)?),
                None => None,
            },
            observations: self.observations,
            created_at: self.created_at,
        })
    }
}

/// One recheck of the site, whatever its status. `None` for another site's id.
pub async fn recheck(
    pool: &PgPool,
    site_id: Uuid,
    id: i64,
) -> Result<Option<Recheck>, sqlx::Error> {
    let row: Option<RecheckRow> = sqlx::query_as(&format!(
        "SELECT {RECHECK_COLUMNS} FROM ai_rechecks WHERE site_id = $1 AND id = $2"
    ))
    .bind(site_id)
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(RecheckRow::into_recheck))
}

/// Why an intent was not saved.
#[derive(Debug, thiserror::Error)]
pub enum IntentError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// The site's intent. A stored value that no longer parses reads as the defaults.
pub async fn get_intent(pool: &PgPool, site_id: Uuid) -> Result<Intent, sqlx::Error> {
    let value: Option<Value> = sqlx::query_scalar("SELECT ai_intent FROM sites WHERE id = $1")
        .bind(site_id)
        .fetch_optional(pool)
        .await?;
    Ok(value
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default())
}

/// Checks an intent before it is stored: sane sizes, bot names that are names, and no accepted
/// `noindex` (it never opens an AI answers incident: it is the SEO checks' business).
pub fn validate_intent(intent: &Intent) -> Result<(), String> {
    if intent.bots.len() > MAX_BOT_OVERRIDES {
        return Err(format!("At most {MAX_BOT_OVERRIDES} bots can be set."));
    }
    for token in intent.bots.keys() {
        let t = token.trim();
        if t.is_empty() || t.len() > MAX_TOKEN_LEN || t.chars().any(char::is_control) {
            return Err(format!("\"{token}\" is not a bot name."));
        }
    }
    if intent.accepted_directives.contains(&DirectiveSlug::Noindex) {
        return Err("noindex can't be accepted here: the SEO checks report it.".to_owned());
    }
    Ok(())
}

/// Saves the site's intent after validating it and round-tripping it through [`Intent`], so
/// what is stored is exactly what [`get_intent`] reads back. Does not touch the incidents: call
/// [`reevaluate_quietly`] afterwards.
pub async fn set_intent(pool: &PgPool, site_id: Uuid, intent: &Intent) -> Result<(), IntentError> {
    validate_intent(intent).map_err(IntentError::Invalid)?;
    let value = serde_json::to_value(intent).map_err(|e| IntentError::Invalid(e.to_string()))?;
    let normalised: Intent =
        serde_json::from_value(value.clone()).map_err(|e| IntentError::Invalid(e.to_string()))?;
    debug_assert_eq!(&normalised, intent);
    sqlx::query("UPDATE sites SET ai_intent = $2 WHERE id = $1")
        .bind(site_id)
        .bind(value)
        .execute(pool)
        .await?;
    Ok(())
}

/// Whether synthetic agent fetch probes run after the site's monitored crawls (on unless the
/// owner switched them off). A site that is gone reads as the default.
pub async fn get_ai_probes(pool: &PgPool, site_id: Uuid) -> Result<bool, sqlx::Error> {
    let on: Option<bool> = sqlx::query_scalar("SELECT ai_probes FROM sites WHERE id = $1")
        .bind(site_id)
        .fetch_optional(pool)
        .await?;
    Ok(on.unwrap_or(true))
}

/// Switches the site's synthetic agent fetch probes on or off. Stored reports and incidents are
/// left alone: a crawl without probes does not judge `fetch_blocked`, so an open one stays open
/// until a crawl probes again or the owner's intent makes it moot.
pub async fn set_ai_probes(pool: &PgPool, site_id: Uuid, on: bool) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE sites SET ai_probes = $2 WHERE id = $1")
        .bind(site_id)
        .bind(on)
        .execute(pool)
        .await?;
    Ok(())
}

/// The newest report of the site (any crawl status), with its crawl's id and status. A report
/// from a failed crawl only knows about robots.txt, so [`evaluated_kinds`] on it protects the
/// incidents it could not judge.
pub async fn latest_report(
    pool: &PgPool,
    site_id: Uuid,
) -> Result<Option<LatestReport>, sqlx::Error> {
    latest_report_in(pool, site_id).await
}

async fn latest_report_in<'e, E>(
    executor: E,
    site_id: Uuid,
) -> Result<Option<LatestReport>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    let row: Option<(Uuid, String, OffsetDateTime, Value)> = sqlx::query_as(
        "SELECT r.crawl_id, c.status::text, r.created_at, r.report \
         FROM ai_reports r JOIN crawls c ON c.id = r.crawl_id \
         WHERE r.site_id = $1 ORDER BY r.created_at DESC, r.crawl_id DESC LIMIT 1",
    )
    .bind(site_id)
    .fetch_optional(executor)
    .await?;
    Ok(
        row.and_then(|(crawl_id, crawl_status, created_at, report)| {
            Some(LatestReport {
                crawl_id,
                crawl_status,
                created_at,
                report: serde_json::from_value(report).ok()?,
            })
        }),
    )
}

/// The `llms_txt` part of the site's newest stored report that has one. A crawl that did not
/// fetch the file (or a failed one) stores a report without it, so this is not simply the latest
/// report's: it is what adoption is carried from ([`codoseo_geo::llms::carry_adoption`]).
pub async fn latest_llms_txt(
    pool: &PgPool,
    site_id: Uuid,
) -> Result<Option<LlmsTxtReport>, sqlx::Error> {
    let value: Option<Value> = sqlx::query_scalar(
        "SELECT report->'llms_txt' FROM ai_reports \
         WHERE site_id = $1 AND jsonb_typeof(report->'llms_txt') = 'object' \
         ORDER BY created_at DESC, crawl_id DESC LIMIT 1",
    )
    .bind(site_id)
    .fetch_optional(pool)
    .await?;
    Ok(value.and_then(|v| serde_json::from_value(v).ok()))
}

/// Whether the site has any stored report (without reading it).
pub async fn has_report(pool: &PgPool, site_id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ai_reports WHERE site_id = $1)")
        .bind(site_id)
        .fetch_one(pool)
        .await
}

/// When each of the site's stored reports was written, by crawl. An incident opened in the same
/// transaction as its crawl's report (the same instant) was opened by that crawl; a quiet one
/// opened later came from an intent change.
pub async fn report_times(
    pool: &PgPool,
    site_id: Uuid,
) -> Result<HashMap<Uuid, OffsetDateTime>, sqlx::Error> {
    let rows: Vec<(Uuid, OffsetDateTime)> =
        sqlx::query_as("SELECT crawl_id, created_at FROM ai_reports WHERE site_id = $1")
            .bind(site_id)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().collect())
}

/// The site's unresolved incidents, most severe first.
pub async fn open_incidents(pool: &PgPool, site_id: Uuid) -> Result<Vec<Incident>, sqlx::Error> {
    let rows: Vec<IncidentRow> = sqlx::query_as(&format!(
        "SELECT {INCIDENT_COLUMNS} FROM ai_incidents \
         WHERE site_id = $1 AND resolved_at IS NULL ORDER BY severity, opened_at, id"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(IncidentRow::into_incident)
        .collect())
}

/// The latest `limit` resolved incidents of the site, newest first.
pub async fn recent_resolved(
    pool: &PgPool,
    site_id: Uuid,
    limit: i64,
) -> Result<Vec<Incident>, sqlx::Error> {
    let rows: Vec<IncidentRow> = sqlx::query_as(&format!(
        "SELECT {INCIDENT_COLUMNS} FROM ai_incidents \
         WHERE site_id = $1 AND resolved_at IS NOT NULL ORDER BY resolved_at DESC, id DESC LIMIT $2"
    ))
    .bind(site_id)
    .bind(limit.max(0))
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(IncidentRow::into_incident)
        .collect())
}

/// One incident of the site, open or resolved. `None` for another site's id.
pub async fn incident(
    pool: &PgPool,
    site_id: Uuid,
    id: i64,
) -> Result<Option<Incident>, sqlx::Error> {
    let row: Option<IncidentRow> = sqlx::query_as(&format!(
        "SELECT {INCIDENT_COLUMNS} FROM ai_incidents WHERE site_id = $1 AND id = $2"
    ))
    .bind(site_id)
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(IncidentRow::into_incident))
}

/// How many incidents are open on the site, and how many of those are critical (the sidebar).
/// Accepted incidents and those snoozed until later are left out: the owner already decided
/// about them, so they don't ask for attention.
pub async fn open_counts(pool: &PgPool, site_id: Uuid) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE severity = 'critical') \
         FROM ai_incidents WHERE site_id = $1 AND resolved_at IS NULL \
           AND accepted_at IS NULL AND (snoozed_until IS NULL OR snoozed_until <= now())",
    )
    .bind(site_id)
    .fetch_one(pool)
    .await
}

/// The longest an incident can be snoozed, in days.
pub const MAX_SNOOZE_DAYS: i64 = 90;

/// Why accepting or snoozing an incident did not happen.
#[derive(Debug, thiserror::Error)]
pub enum IncidentActionError {
    /// No incident with that id on that site.
    #[error("no such incident")]
    NotFound,
    /// It is resolved: there is nothing left to accept or snooze.
    #[error("the incident is resolved")]
    Resolved,
    /// Its kind can't be accepted ([`codoseo_geo::incident::acceptable`]): the bot and directive
    /// kinds go through the owner's intent, a failing robots.txt can only be snoozed.
    #[error("a {} incident can't be accepted", .0.slug())]
    NotAcceptable(FindingKind),
    /// A snooze has to end after now.
    #[error("a snooze must end in the future")]
    SnoozeInPast,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Locks one open incident of the site for an owner's action and returns its kind.
async fn lock_incident(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    id: i64,
) -> Result<FindingKind, IncidentActionError> {
    let row: Option<(String, bool)> = sqlx::query_as(
        "SELECT kind, resolved_at IS NOT NULL FROM ai_incidents \
         WHERE site_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(site_id)
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        None => Err(IncidentActionError::NotFound),
        Some((_, true)) => Err(IncidentActionError::Resolved),
        Some((kind, false)) => FindingKind::from_slug(&kind).ok_or(IncidentActionError::NotFound),
    }
}

/// The owner accepts the incident as it is: it stays open and listed (under "Accepted"), but no
/// longer counts in the badges and writes no change rows, so nothing about it alerts or reaches
/// the digest; a crawl that no longer finds it still resolves it, quietly. Only for kinds
/// without an intent path ([`codoseo_geo::incident::acceptable`]). Accepting again keeps the
/// first time and account.
pub async fn accept(
    pool: &PgPool,
    site_id: Uuid,
    id: i64,
    account_id: Uuid,
) -> Result<(), IncidentActionError> {
    let mut tx = pool.begin().await?;
    let kind = lock_incident(&mut tx, site_id, id).await?;
    if !codoseo_geo::incident::acceptable(kind) {
        return Err(IncidentActionError::NotAcceptable(kind));
    }
    sqlx::query(
        "UPDATE ai_incidents SET accepted_at = now(), accepted_by = $2 \
         WHERE id = $1 AND accepted_at IS NULL",
    )
    .bind(id)
    .bind(account_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Takes back an acceptance: the incident counts and announces its changes again.
pub async fn unaccept(pool: &PgPool, site_id: Uuid, id: i64) -> Result<(), IncidentActionError> {
    let mut tx = pool.begin().await?;
    lock_incident(&mut tx, site_id, id).await?;
    sqlx::query("UPDATE ai_incidents SET accepted_at = NULL, accepted_by = NULL WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Snoozes the incident until `until`, at most [`MAX_SNOOZE_DAYS`] from now (a later time is cut
/// to that). While snoozed it does not count in the badges, and the change rows it writes are
/// still recorded (the changes page and the digest stay truthful) but marked alerted, so no
/// instant alert goes out. The snooze ends on its own. Any kind can be snoozed. Returns the time
/// stored.
pub async fn snooze(
    pool: &PgPool,
    site_id: Uuid,
    id: i64,
    until: OffsetDateTime,
) -> Result<OffsetDateTime, IncidentActionError> {
    let mut tx = pool.begin().await?;
    lock_incident(&mut tx, site_id, id).await?;
    let stored: Option<OffsetDateTime> = sqlx::query_scalar(
        "UPDATE ai_incidents \
         SET snoozed_until = LEAST($2, now() + make_interval(days => $3::int)) \
         WHERE id = $1 AND $2 > now() RETURNING snoozed_until",
    )
    .bind(id)
    .bind(until)
    .bind(MAX_SNOOZE_DAYS as i32)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(stored) = stored else {
        return Err(IncidentActionError::SnoozeInPast);
    };
    tx.commit().await?;
    Ok(stored)
}

/// Ends a snooze early.
pub async fn unsnooze(pool: &PgPool, site_id: Uuid, id: i64) -> Result<(), IncidentActionError> {
    let mut tx = pool.begin().await?;
    lock_incident(&mut tx, site_id, id).await?;
    sqlx::query("UPDATE ai_incidents SET snoozed_until = NULL WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// How long after a manual recheck of an incident the next one can be asked for, in minutes.
pub const RECHECK_COOLDOWN_MINUTES: i32 = 10;
/// The window the daily recheck allowance counts over, in hours: a trailing 24 hours rather
/// than a calendar day, so there is no midnight (in whose time zone?) to wait for and the time a
/// slot frees up is simply a day after the recheck that used it.
pub const RECHECK_WINDOW_HOURS: i32 = 24;

/// A recheck that was queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecheckQueued {
    pub id: i64,
    pub due_at: OffsetDateTime,
}

/// Why a recheck was not queued.
#[derive(Debug, thiserror::Error)]
pub enum RecheckError {
    #[error("no such incident")]
    NotFound,
    #[error("the incident is resolved")]
    Resolved,
    /// A recheck of the incident (manual or follow-up) is already queued or running.
    #[error("a recheck of this incident is already queued")]
    Busy,
    /// The incident was rechecked by hand less than [`RECHECK_COOLDOWN_MINUTES`] ago; another
    /// can be asked for from this time.
    #[error("this incident was rechecked moments ago")]
    Cooldown(OffsetDateTime),
    /// The account used its manual rechecks for the last 24 hours; one frees up at this time
    /// (`None` when the plan allows none).
    #[error("the daily recheck allowance is used up")]
    LimitReached(Option<OffsetDateTime>),
    /// A crawl of the site is queued or running: it will judge the incident soon anyway.
    #[error("a crawl of this site is under way")]
    CrawlActive,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Queues a manual recheck of an open incident, with its `ai_recheck` job (one attempt, due
/// now), in one transaction. Refused while one is already queued or running for the incident,
/// within [`RECHECK_COOLDOWN_MINUTES`] of the last manual one, while a crawl of the site is
/// queued or running, and past the plan's `ai_rechecks_per_day`: manual rechecks over the last
/// [`RECHECK_WINDOW_HOURS`] across every site of the site's account. Follow-ups don't count,
/// nor do manual ones that ended `superseded` (a crawl took over) or `failed` (they never ran
/// on our side): the owner got no answer from those. They still start the cooldown.
/// Requests for one account are serialised, so two at once can't both take the last slot.
pub async fn request_recheck(
    pool: &PgPool,
    site_id: Uuid,
    incident_id: i64,
    account_id: Uuid,
    limits: &PlanLimits,
) -> Result<RecheckQueued, RecheckError> {
    let mut tx = pool.begin().await?;
    let owner: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT account_id FROM sites WHERE id = $1")
            .bind(site_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(owner) = owner else {
        return Err(RecheckError::NotFound);
    };
    let key = format!("codoseo.ai_recheck.{}", owner.unwrap_or(site_id));
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(key)
        .execute(&mut *tx)
        .await?;
    match lock_incident(&mut tx, site_id, incident_id).await {
        Ok(_) => {}
        Err(IncidentActionError::Resolved) => return Err(RecheckError::Resolved),
        Err(IncidentActionError::Db(e)) => return Err(RecheckError::Db(e)),
        Err(_) => return Err(RecheckError::NotFound),
    }
    let busy: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM ai_rechecks \
         WHERE incident_id = $1 AND status IN ('queued', 'running'))",
    )
    .bind(incident_id)
    .fetch_one(&mut *tx)
    .await?;
    if busy {
        return Err(RecheckError::Busy);
    }
    let cooling: Option<OffsetDateTime> = sqlx::query_scalar(
        "SELECT until FROM ( \
           SELECT max(created_at) + make_interval(mins => $2) AS until FROM ai_rechecks \
           WHERE incident_id = $1 AND origin = 'manual') t \
         WHERE until > now()",
    )
    .bind(incident_id)
    .bind(RECHECK_COOLDOWN_MINUTES)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(until) = cooling {
        return Err(RecheckError::Cooldown(until));
    }
    let crawling: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM crawls \
         WHERE site_id = $1 AND status IN ('queued', 'running'))",
    )
    .bind(site_id)
    .fetch_one(&mut *tx)
    .await?;
    if crawling {
        return Err(RecheckError::CrawlActive);
    }
    if let Some(cap) = limits.ai_rechecks_per_day {
        let cap = usize::from(cap);
        let used: Vec<OffsetDateTime> = sqlx::query_scalar(
            "SELECT r.created_at FROM ai_rechecks r JOIN sites s ON s.id = r.site_id \
             WHERE r.origin = 'manual' AND r.status NOT IN ('superseded', 'failed') \
               AND r.created_at > now() - make_interval(hours => $3) \
               AND (r.site_id = $1 OR ($2::uuid IS NOT NULL AND s.account_id = $2)) \
             ORDER BY r.created_at",
        )
        .bind(site_id)
        .bind(owner)
        .bind(RECHECK_WINDOW_HOURS)
        .fetch_all(&mut *tx)
        .await?;
        if used.len() >= cap {
            // The slot frees when enough of the oldest leave the window to drop below the cap.
            let frees_at = used
                .get(used.len() - cap)
                .map(|t| *t + time::Duration::hours(i64::from(RECHECK_WINDOW_HOURS)));
            return Err(RecheckError::LimitReached(frees_at));
        }
    }
    let (id, due_at): (i64, OffsetDateTime) = sqlx::query_as(
        "INSERT INTO ai_rechecks (site_id, incident_id, origin, requested_by, due_at) \
         VALUES ($1, $2, 'manual', $3, now()) RETURNING id, due_at",
    )
    .bind(site_id)
    .bind(incident_id)
    .bind(account_id)
    .fetch_one(&mut *tx)
    .await?;
    enqueue_at_in(
        &mut *tx,
        JobKind::AiRecheck,
        serde_json::json!({ "recheck_id": id }),
        due_at,
        1,
    )
    .await?;
    tx.commit().await?;
    Ok(RecheckQueued { id, due_at })
}

const RECHECK_COLUMNS: &str = "id, site_id, incident_id, origin, requested_by, status, due_at, \
     started_at, finished_at, outcome, observations, created_at";

/// The newest recheck of each of the site's open incidents, by incident id: what the screen
/// says about a recheck under way or the last one's outcome.
pub async fn latest_rechecks(
    pool: &PgPool,
    site_id: Uuid,
) -> Result<HashMap<i64, Recheck>, sqlx::Error> {
    let rows: Vec<RecheckRow> = sqlx::query_as(&format!(
        "SELECT DISTINCT ON (r.incident_id) {} FROM ai_rechecks r \
         JOIN ai_incidents i ON i.id = r.incident_id \
         WHERE r.site_id = $1 AND i.resolved_at IS NULL \
         ORDER BY r.incident_id, r.id DESC",
        RECHECK_COLUMNS
            .split(", ")
            .map(|c| format!("r.{c}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(RecheckRow::into_recheck)
        .map(|r| (r.incident_id, r))
        .collect())
}

/// Marks a queued recheck running (the runner, when its job is claimed) and returns it; `None`
/// when it is not queued (superseded, or already run).
pub async fn start_recheck(pool: &PgPool, id: i64) -> Result<Option<Recheck>, sqlx::Error> {
    let row: Option<RecheckRow> = sqlx::query_as(&format!(
        "UPDATE ai_rechecks SET status = 'running', started_at = now() \
         WHERE id = $1 AND status = 'queued' RETURNING {RECHECK_COLUMNS}"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(RecheckRow::into_recheck))
}

/// Marks a queued or running recheck failed (it could not run at all). Returns whether it did.
pub async fn fail_recheck(pool: &PgPool, id: i64) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE ai_rechecks SET status = 'failed', finished_at = now(), \
           started_at = COALESCE(started_at, now()) \
         WHERE id = $1 AND status IN ('queued', 'running')",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// How long after an inconclusive follow-up recheck the next one is due, in hours.
pub const FOLLOWUP_RETRY_HOURS: i32 = 6;
/// Inconclusive follow-ups in a row (since the manual recheck that queued the first) after which
/// no other is queued: the incident stays recovered and the next crawl judges it.
pub const MAX_INCONCLUSIVE_FOLLOWUPS: i64 = 3;

/// After an inconclusive follow-up: queues the next one [`FOLLOWUP_RETRY_HOURS`] on, with its
/// job, unless [`MAX_INCONCLUSIVE_FOLLOWUPS`] were inconclusive in a row. Returns its id.
async fn retry_followup(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    incident_id: i64,
) -> Result<Option<i64>, sqlx::Error> {
    let in_a_row: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ai_rechecks \
         WHERE incident_id = $1 AND origin = 'followup' AND outcome = 'inconclusive' \
           AND id > COALESCE((SELECT max(id) FROM ai_rechecks \
                              WHERE incident_id = $1 AND origin = 'manual'), 0)",
    )
    .bind(incident_id)
    .fetch_one(&mut **tx)
    .await?;
    if in_a_row >= MAX_INCONCLUSIVE_FOLLOWUPS {
        return Ok(None);
    }
    let (id, due_at): (i64, OffsetDateTime) = sqlx::query_as(
        "INSERT INTO ai_rechecks (site_id, incident_id, origin, due_at) \
         VALUES ($1, $2, 'followup', now() + make_interval(hours => $3)) \
         RETURNING id, due_at",
    )
    .bind(site_id)
    .bind(incident_id)
    .bind(FOLLOWUP_RETRY_HOURS)
    .fetch_one(&mut **tx)
    .await?;
    enqueue_at_in(
        &mut **tx,
        JobKind::AiRecheck,
        serde_json::json!({ "recheck_id": id }),
        due_at,
        1,
    )
    .await?;
    Ok(Some(id))
}

/// Marks a queued or running recheck superseded (a crawl of the site is under way and will
/// judge the incident). Returns whether it did.
pub async fn supersede_recheck(pool: &PgPool, id: i64) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE ai_rechecks SET status = 'superseded', finished_at = now(), \
           started_at = COALESCE(started_at, now()) \
         WHERE id = $1 AND status IN ('queued', 'running')",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Fails the rechecks nothing will finish, so an incident is never left busy: those `running`
/// for longer than `running_for` (the runner's own time limit is far shorter, so its worker
/// died), and those queued or running whose `ai_recheck` job is no longer queued or running
/// (it failed, or was removed). Returns how many.
pub async fn fail_orphaned_rechecks(
    pool: &PgPool,
    running_for: std::time::Duration,
) -> Result<u64, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE ai_rechecks r SET status = 'failed', finished_at = now(), \
           started_at = COALESCE(r.started_at, now()) \
         WHERE (r.status = 'running' AND r.started_at < now() - make_interval(secs => $1)) \
            OR (r.status IN ('queued', 'running') AND NOT EXISTS ( \
                  SELECT 1 FROM jobs j WHERE j.kind = 'ai_recheck' \
                    AND j.status IN ('queued', 'running') \
                    AND j.payload->>'recheck_id' = r.id::text))",
    )
    .bind(running_for.as_secs_f64())
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

/// Applies what a recheck saw to its incident: `report` is the stored report with the
/// recheck's observations laid over it and `kinds` the kinds they were conclusive for
/// ([`codoseo_geo::recheck::overlay`]). Locks like [`apply_geo`] (the site row, then the open
/// incidents) and judges the incident as a crawl would, with the bots and pages the report could
/// not judge held back, but only that incident: a recheck never opens one, and never touches
/// another.
///
/// - Not conclusive for its kind: `inconclusive`, nothing changes; a follow-up queues the next
///   one [`FOLLOWUP_RETRY_HOURS`] on, at most [`MAX_INCONCLUSIVE_FOLLOWUPS`] in a row.
/// - Still there: the incident's evidence and `last_seen_at` are refreshed (its
///   `last_seen_crawl_id` stays: no crawl saw it), a recovery is undone: `still_failing`.
/// - Gone, on a manual recheck: `recovered_at` is set and a follow-up recheck is queued for when
///   the operators will have seen the fix ([`codoseo_geo::ledger::lag`]): `clean`.
/// - Gone again, on that follow-up: resolved (`fixed`, verified now, by `recheck`) and, unless it
///   was quiet or accepted, announced like a crawl's resolution, on the crawl that last saw it
///   (already alerted while snoozed): `clean`.
///
/// A follow-up for an incident that is resolved, or whose recovery a crawl undid meanwhile, does
/// nothing and is marked `superseded`, as is a recheck for an incident resolved meanwhile.
/// Returns `None` then, and for a recheck that already finished. `observations` (what the
/// recheck saw, as JSON) is kept on the recheck.
pub async fn apply_recheck(
    pool: &PgPool,
    recheck_id: i64,
    report: &AccessReport,
    kinds: &[FindingKind],
    observations: Option<&Value>,
) -> Result<Option<RecheckOutcome>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let row: Option<(Uuid, i64, String)> =
        sqlx::query_as("SELECT site_id, incident_id, origin FROM ai_rechecks WHERE id = $1")
            .bind(recheck_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((site_id, incident_id, origin)) = row else {
        return Err(sqlx::Error::RowNotFound);
    };
    let origin = RecheckOrigin::from_slug(&origin).unwrap_or(RecheckOrigin::Manual);
    let intent = lock_site(&mut tx, site_id).await?;
    let open = lock_open(&mut tx, site_id).await?;
    let status: String =
        sqlx::query_scalar("SELECT status FROM ai_rechecks WHERE id = $1 FOR UPDATE")
            .bind(recheck_id)
            .fetch_one(&mut *tx)
            .await?;
    if !matches!(status.as_str(), "queued" | "running") {
        return Ok(None);
    }
    let mut observations = observations.cloned();
    if let Some(v) = observations.as_mut() {
        strip_nul(v);
    }
    let finish = |status: RecheckStatus, outcome: Option<RecheckOutcome>| {
        sqlx::query(
            "UPDATE ai_rechecks SET status = $2, outcome = $3, finished_at = now(), \
               started_at = COALESCE(started_at, now()), \
               observations = COALESCE($4, observations) \
             WHERE id = $1",
        )
        .bind(recheck_id)
        .bind(status.slug())
        .bind(outcome.map(RecheckOutcome::slug))
        .bind(observations.clone())
    };

    let incident = open.iter().find(|r| r.id == incident_id);
    let recovery: Option<(Option<OffsetDateTime>, Option<Uuid>)> = match incident {
        Some(_) => Some(
            sqlx::query_as(
                "SELECT recovered_at, last_seen_crawl_id FROM ai_incidents WHERE id = $1",
            )
            .bind(incident_id)
            .fetch_one(&mut *tx)
            .await?,
        ),
        None => None,
    };
    let (Some(row), Some((recovered_at, last_seen_crawl))) = (incident, recovery) else {
        finish(RecheckStatus::Superseded, None)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(None);
    };
    if origin == RecheckOrigin::Followup && recovered_at.is_none() {
        finish(RecheckStatus::Superseded, None)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(None);
    }
    if !kinds.contains(&row.kind) || !evaluated_kinds(report).contains(&row.kind) {
        finish(RecheckStatus::Done, Some(RecheckOutcome::Inconclusive))
            .execute(&mut *tx)
            .await?;
        if origin == RecheckOrigin::Followup {
            retry_followup(&mut tx, site_id, row.id).await?;
        }
        tx.commit().await?;
        return Ok(Some(RecheckOutcome::Inconclusive));
    }

    let found = hold_unjudged(report, &intent, &open)
        .into_iter()
        .find(|f| f.kind == row.kind && f.subject == row.subject);
    let outcome = if let Some(finding) = found {
        let mut evidence =
            serde_json::to_value(&finding.evidence).expect("evidence always serializes");
        strip_nul(&mut evidence);
        sqlx::query(
            "UPDATE ai_incidents SET severity = $2::severity, title = $3, summary = $4, \
               evidence = $5, last_seen_at = now(), recovered_at = NULL \
             WHERE id = $1",
        )
        .bind(row.id)
        .bind(enum_slug(&finding.severity))
        .bind(without_nul(&finding.title))
        .bind(without_nul(&finding.summary))
        .bind(evidence)
        .execute(&mut *tx)
        .await?;
        finish(RecheckStatus::Done, Some(RecheckOutcome::StillFailing))
            .execute(&mut *tx)
            .await?;
        RecheckOutcome::StillFailing
    } else if origin == RecheckOrigin::Manual {
        sqlx::query(
            "UPDATE ai_incidents SET recovered_at = COALESCE(recovered_at, now()) WHERE id = $1",
        )
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
        // Done first: one recheck at a time per incident.
        finish(RecheckStatus::Done, Some(RecheckOutcome::Clean))
            .execute(&mut *tx)
            .await?;
        let lag = codoseo_geo::ledger::lag(row.kind, &row.evidence);
        let (id, due_at): (i64, OffsetDateTime) = sqlx::query_as(
            "INSERT INTO ai_rechecks (site_id, incident_id, origin, due_at) \
             VALUES ($1, $2, 'followup', now() + make_interval(hours => $3)) \
             RETURNING id, due_at",
        )
        .bind(site_id)
        .bind(row.id)
        .bind(i32::try_from(lag.hours).unwrap_or(i32::MAX))
        .fetch_one(&mut *tx)
        .await?;
        enqueue_at_in(
            &mut *tx,
            JobKind::AiRecheck,
            serde_json::json!({ "recheck_id": id }),
            due_at,
            1,
        )
        .await?;
        RecheckOutcome::Clean
    } else {
        sqlx::query(
            "UPDATE ai_incidents SET resolved_at = now(), resolution = 'fixed', \
               verified_at = now(), resolved_by = 'recheck' WHERE id = $1",
        )
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
        // Announced like a crawl's resolution, on the crawl that last saw it (a recheck is no
        // crawl); nothing when that crawl is gone.
        let crawl = match last_seen_crawl {
            Some(c) if !row.quiet => {
                let exists: bool =
                    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM crawls WHERE id = $1)")
                        .bind(c)
                        .fetch_one(&mut *tx)
                        .await?;
                exists.then_some(c)
            }
            _ => None,
        };
        if let Some(crawl) = crawl {
            let mut written = Written::default();
            written.push(Some(row), resolved_change(row.kind, &row.title));
            insert_changes(&mut tx, crawl, site_id, &written.changes).await?;
            insert_muted_changes(&mut tx, crawl, site_id, &written.muted).await?;
            enqueue_alert_job(&mut tx, crawl, !written.changes.is_empty()).await?;
        }
        finish(RecheckStatus::Done, Some(RecheckOutcome::Clean))
            .execute(&mut *tx)
            .await?;
        RecheckOutcome::Clean
    };
    tx.commit().await?;
    Ok(Some(outcome))
}

/// After an intent change: the findings of the site's stored reports under the new intent,
/// reconciled with the open incidents. Each kind of finding is judged by the newest report that
/// could judge it (a failed crawl's report knows robots.txt failed, not which bots it blocks), so
/// "Mark intended" works after a failed crawl too. Newly wanted findings open `quiet`, findings
/// the intent made moot resolve with `resolution = 'intent'`, and no change rows or alerts are
/// written: the owner just made the choice. Incidents that stay open keep their `last_seen_*`: no
/// crawl saw them again. Returns how many were (opened, resolved).
///
/// What it opens keeps no `before`: the problem did not appear between two reports, it was there
/// all along under the old intent, so the earlier report would only show the same state and a
/// repair has nothing to put back.
pub async fn reevaluate_quietly(
    pool: &PgPool,
    site_id: Uuid,
) -> Result<(usize, usize), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let intent = lock_site(&mut tx, site_id).await?;
    let reports = stored_reports(&mut tx, site_id, None).await?;
    let open = lock_open(&mut tx, site_id).await?;
    let (mut opened, mut resolved) = (0, 0);
    let mut judged: HashSet<FindingKind> = HashSet::new();
    for (crawl_id, report) in &reports {
        let Some(report) = report else { continue };
        let kinds: Vec<FindingKind> = evaluated_kinds(report)
            .into_iter()
            .filter(|k| judged.insert(*k))
            .collect();
        if kinds.is_empty() {
            continue;
        }
        let found: Vec<Finding> = hold_unjudged(report, &intent, &open)
            .into_iter()
            .filter(|f| kinds.contains(&f.kind))
            .collect();
        let transitions = reconcile(&as_open(&open), &found, &kinds);
        opened += transitions
            .iter()
            .filter(|t| matches!(t, Transition::Opened { .. }))
            .count();
        resolved += transitions
            .iter()
            .filter(|t| matches!(t, Transition::Resolved { .. }))
            .count();
        write_transitions(
            &mut tx,
            site_id,
            *crawl_id,
            &transitions,
            &open,
            Mode {
                quiet: ALL_KINDS.into(),
                changes: false,
                resolution: "intent",
                resolved_by: "intent",
                seen: false,
                befores: HashMap::new(),
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok((opened, resolved))
}

/// A crawl that failed for good (its site gave no pages) but did read a failing robots.txt:
/// records its report and reconciles in a transaction of its own. Only `RobotsUnavailable` can be
/// judged from such a report, so only that incident opens, updates or resolves. Writes the change
/// row and queues the `send_alert` job like a finished crawl would. Returns the number of changes.
///
/// Guarded like `finalize`: the crawl must still be `running` under `worker_id`, or nothing is
/// written and the error is `RowNotFound`, so a worker whose crawl was requeued and claimed by
/// another can't record for it.
pub async fn record_failed_crawl(
    pool: &PgPool,
    site_id: Uuid,
    crawl_id: Uuid,
    worker_id: &str,
    input: &GeoInput,
) -> Result<usize, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let owned: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM crawls WHERE id = $1 AND status = 'running' AND worker_id = $2 FOR UPDATE",
    )
    .bind(crawl_id)
    .bind(worker_id)
    .fetch_optional(&mut *tx)
    .await?;
    if owned.is_none() {
        return Err(sqlx::Error::RowNotFound);
    }
    let changes = apply_geo(&mut tx, site_id, crawl_id, input).await?;
    insert_changes(&mut tx, crawl_id, site_id, &changes).await?;
    enqueue_alert_job(&mut tx, crawl_id, !changes.is_empty()).await?;
    tx.commit().await?;
    Ok(changes.len())
}

/// Stores the crawl's report, trims the site's reports ([`reports_to_keep`]), and opens, updates
/// and resolves the site's incidents from the report's findings. Returns the changes to record
/// (the caller inserts them with the crawl's other changes).
///
/// The site row is locked first and the intent read under that lock, so the findings follow the
/// intent stored now, not the one the worker saw when the crawl started: an intent change that
/// lands meanwhile either commits first (and is read here) or waits for this transaction (and
/// re-evaluates what it wrote). The lock also keeps two writers from opening the same incident
/// twice. An incident opens `quiet`, without a change row, when no earlier stored report judged
/// its kind (the kind's baseline); otherwise it keeps, as `before`, what the newest earlier
/// report that judged its kind held about it ([`before_of`]). A report written again for the
/// same crawl replaces the earlier one, and "earlier reports" are always other crawls'.
pub(crate) async fn apply_geo(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    crawl_id: Uuid,
    input: &GeoInput,
) -> Result<Vec<Change>, sqlx::Error> {
    let intent = lock_site(tx, site_id).await?;
    let earlier = stored_reports(tx, site_id, Some(crawl_id)).await?;
    let judged_before: HashSet<FindingKind> = earlier
        .iter()
        .filter_map(|(_, r)| r.as_ref())
        .flat_map(evaluated_kinds)
        .collect();

    let mut report = serde_json::to_value(&input.report).expect("a report always serializes");
    strip_nul(&mut report);
    sqlx::query(
        "INSERT INTO ai_reports (crawl_id, site_id, report) VALUES ($1, $2, $3) \
         ON CONFLICT (crawl_id) DO UPDATE SET report = EXCLUDED.report, created_at = now()",
    )
    .bind(crawl_id)
    .bind(site_id)
    .bind(report)
    .execute(&mut **tx)
    .await?;
    let all: Vec<(Uuid, Option<&AccessReport>)> = std::iter::once((crawl_id, Some(&input.report)))
        .chain(earlier.iter().map(|(id, r)| (*id, r.as_ref())))
        .collect();
    let keep: Vec<Uuid> = reports_to_keep(&all).into_iter().collect();
    sqlx::query("DELETE FROM ai_reports WHERE site_id = $1 AND NOT (crawl_id = ANY($2))")
        .bind(site_id)
        .bind(&keep)
        .execute(&mut **tx)
        .await?;

    let evaluated = evaluated_kinds(&input.report);
    let open = lock_open(tx, site_id).await?;
    let found = hold_unjudged(&input.report, &intent, &open);
    let transitions = reconcile(&as_open(&open), &found, &evaluated);
    // What the newest earlier report that judged each kind held about what just opened.
    let befores: HashMap<(FindingKind, String), Value> = transitions
        .iter()
        .filter_map(|t| match t {
            Transition::Opened { finding } => Some(finding),
            _ => None,
        })
        .filter_map(|f| {
            let prev = earlier
                .iter()
                .filter_map(|(_, r)| r.as_ref())
                .find(|r| evaluated_kinds(r).contains(&f.kind))?;
            let before = before_of(f.kind, &f.subject, &f.evidence, prev)?;
            let mut value = serde_json::to_value(&before).expect("before always serializes");
            strip_nul(&mut value);
            Some(((f.kind, f.subject.clone()), value))
        })
        .collect();
    let written = write_transitions(
        tx,
        site_id,
        crawl_id,
        &transitions,
        &open,
        Mode {
            quiet: ALL_KINDS
                .into_iter()
                .filter(|k| !judged_before.contains(k))
                .collect(),
            changes: true,
            resolution: "fixed",
            resolved_by: "crawl",
            seen: true,
            befores,
        },
    )
    .await?;
    insert_muted_changes(tx, crawl_id, site_id, &written.muted).await?;
    let mut changes = written.changes;

    // Preferences are compared with the newest earlier report that knew them all.
    let previous = earlier
        .iter()
        .filter_map(|(_, r)| r.as_ref())
        .find(|r| r.preferences_known());
    if let Some(previous) = previous
        && input.report.preferences_known()
        && let Some(change) = preferences_change(previous, &input.report)
    {
        changes.push(change);
    }
    Ok(changes)
}

/// Locks the site row and reads its intent. `FOR NO KEY UPDATE` conflicts with an intent save's
/// `UPDATE` and with the other AI access writers, but not with rows that merely reference the
/// site, so crawls can still be queued meanwhile.
async fn lock_site(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
) -> Result<Intent, sqlx::Error> {
    let value: Option<Value> =
        sqlx::query_scalar("SELECT ai_intent FROM sites WHERE id = $1 FOR NO KEY UPDATE")
            .bind(site_id)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(value
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default())
}

/// The site's stored reports, newest first, except `except`'s; `None` for one that no longer
/// parses.
async fn stored_reports(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    except: Option<Uuid>,
) -> Result<Vec<(Uuid, Option<AccessReport>)>, sqlx::Error> {
    let rows: Vec<(Uuid, Value)> = sqlx::query_as(
        "SELECT crawl_id, report FROM ai_reports \
         WHERE site_id = $1 AND crawl_id IS DISTINCT FROM $2 \
         ORDER BY created_at DESC, crawl_id DESC",
    )
    .bind(site_id)
    .bind(except)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, v)| (id, serde_json::from_value(v).ok()))
        .collect())
}

/// Which of a site's reports (newest first) to keep: the newest two, plus the newest that judged
/// each kind of finding and the newest that knew every declared preference. A run of failed
/// crawls then loses neither what the last good crawl saw (for [`reevaluate_quietly`] and the
/// preference comparison) nor each kind's baseline.
fn reports_to_keep(reports: &[(Uuid, Option<&AccessReport>)]) -> BTreeSet<Uuid> {
    let mut keep: BTreeSet<Uuid> = reports
        .iter()
        .take(KEEP_REPORTS)
        .map(|(id, _)| *id)
        .collect();
    let mut judged: HashSet<FindingKind> = HashSet::new();
    let mut preferences = false;
    for (id, report) in reports {
        let Some(report) = report else { continue };
        for kind in evaluated_kinds(report) {
            if judged.insert(kind) {
                keep.insert(*id);
            }
        }
        if !preferences && report.preferences_known() {
            preferences = true;
            keep.insert(*id);
        }
    }
    keep
}

/// How [`write_transitions`] treats what it writes.
struct Mode {
    /// Kinds whose incidents open `quiet`, without a change row.
    quiet: HashSet<FindingKind>,
    /// Whether to return change rows for the transitions.
    changes: bool,
    /// The `resolution` recorded on resolved incidents.
    resolution: &'static str,
    /// The `resolved_by` recorded on them: `crawl` or `intent`.
    resolved_by: &'static str,
    /// A crawl read the site: the incidents it keeps open were seen again by it. A quiet
    /// re-evaluation only re-reads the stored report, so their `last_seen_*` stay as they were.
    seen: bool,
    /// What was true before each incident that opens, by (kind, subject) ([`before_of`]).
    befores: HashMap<(FindingKind, String), Value>,
}

/// An open incident as read for reconciling.
struct OpenRow {
    id: i64,
    kind: FindingKind,
    subject: String,
    severity: Severity,
    title: String,
    quiet: bool,
    /// The owner accepted it: its changes are not written.
    accepted: bool,
    /// Snoozed until later: its changes are written already alerted.
    snoozed: bool,
    members: BTreeSet<String>,
    evidence: Evidence,
}

#[derive(sqlx::FromRow)]
struct OpenDbRow {
    id: i64,
    kind: String,
    subject: String,
    severity: String,
    title: String,
    quiet: bool,
    evidence: Value,
    accepted: bool,
    snoozed: bool,
}

impl OpenDbRow {
    fn into_open(self) -> Option<OpenRow> {
        let evidence = serde_json::from_value::<Evidence>(self.evidence).unwrap_or_default();
        Some(OpenRow {
            id: self.id,
            kind: FindingKind::from_slug(&self.kind)?,
            subject: self.subject,
            severity: parse_severity(&self.severity)?,
            title: self.title,
            quiet: self.quiet,
            accepted: self.accepted,
            snoozed: self.snoozed,
            members: evidence.members(),
            evidence,
        })
    }
}

async fn lock_open(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
) -> Result<Vec<OpenRow>, sqlx::Error> {
    let rows: Vec<OpenDbRow> = sqlx::query_as(
        "SELECT id, kind, subject, severity::text AS severity, title, quiet, evidence, \
           accepted_at IS NOT NULL AS accepted, \
           COALESCE(snoozed_until > now(), false) AS snoozed \
         FROM ai_incidents WHERE site_id = $1 AND resolved_at IS NULL ORDER BY id FOR UPDATE",
    )
    .bind(site_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().filter_map(OpenDbRow::into_open).collect())
}

/// The report's findings under `intent`, with the bots and pages it could not judge held back
/// from the `open` incidents ([`hold_unjudged_probes`], [`hold_unjudged_content`]).
fn hold_unjudged(report: &AccessReport, intent: &Intent, open: &[OpenRow]) -> Vec<Finding> {
    let content = open
        .iter()
        .find(|r| r.kind == FindingKind::ContentMissing)
        .map(|r| &r.evidence);
    let found = hold_unjudged_probes(report, intent, &held(open), findings(report, intent));
    hold_unjudged_content(report, intent, content, found)
}

/// The open `fetch_blocked` incidents' subjects and evidence, for [`hold_unjudged_probes`].
fn held(rows: &[OpenRow]) -> Vec<(String, Evidence)> {
    rows.iter()
        .filter(|r| r.kind == FindingKind::FetchBlocked)
        .map(|r| (r.subject.clone(), r.evidence.clone()))
        .collect()
}

fn as_open(rows: &[OpenRow]) -> Vec<OpenIncident> {
    rows.iter()
        .map(|r| OpenIncident {
            id: r.id,
            kind: r.kind,
            subject: r.subject.clone(),
            severity: r.severity,
            members: r.members.clone(),
        })
        .collect()
}

/// The change rows [`write_transitions`] produced: `changes` go through the alert planner,
/// `muted` (a snoozed incident's) are recorded already alerted.
#[derive(Default)]
struct Written {
    changes: Vec<Change>,
    muted: Vec<Change>,
}

impl Written {
    /// Files a change of the incident `row` (`None` for one that just opened): nothing for an
    /// accepted one, muted for one snoozed until later.
    fn push(&mut self, row: Option<&OpenRow>, change: Change) {
        match row {
            Some(r) if r.accepted => {}
            Some(r) if r.snoozed => self.muted.push(change),
            _ => self.changes.push(change),
        }
    }
}

async fn write_transitions(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    crawl_id: Uuid,
    transitions: &[Transition],
    open: &[OpenRow],
    mode: Mode,
) -> Result<Written, sqlx::Error> {
    let by_id: HashMap<i64, &OpenRow> = open.iter().map(|r| (r.id, r)).collect();
    let mut written = Written::default();
    for transition in transitions {
        match transition {
            Transition::Opened { finding } => {
                let quiet = mode.quiet.contains(&finding.kind);
                let mut evidence =
                    serde_json::to_value(&finding.evidence).expect("evidence always serializes");
                strip_nul(&mut evidence);
                // A concurrent writer may have opened the same problem since the lock was
                // taken (the lock only covers rows that existed); refresh that row instead.
                sqlx::query(
                    "INSERT INTO ai_incidents (site_id, kind, subject, severity, title, summary, \
                       evidence, opened_crawl_id, last_seen_crawl_id, quiet, before) \
                     VALUES ($1, $2, $3, $4::severity, $5, $6, $7, $8, $8, $9, $11) \
                     ON CONFLICT (site_id, kind, subject) WHERE resolved_at IS NULL DO UPDATE SET \
                       severity = EXCLUDED.severity, title = EXCLUDED.title, \
                       summary = EXCLUDED.summary, evidence = EXCLUDED.evidence, \
                       last_seen_crawl_id = CASE WHEN $10 THEN EXCLUDED.last_seen_crawl_id \
                         ELSE ai_incidents.last_seen_crawl_id END, \
                       last_seen_at = CASE WHEN $10 THEN now() ELSE ai_incidents.last_seen_at END, \
                       recovered_at = CASE WHEN $10 THEN NULL ELSE ai_incidents.recovered_at END",
                )
                .bind(site_id)
                .bind(finding.kind.slug())
                .bind(&finding.subject)
                .bind(enum_slug(&finding.severity))
                .bind(without_nul(&finding.title))
                .bind(without_nul(&finding.summary))
                .bind(evidence)
                .bind(crawl_id)
                .bind(quiet)
                .bind(mode.seen)
                .bind(
                    mode.befores
                        .get(&(finding.kind, finding.subject.clone()))
                        .cloned(),
                )
                .execute(&mut **tx)
                .await?;
                if mode.changes && !quiet {
                    written.push(None, finding_change(finding));
                }
            }
            Transition::Updated {
                id,
                finding,
                escalated,
                widened,
            } => {
                let row = by_id.get(id).copied();
                // Worse, or about more bots or engines than before: announced like a new
                // incident, and from then on no longer quiet, so its resolution is announced too.
                // An accepted incident announces nothing.
                let announce =
                    mode.changes && (*escalated || *widened) && !row.is_some_and(|r| r.accepted);
                let mut evidence =
                    serde_json::to_value(&finding.evidence).expect("evidence always serializes");
                strip_nul(&mut evidence);
                // A crawl that still finds it undoes a recovery a recheck saw.
                sqlx::query(
                    "UPDATE ai_incidents SET severity = $2::severity, title = $3, summary = $4, \
                       evidence = $5, \
                       last_seen_crawl_id = CASE WHEN $7 THEN $6 ELSE last_seen_crawl_id END, \
                       last_seen_at = CASE WHEN $7 THEN now() ELSE last_seen_at END, \
                       recovered_at = CASE WHEN $7 THEN NULL ELSE recovered_at END, \
                       quiet = quiet AND NOT $8 \
                     WHERE id = $1",
                )
                .bind(id)
                .bind(enum_slug(&finding.severity))
                .bind(without_nul(&finding.title))
                .bind(without_nul(&finding.summary))
                .bind(evidence)
                .bind(crawl_id)
                .bind(mode.seen)
                .bind(announce)
                .execute(&mut **tx)
                .await?;
                if announce {
                    written.push(row, finding_change(finding));
                }
            }
            Transition::Resolved { id, .. } => {
                sqlx::query(
                    "UPDATE ai_incidents SET resolved_at = now(), resolved_crawl_id = $2, \
                       resolution = $3, resolved_by = $4 WHERE id = $1",
                )
                .bind(id)
                .bind(crawl_id)
                .bind(mode.resolution)
                .bind(mode.resolved_by)
                .execute(&mut **tx)
                .await?;
                // Nobody was told a quiet incident opened, so nobody is told it is gone.
                if let Some(row) = by_id.get(id)
                    && mode.changes
                    && !row.quiet
                {
                    written.push(Some(row), resolved_change(row.kind, &row.title));
                }
            }
        }
    }
    Ok(written)
}

/// The change row announcing that the incident of `kind` titled `title` is gone.
fn resolved_change(kind: FindingKind, title: &str) -> Change {
    Change {
        kind: resolved_kind(kind),
        severity: Severity::Notice,
        url: None,
        before: truncate(title),
        after: "resolved".to_owned(),
    }
}

/// Records a snoozed incident's changes already marked alerted: on the changes page and in the
/// digest, but never routed to an instant channel.
async fn insert_muted_changes(
    tx: &mut Transaction<'_, Postgres>,
    crawl_id: Uuid,
    site_id: Uuid,
    changes: &[Change],
) -> Result<(), sqlx::Error> {
    for c in changes {
        sqlx::query(
            "INSERT INTO changes (crawl_id, site_id, kind, severity, url, before_value, \
               after_value, alerted_at) \
             VALUES ($1, $2, $3::change_kind, $4::severity, $5, $6, $7, now())",
        )
        .bind(crawl_id)
        .bind(site_id)
        .bind(enum_slug(&c.kind))
        .bind(enum_slug(&c.severity))
        .bind(c.url.as_ref().map(|u| u.as_str()))
        .bind(&c.before)
        .bind(&c.after)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// The change kind that announces an incident of `kind` is gone. Most use `ai_issue_resolved`,
/// which alerts at once by default. `llms_txt_unavailable` opens as a digest-only kind, so its
/// recovery is a change of that same kind (after: `resolved`): it reaches people by the same rule
/// the failure did, and nobody is alerted at once about something they were only told in a
/// digest.
fn resolved_kind(kind: FindingKind) -> ChangeKind {
    match kind {
        FindingKind::LlmsTxtUnavailable => ChangeKind::AiLlmsTxtUnavailable,
        _ => ChangeKind::AiIssueResolved,
    }
}

/// The change row for an incident that opened, got worse or widened. `before` is what was
/// expected, `after` the finding's title; the summary and evidence live on the incident.
fn finding_change(finding: &Finding) -> Change {
    let (kind, before) = match finding.kind {
        FindingKind::RobotsUnavailable | FindingKind::BotsBlocked => {
            (ChangeKind::AiBotBlocked, "allowed")
        }
        FindingKind::BotsNotBlocked => (ChangeKind::AiBlockNotApplied, "intent: block"),
        FindingKind::AnswersRestricted => (ChangeKind::AiAnswersRestricted, "eligible"),
        FindingKind::FetchBlocked => (ChangeKind::AiFetchBlocked, "reachable"),
        FindingKind::ContentMissing => (ChangeKind::AiContentMissing, "content in the HTML"),
        FindingKind::LlmsTxtUnavailable => (ChangeKind::AiLlmsTxtUnavailable, "llms.txt served"),
    };
    Change {
        kind,
        severity: finding.severity,
        url: None,
        before: before.to_owned(),
        after: truncate(&finding.title),
    }
}

/// `Some` when the declared AI preferences differ between two reports that both knew them
/// ([`AccessReport::preferences_known`]: a failing robots.txt or an error home page says nothing
/// about them). Lists only what was removed and what was added, not line numbers, so moving a
/// line is no change.
fn preferences_change(previous: &AccessReport, current: &AccessReport) -> Option<Change> {
    let before = declared_parts(&previous.declared);
    let after = declared_parts(&current.declared);
    if before == after {
        return None;
    }
    let list = |parts: Vec<&String>| {
        if parts.is_empty() {
            "none".to_owned()
        } else {
            truncate(&parts.into_iter().cloned().collect::<Vec<_>>().join("; "))
        }
    };
    Some(Change {
        kind: ChangeKind::AiPreferencesChanged,
        severity: Severity::Notice,
        url: None,
        before: list(before.difference(&after).collect()),
        after: list(after.difference(&before).collect()),
    })
}

/// The declared preferences as a set of short statements, without line numbers.
fn declared_parts(d: &Declared) -> BTreeSet<String> {
    fn pairs(p: &[Pair]) -> String {
        p.iter()
            .map(|p| format!("{}={}", p.key, p.value))
            .collect::<Vec<_>>()
            .join(", ")
    }
    fn agents(a: &[String], more: u32) -> String {
        match (a.is_empty(), more) {
            (true, _) => "*".to_owned(),
            (false, 0) => a.join(","),
            (false, n) => format!("{} and {n} more", a.join(",")),
        }
    }
    let mut parts = BTreeSet::new();
    for s in &d.content_signals {
        parts.insert(format!(
            "robots.txt Content-Signal for {}: {}",
            agents(&s.agents, s.more_agents),
            pairs(&s.pairs)
        ));
    }
    for u in &d.content_usage {
        parts.insert(format!(
            "robots.txt Content-Usage for {}{}: {}",
            agents(&u.agents, u.more_agents),
            u.path
                .as_deref()
                .map(|p| format!(" {p}"))
                .unwrap_or_default(),
            pairs(&u.pairs)
        ));
    }
    if !d.headers.content_signal.is_empty() {
        parts.insert(format!(
            "Content-Signal header: {}",
            pairs(&d.headers.content_signal)
        ));
    }
    for u in &d.headers.content_usage {
        parts.insert(format!(
            "Content-Usage header{}: {}",
            u.path
                .as_deref()
                .map(|p| format!(" {p}"))
                .unwrap_or_default(),
            pairs(&u.pairs)
        ));
    }
    let mut single = |name: &str, v: &Option<String>| {
        if let Some(v) = v {
            parts.insert(format!("{name}: {v}"));
        }
    };
    single("TDM-Reservation header", &d.headers.tdm_reservation);
    single("TDM-Policy header", &d.headers.tdm_policy);
    single("tdm-reservation meta", &d.tdm_meta.reservation);
    single("tdm-policy meta", &d.tdm_meta.policy);
    if let Some(file) = &d.tdmrep {
        if file.error.is_some() {
            parts.insert("tdmrep.json: unreadable".to_owned());
        }
        for e in &file.entries {
            parts.insert(format!(
                "tdmrep.json {}: reservation={}{}",
                e.location,
                e.reservation
                    .map_or_else(|| "unset".to_owned(), |r| r.to_string()),
                e.policy
                    .as_deref()
                    .map(|p| format!(", policy={p}"))
                    .unwrap_or_default()
            ));
        }
    }
    parts
}

/// At most [`MAX_CHANGE_TEXT`] characters, cut on a character boundary with a closing "…".
fn truncate(s: &str) -> String {
    let s = without_nul(s);
    if s.chars().count() <= MAX_CHANGE_TEXT {
        return s;
    }
    let mut out: String = s.chars().take(MAX_CHANGE_TEXT - 1).collect();
    out.push('…');
    out
}

fn without_nul(s: &str) -> String {
    s.replace('\0', "")
}

/// Postgres text and jsonb can't hold a NUL; robots.txt and header values can.
fn strip_nul(v: &mut Value) {
    match v {
        Value::String(s) => {
            if s.contains('\0') {
                *s = without_nul(s);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(strip_nul),
        Value::Object(o) => {
            let keys: Vec<String> = o.keys().filter(|k| k.contains('\0')).cloned().collect();
            for k in keys {
                if let Some(val) = o.remove(&k) {
                    o.insert(without_nul(&k), val);
                }
            }
            o.values_mut().for_each(strip_nul);
        }
        _ => {}
    }
}
