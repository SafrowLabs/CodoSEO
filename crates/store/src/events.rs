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
    /// A "Send test" on the alerts screen. Not a funnel step (not in [`EventKind::ALL`]); the
    /// rows are what rate-limits the button.
    ChannelTest,
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
            EventKind::ChannelTest => "channel_test",
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

/// Takes one of `limit` allowances per `window_minutes` for this account, by recording an event
/// of `kind`. `Ok(Err(minutes))` when none is left: the whole minutes until the oldest one in
/// the window expires (at least 1). The check and the insert share a lock, so concurrent calls
/// can't both take the last allowance.
pub async fn take_allowance(
    pool: &PgPool,
    kind: EventKind,
    account_id: Uuid,
    limit: i64,
    window_minutes: i64,
) -> Result<Result<(), i64>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(format!("allowance:{}:{account_id}", kind.as_str()))
        .execute(&mut *tx)
        .await?;
    let (used, wait_secs): (i64, Option<f64>) = sqlx::query_as(
        "SELECT count(*), \
                EXTRACT(EPOCH FROM (min(created_at) + make_interval(mins => $3) - now()))::float8 \
         FROM events WHERE kind = $1 AND account_id = $2 \
           AND created_at > now() - make_interval(mins => $3)",
    )
    .bind(kind.as_str())
    .bind(account_id)
    .bind(window_minutes as i32)
    .fetch_one(&mut *tx)
    .await?;
    if used >= limit {
        let minutes = (wait_secs.unwrap_or(0.0) / 60.0).ceil().max(1.0) as i64;
        return Ok(Err(minutes));
    }
    record(&mut *tx, kind, Some(account_id), None, None).await?;
    tx.commit().await?;
    Ok(Ok(()))
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

/// The website's funnel for the last `days` days, in order: every step, with no agent events
/// (`payload.source = 'agent'`). Steps with no events are zeros.
pub async fn funnel_counts(pool: &PgPool, days: i64) -> Result<Vec<FunnelCount>, sqlx::Error> {
    funnel(pool, days, false).await
}

/// The same steps for agents only: audits started through the no-key MCP tools, emails given
/// to `start_monitoring`, links clicked, and the audits and first crawls that came of them.
pub async fn agent_funnel_counts(
    pool: &PgPool,
    days: i64,
) -> Result<Vec<FunnelCount>, sqlx::Error> {
    funnel(pool, days, true).await
}

async fn funnel(pool: &PgPool, days: i64, agents: bool) -> Result<Vec<FunnelCount>, sqlx::Error> {
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT kind, count(*), \
                count(DISTINCT COALESCE(payload->>'crawl_id', site_id::text, id::text)) \
         FROM events WHERE created_at > now() - ($1 || ' days')::interval \
           AND (COALESCE(payload->>'source', '') = 'agent') = $2 GROUP BY kind",
    )
    .bind(days.to_string())
    .bind(agents)
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
