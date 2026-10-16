//! The site policy in the database: `sites.policy` (a [`SitePolicy`] as JSON) and its history.
//!
//! [`save`] is the one way a policy changes, from the policy screen, the API, an import or "Mark
//! intended": in one transaction it locks the site row, validates, writes the policy and the
//! `ai_intent`/`ai_probes` mirror (kept for one release, so a rollback reads the same intent),
//! re-evaluates the stored AI access reports quietly and rescores the newest finished crawl
//! (exactly what saving the intent did before, [`crate::geo::reevaluate_quietly`]), and records a
//! [`policy_history`](history) row with the scores before and after.
//!
//! A stored value is read leniently ([`SitePolicy::from_json_lenient`]), and a site whose
//! `policy` is NULL reads its mirror columns, so a policy is never unreadable.

use codoseo_policy::{PolicyError, SitePolicy};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

/// Where a policy change came from (`policy_history.via`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Web,
    Api,
    Mcp,
    Import,
    Migration,
    MarkIntended,
}

impl Via {
    pub fn slug(self) -> &'static str {
        match self {
            Via::Web => "web",
            Via::Api => "api",
            Via::Mcp => "mcp",
            Via::Import => "import",
            Via::Migration => "migration",
            Via::MarkIntended => "mark_intended",
        }
    }
}

/// Who changed the policy, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actor {
    pub via: Via,
    pub account_id: Option<Uuid>,
    pub api_key_id: Option<Uuid>,
}

impl Actor {
    pub fn web(account_id: Uuid) -> Actor {
        Actor::new(Via::Web, account_id)
    }

    pub fn new(via: Via, account_id: Uuid) -> Actor {
        Actor {
            via,
            account_id: Some(account_id),
            api_key_id: None,
        }
    }
}

/// Why a policy was not saved.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("{}", codoseo_policy::error_lines(.0))]
    Invalid(Vec<PolicyError>),
    #[error("no such site")]
    NotFound,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// What a save did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// The site had a stored AI access report to re-evaluate.
    pub had_report: bool,
    /// AI access incidents the new policy opened and resolved (quietly).
    pub opened: usize,
    pub resolved: usize,
    /// The history row, or `None` when nothing changed.
    pub history_id: Option<i64>,
    /// The newest finished crawl's health and GEO scores before and after.
    pub health: (Option<i16>, Option<i16>),
    pub geo: (Option<i16>, Option<i16>),
}

/// The policy from the site row's columns: `policy`, else the mirror (`ai_intent`, `ai_probes`).
pub(crate) fn from_columns(
    policy: Option<Value>,
    ai_intent: Option<Value>,
    ai_probes: Option<bool>,
) -> SitePolicy {
    match policy {
        Some(v) if !v.is_null() => SitePolicy::from_json_lenient(v),
        _ => {
            let mut p = SitePolicy::default();
            if let Some(intent) = ai_intent.and_then(|v| serde_json::from_value(v).ok()) {
                p.ai.intent = intent;
            }
            p.ai.probes = ai_probes.unwrap_or(true);
            p
        }
    }
}

type PolicyColumns = (Option<Value>, Option<Value>, Option<bool>);

/// The site's policy. A site that is gone reads as the defaults.
pub async fn get(pool: &PgPool, site_id: Uuid) -> Result<SitePolicy, sqlx::Error> {
    let row: Option<PolicyColumns> =
        sqlx::query_as("SELECT policy, ai_intent, ai_probes FROM sites WHERE id = $1")
            .bind(site_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map_or_else(SitePolicy::default, |(p, i, pr)| from_columns(p, i, pr)))
}

/// The site's policy and when it last changed (its newest history row; `None` when it never
/// has).
pub async fn get_with_time(
    pool: &PgPool,
    site_id: Uuid,
) -> Result<(SitePolicy, Option<OffsetDateTime>), sqlx::Error> {
    let policy = get(pool, site_id).await?;
    let at: Option<OffsetDateTime> =
        sqlx::query_scalar("SELECT max(created_at) FROM policy_history WHERE site_id = $1")
            .bind(site_id)
            .fetch_one(pool)
            .await?;
    Ok((policy, at))
}

/// Locks the site row and reads its policy. `FOR NO KEY UPDATE` conflicts with a policy save's
/// `UPDATE` and with the AI access writers, but not with rows that merely reference the site, so
/// crawls can still be queued meanwhile. `None` for a site that is gone.
pub(crate) async fn lock(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
) -> Result<Option<SitePolicy>, sqlx::Error> {
    let row: Option<PolicyColumns> = sqlx::query_as(
        "SELECT policy, ai_intent, ai_probes FROM sites WHERE id = $1 FOR NO KEY UPDATE",
    )
    .bind(site_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|(p, i, pr)| from_columns(p, i, pr)))
}

