//! AI access in the database: the owner's intent per site, the access report of each crawl, and
//! the incidents the findings open and resolve.
//!
//! A crawl's report and findings are pure data ([`codoseo_geo`]); this module is where they meet
//! what is already open. [`apply_geo`] is the one place that does it, in the caller's
//! transaction: `finalize` calls it for a finished crawl and [`record_failed_crawl`] for one that
//! failed with a failing robots.txt, so the two never disagree.
//!
//! The first report a site ever gets is its *baseline*: the incidents it opens are `quiet` and
//! write no change rows, so switching the feature on does not fire an alert storm. A change of
//! intent ([`reevaluate_quietly`]) is quiet the same way.

use std::collections::{BTreeSet, HashMap};

use codoseo_core::change::{Change, ChangeKind};
use codoseo_core::check::Severity;
use codoseo_geo::Intent;
use codoseo_geo::findings::{Finding, FindingKind, evaluated_kinds, findings};
use codoseo_geo::incident::{OpenIncident, Transition, reconcile};
use codoseo_geo::report::{AccessReport, Declared};
use codoseo_geo::robots::{Pair, RobotsAvailability};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::dbenum::enum_slug;
use crate::finalize::{enqueue_alert_job, insert_changes};

/// Reports kept per site: the latest two, like the per-URL data of a crawl.
const KEEP_REPORTS: i64 = 2;
/// Longest `before_value`/`after_value` a change row gets, in characters.
const MAX_CHANGE_TEXT: usize = 300;
/// Most per-bot overrides an intent may hold.
const MAX_BOT_OVERRIDES: usize = 200;
/// Longest bot token an intent may hold.
const MAX_TOKEN_LEN: usize = 100;

/// Everything the store needs from one crawl's AI access evaluation.
#[derive(Debug, Clone)]
pub struct GeoInput {
    pub report: AccessReport,
    pub findings: Vec<Finding>,
    /// The kinds the report could judge; open incidents of other kinds are left alone.
    pub evaluated: Vec<FindingKind>,
}

impl GeoInput {
    /// Draws the findings from `report` under the site's `intent`.
    pub fn new(report: AccessReport, intent: &Intent) -> GeoInput {
        GeoInput {
            findings: findings(&report, intent),
            evaluated: evaluated_kinds(&report),
            report,
        }
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
        })
    }
}

const INCIDENT_COLUMNS: &str = "id, kind, subject, severity::text AS severity, title, summary, \
     evidence, opened_crawl_id, opened_at, last_seen_at, resolved_at, resolution, quiet";

fn parse_severity(s: &str) -> Option<Severity> {
    serde_json::from_value(Value::String(s.to_owned())).ok()
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

/// Checks an intent before it is stored: sane sizes, and no `Any` override that says nothing.
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
pub async fn open_counts(pool: &PgPool, site_id: Uuid) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE severity = 'critical') \
         FROM ai_incidents WHERE site_id = $1 AND resolved_at IS NULL",
    )
    .bind(site_id)
    .fetch_one(pool)
    .await
}

/// After an intent change: the findings on the site's latest report under the new intent,
/// reconciled with the open incidents. Newly wanted findings open `quiet`, findings the intent
/// made moot resolve with `resolution = 'intent'`, and no change rows or alerts are written: the
/// owner just made the choice. Returns how many were (opened, resolved).
pub async fn reevaluate_quietly(
    pool: &PgPool,
    site_id: Uuid,
) -> Result<(usize, usize), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let Some(latest) = latest_report_in(&mut *tx, site_id).await? else {
        return Ok((0, 0));
    };
    let intent: Intent = {
        let value: Option<Value> = sqlx::query_scalar("SELECT ai_intent FROM sites WHERE id = $1")
            .bind(site_id)
            .fetch_optional(&mut *tx)
            .await?;
        value
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    };
    let input = GeoInput::new(latest.report, &intent);
    let open = lock_open(&mut tx, site_id).await?;
    let transitions = reconcile(&as_open(&open), &input.findings, &input.evaluated);
    let opened = transitions
        .iter()
        .filter(|t| matches!(t, Transition::Opened { .. }))
        .count();
    let resolved = transitions
        .iter()
        .filter(|t| matches!(t, Transition::Resolved { .. }))
        .count();
    write_transitions(
        &mut tx,
        site_id,
        latest.crawl_id,
        &transitions,
        &open,
        Mode {
            quiet: true,
            changes: false,
            resolution: "intent",
        },
    )
    .await?;
    tx.commit().await?;
    Ok((opened, resolved))
}

