//! Funnel events (spec section 11): audit started → audit finished → email given → link
//! clicked → first full crawl → active after 4 weeks → RankOrg click. One row per step, so the
//! admin page can show where visitors drop off.

use sqlx::PgExecutor;
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
