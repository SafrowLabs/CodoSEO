//! Funnel events (spec section 11): audit started → audit finished → email given → link
//! clicked → first full crawl → active after 4 weeks → RankOrg click. One row per step, so the
//! admin page can show where visitors drop off.

use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    AuditStarted,
    AuditFinished,
    EmailGiven,
    LinkClicked,
    FirstFullCrawl,
    ActiveAfter4Weeks,
    RankorgClick,
}

impl EventKind {
    /// The funnel in order.
    pub const ALL: [EventKind; 7] = [
        EventKind::AuditStarted,
        EventKind::AuditFinished,
        EventKind::EmailGiven,
        EventKind::LinkClicked,
        EventKind::FirstFullCrawl,
        EventKind::ActiveAfter4Weeks,
        EventKind::RankorgClick,
    ];

    /// What `events.kind` holds.
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::AuditStarted => "audit_started",
            EventKind::AuditFinished => "audit_finished",
            EventKind::EmailGiven => "email_given",
            EventKind::LinkClicked => "link_clicked",
            EventKind::FirstFullCrawl => "first_full_crawl",
            EventKind::ActiveAfter4Weeks => "active_after_4_weeks",
            EventKind::RankorgClick => "rankorg_click",
        }
    }
}

/// Records one event. Takes any executor, so `finalize` can write it inside its transaction.
pub async fn record(
    executor: impl PgExecutor<'_>,
    kind: EventKind,
    account_id: Option<Uuid>,
    site_id: Option<Uuid>,
    payload: Option<serde_json::Value>,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO events (account_id, site_id, kind, payload) VALUES ($1, $2, $3, $4)")
        .bind(account_id)
        .bind(site_id)
        .bind(kind.as_str())
        .bind(payload)
        .execute(executor)
        .await?;
    Ok(())
}

/// One step of the funnel over a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunnelCount {
    pub kind: EventKind,
    /// Every event recorded.
    pub events: i64,
    /// Distinct audits (or sites, or clicks) behind them: two visitors reading one cached
    /// audit are two `audit_started` events but one audit. Conversions use this number.
    pub unique: i64,
}

/// Every funnel step, in order, for the last `days` days. Steps with no events are zeros.
pub async fn funnel_counts(pool: &PgPool, days: i64) -> Result<Vec<FunnelCount>, sqlx::Error> {
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT kind, count(*), \
                count(DISTINCT COALESCE(payload->>'crawl_id', site_id::text, id::text)) \
         FROM events WHERE created_at > now() - ($1 || ' days')::interval GROUP BY kind",
    )
    .bind(days.to_string())
    .fetch_all(pool)
    .await?;
    Ok(EventKind::ALL
        .into_iter()
        .map(|kind| {
            let (events, unique) = rows
                .iter()
                .find(|(k, ..)| k == kind.as_str())
                .map_or((0, 0), |&(_, e, u)| (e, u));
            FunnelCount {
                kind,
                events,
                unique,
            }
        })
        .collect())
}