/// A crawl that failed (its site gave no pages) but did read a failing robots.txt: records its
/// report and reconciles in a transaction of its own. Only `RobotsUnavailable` can be judged
/// from such a report, so only that incident opens, updates or resolves. Writes the change row
/// and queues the `send_alert` job like a finished crawl would. Returns the number of changes.
pub async fn record_failed_crawl(
    pool: &PgPool,
    site_id: Uuid,
    crawl_id: Uuid,
    input: &GeoInput,
) -> Result<usize, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let changes = apply_geo(&mut tx, site_id, crawl_id, input).await?;
    insert_changes(&mut tx, crawl_id, site_id, &changes).await?;
    enqueue_alert_job(&mut tx, crawl_id, !changes.is_empty()).await?;
    tx.commit().await?;
    Ok(changes.len())
}

/// Stores the crawl's report, keeps the site's newest two, and opens, updates and resolves the
/// site's incidents from the findings. Returns the changes to record (the caller inserts them
/// with the crawl's other changes).
///
/// The site's open incidents are locked first (`FOR UPDATE`), so an intent change that lands
/// mid-crawl waits for this transaction instead of interleaving with it. When the site has no
/// earlier report (a different crawl's) this is the baseline: incidents open `quiet` and nothing
/// is returned. A report can be written twice for one crawl (a failed crawl retries under the
/// same id); the later one replaces the earlier and the "earlier report" is still another crawl's.
pub(crate) async fn apply_geo(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    crawl_id: Uuid,
    input: &GeoInput,
) -> Result<Vec<Change>, sqlx::Error> {
    let previous: Option<Value> = sqlx::query_scalar(
        "SELECT report FROM ai_reports WHERE site_id = $1 AND crawl_id <> $2 \
         ORDER BY created_at DESC, crawl_id DESC LIMIT 1",
    )
    .bind(site_id)
    .bind(crawl_id)
    .fetch_optional(&mut **tx)
    .await?;
    let baseline = previous.is_none();
    let previous: Option<AccessReport> = previous.and_then(|v| serde_json::from_value(v).ok());

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
    sqlx::query(
        "DELETE FROM ai_reports WHERE site_id = $1 AND crawl_id NOT IN (\
           SELECT crawl_id FROM ai_reports WHERE site_id = $1 \
           ORDER BY created_at DESC, crawl_id DESC LIMIT $2)",
    )
    .bind(site_id)
    .bind(KEEP_REPORTS)
    .execute(&mut **tx)
    .await?;

    let open = lock_open(tx, site_id).await?;
    let transitions = reconcile(&as_open(&open), &input.findings, &input.evaluated);
    let mut changes = write_transitions(
        tx,
        site_id,
        crawl_id,
        &transitions,
        &open,
        Mode {
            quiet: baseline,
            changes: !baseline,
            resolution: "fixed",
        },
    )
    .await?;

    if !baseline
        && let Some(previous) = &previous
        && let Some(change) = preferences_change(previous, &input.report)
    {
        changes.push(change);
    }
    Ok(changes)
}

/// How [`write_transitions`] treats what it writes.
struct Mode {
    /// Incidents opened by this run are `quiet`.
    quiet: bool,
    /// Whether to return change rows for the transitions.
    changes: bool,
    /// The `resolution` recorded on resolved incidents.
    resolution: &'static str,
}

/// An open incident as read for reconciling.
struct OpenRow {
    id: i64,
    kind: FindingKind,
    subject: String,
    severity: Severity,
    title: String,
    quiet: bool,
}

async fn lock_open(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
) -> Result<Vec<OpenRow>, sqlx::Error> {
    let rows: Vec<(i64, String, String, String, String, bool)> = sqlx::query_as(
        "SELECT id, kind, subject, severity::text, title, quiet FROM ai_incidents \
         WHERE site_id = $1 AND resolved_at IS NULL ORDER BY id FOR UPDATE",
    )
    .bind(site_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, kind, subject, severity, title, quiet)| {
            Some(OpenRow {
                id,
                kind: FindingKind::from_slug(&kind)?,
                subject,
                severity: parse_severity(&severity)?,
                title,
                quiet,
            })
        })
        .collect())
}

fn as_open(rows: &[OpenRow]) -> Vec<OpenIncident> {
    rows.iter()
        .map(|r| OpenIncident {
            id: r.id,
            kind: r.kind,
            subject: r.subject.clone(),
            severity: r.severity,
        })
        .collect()
}