/// Writes the policy and its mirror columns, in the caller's transaction (the row is locked).
pub(crate) async fn write(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    policy: &SitePolicy,
) -> Result<(), sqlx::Error> {
    let intent =
        serde_json::to_value(&policy.ai.intent).expect("an intent always serializes");
    sqlx::query("UPDATE sites SET policy = $2, ai_intent = $3, ai_probes = $4 WHERE id = $1")
        .bind(site_id)
        .bind(policy.to_json())
        .bind(intent)
        .bind(policy.ai.probes)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// The newest finished crawl's id and its health and GEO scores, chosen like
/// `crawls::latest_done`.
async fn latest_scores(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
) -> Result<Option<(Uuid, Option<i16>, Option<i16>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, health_score, geo_score FROM crawls WHERE site_id = $1 AND status = 'done' \
         ORDER BY finished_at DESC, created_at DESC, id DESC LIMIT 1",
    )
    .bind(site_id)
    .fetch_optional(&mut **tx)
    .await
}

/// Validates and saves the site's policy, re-evaluates its AI access quietly, rescores the
/// newest finished crawl and records the change, all in one transaction. Saving the policy it
/// already has re-evaluates all the same but records no history row.
pub async fn save(
    pool: &PgPool,
    site_id: Uuid,
    policy: &SitePolicy,
    actor: &Actor,
) -> Result<Saved, SaveError> {
    policy.validate().map_err(SaveError::Invalid)?;
    // Stored exactly as it reads back.
    let policy = SitePolicy::from_json_lenient(policy.to_json());

    let mut tx = pool.begin().await?;
    let before = lock(&mut tx, site_id).await?.ok_or(SaveError::NotFound)?;
    let scores_before = latest_scores(&mut tx, site_id).await?;
    write(&mut tx, site_id, &policy).await?;
    let had_report: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ai_reports WHERE site_id = $1)")
            .bind(site_id)
            .fetch_one(&mut *tx)
            .await?;
    let (opened, resolved) = crate::geo::reevaluate_in(&mut tx, site_id).await?;
    let scores_after = latest_scores(&mut tx, site_id).await?;
    let health = (
        scores_before.and_then(|s| s.1),
        scores_after.and_then(|s| s.1),
    );
    let geo = (
        scores_before.and_then(|s| s.2),
        scores_after.and_then(|s| s.2),
    );
    let history_id = if before == policy {
        None
    } else {
        Some(
            sqlx::query_scalar(
                "INSERT INTO policy_history (site_id, account_id, via, api_key_id, before, after, \
                   summary, crawl_id, health_before, health_after, geo_before, geo_after) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING id",
            )
            .bind(site_id)
            .bind(actor.account_id)
            .bind(actor.via.slug())
            .bind(actor.api_key_id)
            .bind(before.to_json())
            .bind(policy.to_json())
            .bind(codoseo_policy::diff::summary(&before, &policy))
            .bind(scores_after.map(|s| s.0))
            .bind(health.0)
            .bind(health.1)
            .bind(geo.0)
            .bind(geo.1)
            .fetch_one(&mut *tx)
            .await?,
        )
    };
    tx.commit().await?;
    Ok(Saved {
        had_report,
        opened,
        resolved,
        history_id,
        health,
        geo,
    })
}

/// One change of a site's policy.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct HistoryRow {
    pub id: i64,
    /// `web`, `api`, `mcp`, `import`, `migration` or `mark_intended`.
    pub via: String,
    pub account_id: Option<Uuid>,
    /// Who made it, if the account is still there.
    pub email: Option<String>,
    pub api_key_id: Option<Uuid>,
    /// The key's name, if it is still there.
    pub api_key_name: Option<String>,
    pub summary: String,
    pub before: Option<Value>,
    pub after: Value,
    /// The crawl the save re-evaluated, and its place in the site's history (`#44`).
    pub crawl_id: Option<Uuid>,
    pub crawl_number: Option<i64>,
    pub health_before: Option<i16>,
    pub health_after: Option<i16>,
    pub geo_before: Option<i16>,
    pub geo_after: Option<i16>,
    pub created_at: OffsetDateTime,
}

/// The site's newest `limit` policy changes, newest first.
pub async fn history(
    pool: &PgPool,
    site_id: Uuid,
    limit: i64,
) -> Result<Vec<HistoryRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT h.id, h.via, h.account_id, a.email, h.api_key_id, k.name AS api_key_name, \
                h.summary, h.before, h.after, h.crawl_id, \
                CASE WHEN c.id IS NULL THEN NULL ELSE \
                  (SELECT count(*) FROM crawls c2 WHERE c2.site_id = h.site_id \
                     AND (c2.created_at, c2.id) <= (c.created_at, c.id)) END AS crawl_number, \
                h.health_before, h.health_after, h.geo_before, h.geo_after, h.created_at \
         FROM policy_history h \
         LEFT JOIN accounts a ON a.id = h.account_id \
         LEFT JOIN api_keys k ON k.id = h.api_key_id \
         LEFT JOIN crawls c ON c.id = h.crawl_id \
         WHERE h.site_id = $1 \
         ORDER BY h.created_at DESC, h.id DESC LIMIT $2",
    )
    .bind(site_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}
