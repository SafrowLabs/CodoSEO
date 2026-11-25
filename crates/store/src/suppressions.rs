//! Addresses CodoSEO must not email (`email_suppressions`), keyed by the canonical address.
//!
//! `Notifications` stops alerts, the weekly digest and reminders: the person unsubscribed, and
//! can turn email back on from their alert settings. `All` stops every email, sign-in links
//! included: the person asked never to be emailed, or the address bounced or complained. A
//! scope only ever widens; lifting a suppression is a separate, deliberate act.

use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    Notifications,
    All,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Notifications => "notifications",
            Scope::All => "all",
        }
    }

    pub fn parse(slug: &str) -> Option<Scope> {
        match slug {
            "notifications" => Some(Scope::Notifications),
            "all" => Some(Scope::All),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suppression {
    pub scope: Scope,
    /// `unsubscribe` (a link in a notification), `opt_out` (a link in a first-contact email),
    /// `bounce`, `complaint`, or whatever an operator records.
    pub reason: String,
    pub created_at: OffsetDateTime,
}

#[derive(FromRow)]
struct Row {
    scope: String,
    reason: String,
    created_at: OffsetDateTime,
}

/// Records that `canonical` must not be emailed within `scope`. An existing wider scope is kept;
/// a narrower one is widened, and the reason is updated either way.
pub async fn add(
    pool: &PgPool,
    canonical: &str,
    scope: Scope,
    reason: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO email_suppressions (address, scope, reason) \
         VALUES ($1, $2::email_suppression_scope, $3) \
         ON CONFLICT (address) DO UPDATE SET \
           scope = GREATEST(email_suppressions.scope, EXCLUDED.scope), \
           reason = EXCLUDED.reason, \
           created_at = now()",
    )
    .bind(canonical)
    .bind(scope.as_str())
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get(pool: &PgPool, canonical: &str) -> Result<Option<Suppression>, sqlx::Error> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT scope::text AS scope, reason, created_at FROM email_suppressions WHERE address = $1",
    )
    .bind(canonical)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| Suppression {
        scope: Scope::parse(&r.scope).unwrap_or(Scope::All),
        reason: r.reason,
        created_at: r.created_at,
    }))
}

/// Whether a notification (an alert, the digest, a reminder) may not go to `canonical`.
pub async fn blocks_notifications(pool: &PgPool, canonical: &str) -> Result<bool, sqlx::Error> {
    Ok(get(pool, canonical).await?.is_some())
}

/// Whether nothing at all may go to `canonical`, sign-in links included.
pub async fn blocks_all(pool: &PgPool, canonical: &str) -> Result<bool, sqlx::Error> {
    Ok(get(pool, canonical)
        .await?
        .is_some_and(|s| s.scope == Scope::All))
}

/// Lifts the suppression on `canonical`. Returns whether there was one.
pub async fn remove(pool: &PgPool, canonical: &str) -> Result<bool, sqlx::Error> {
    let done = sqlx::query("DELETE FROM email_suppressions WHERE address = $1")
        .bind(canonical)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() == 1)
}