async fn write_transitions(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    crawl_id: Uuid,
    transitions: &[Transition],
    open: &[OpenRow],
    mode: Mode,
) -> Result<Vec<Change>, sqlx::Error> {
    let by_id: HashMap<i64, &OpenRow> = open.iter().map(|r| (r.id, r)).collect();
    let mut changes = Vec::new();
    for transition in transitions {
        match transition {
            Transition::Opened { finding } => {
                let mut evidence =
                    serde_json::to_value(&finding.evidence).expect("evidence always serializes");
                strip_nul(&mut evidence);
                // A concurrent writer may have opened the same problem since the lock was
                // taken (the lock only covers rows that existed); refresh that row instead.
                sqlx::query(
                    "INSERT INTO ai_incidents (site_id, kind, subject, severity, title, summary, \
                       evidence, opened_crawl_id, last_seen_crawl_id, quiet) \
                     VALUES ($1, $2, $3, $4::severity, $5, $6, $7, $8, $8, $9) \
                     ON CONFLICT (site_id, kind, subject) WHERE resolved_at IS NULL DO UPDATE SET \
                       severity = EXCLUDED.severity, title = EXCLUDED.title, \
                       summary = EXCLUDED.summary, evidence = EXCLUDED.evidence, \
                       last_seen_crawl_id = EXCLUDED.last_seen_crawl_id, last_seen_at = now()",
                )
                .bind(site_id)
                .bind(finding.kind.slug())
                .bind(&finding.subject)
                .bind(enum_slug(&finding.severity))
                .bind(without_nul(&finding.title))
                .bind(without_nul(&finding.summary))
                .bind(evidence)
                .bind(crawl_id)
                .bind(mode.quiet)
                .execute(&mut **tx)
                .await?;
                if mode.changes {
                    changes.push(finding_change(finding));
                }
            }
            Transition::Updated {
                id,
                finding,
                escalated,
            } => {
                let mut evidence =
                    serde_json::to_value(&finding.evidence).expect("evidence always serializes");
                strip_nul(&mut evidence);
                sqlx::query(
                    "UPDATE ai_incidents SET severity = $2::severity, title = $3, summary = $4, \
                       evidence = $5, last_seen_crawl_id = $6, last_seen_at = now() \
                     WHERE id = $1",
                )
                .bind(id)
                .bind(enum_slug(&finding.severity))
                .bind(without_nul(&finding.title))
                .bind(without_nul(&finding.summary))
                .bind(evidence)
                .bind(crawl_id)
                .execute(&mut **tx)
                .await?;
                if mode.changes && *escalated {
                    changes.push(finding_change(finding));
                }
            }
            Transition::Resolved { id, .. } => {
                sqlx::query(
                    "UPDATE ai_incidents SET resolved_at = now(), resolved_crawl_id = $2, \
                       resolution = $3 WHERE id = $1",
                )
                .bind(id)
                .bind(crawl_id)
                .bind(mode.resolution)
                .execute(&mut **tx)
                .await?;
                // Nobody was told a quiet incident opened, so nobody is told it is gone.
                if let Some(row) = by_id.get(id)
                    && mode.changes
                    && !row.quiet
                {
                    changes.push(Change {
                        kind: ChangeKind::AiIssueResolved,
                        severity: Severity::Notice,
                        url: None,
                        before: truncate(&row.title),
                        after: "resolved".to_owned(),
                    });
                }
            }
        }
    }
    Ok(changes)
}

/// The change row for an incident that opened or got worse. `before` is what was expected,
/// `after` the finding's title; the summary and evidence live on the incident.
fn finding_change(finding: &Finding) -> Change {
    let (kind, before) = match finding.kind {
        FindingKind::RobotsUnavailable | FindingKind::BotsBlocked => {
            (ChangeKind::AiBotBlocked, "allowed")
        }
        FindingKind::BotsNotBlocked => (ChangeKind::AiBlockNotApplied, "intent: block"),
        FindingKind::AnswersRestricted => (ChangeKind::AiAnswersRestricted, "eligible"),
    };
    Change {
        kind,
        severity: finding.severity,
        url: None,
        before: before.to_owned(),
        after: truncate(&finding.title),
    }
}

/// `Some` when the declared AI preferences differ between two reports that both read robots.txt
/// (a failing robots.txt says nothing about them). Lists only what was removed and what was
/// added, not line numbers, so moving a line is no change.
fn preferences_change(previous: &AccessReport, current: &AccessReport) -> Option<Change> {
    let readable = |r: &AccessReport| {
        matches!(
            r.robots.availability,
            RobotsAvailability::Ok | RobotsAvailability::Missing
        )
    };
    if !readable(previous) || !readable(current) {
        return None;
    }
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
    fn agents(a: &[String]) -> String {
        if a.is_empty() {
            "*".to_owned()
        } else {
            a.join(",")
        }
    }
    let mut parts = BTreeSet::new();
    for s in &d.content_signals {
        parts.insert(format!(
            "robots.txt Content-Signal for {}: {}",
            agents(&s.agents),
            pairs(&s.pairs)
        ));
    }
    for u in &d.content_usage {
        parts.insert(format!(
            "robots.txt Content-Usage for {}{}: {}",
            agents(&u.agents),
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
