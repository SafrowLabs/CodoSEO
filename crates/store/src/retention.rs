//! Daily cleanup: trims old crawl history past each plan's retention window, deletes
//! unclaimed no-signup audits after 7 days, and clears out expired tokens/sessions and
//! old failed jobs. Each step is its own statement — these are independent cleanup
//! passes, not one atomic unit (unlike `finalize`, which must be all-or-nothing).

use codoseo_core::plan::{Plan, PlanLimits};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RetentionReport {
    pub crawls_trimmed: u64,
    pub unclaimed_sites_deleted: u64,
    pub tokens_deleted: u64,
    pub sessions_deleted: u64,
    pub failed_jobs_deleted: u64,
}

/// Runs every cleanup pass once. `self_hosted_history_days` overrides the self-hosted
/// plan's default of 365 days (spec: "1 year (configurable)"); cloud plans always use
/// `PlanLimits::for_plan`'s fixed `history_days`.
pub async fn run(
    pool: &PgPool,
    self_hosted_history_days: Option<u32>,
) -> Result<RetentionReport, sqlx::Error> {
    let mut crawls_trimmed = 0;
    for plan in [Plan::Free, Plan::Pro, Plan::Agency, Plan::SelfHosted] {
        let days = match plan {
            Plan::SelfHosted => self_hosted_history_days.unwrap_or(365),
            _ => PlanLimits::for_plan(plan)
                .history_days
                .expect("cloud plans always have a history_days limit"),
        };
        crawls_trimmed += trim_plan_history(pool, plan_db_value(plan), i64::from(days)).await?;
    }

    let unclaimed_sites_deleted = sqlx::query(
        "DELETE FROM sites WHERE account_id IS NULL AND created_at < now() - interval '7 days'",
    )
    .execute(pool)
    .await?
    .rows_affected();

    let tokens_deleted = sqlx::query("DELETE FROM login_tokens WHERE expires_at < now()")
        .execute(pool)
        .await?
        .rows_affected();

    let sessions_deleted = sqlx::query("DELETE FROM sessions WHERE expires_at < now()")
        .execute(pool)
        .await?
        .rows_affected();

    let failed_jobs_deleted = sqlx::query(
        "DELETE FROM jobs WHERE status = 'failed' \
           AND COALESCE(completed_at, claimed_at, created_at) < now() - interval '30 days'",
    )
    .execute(pool)
    .await?
    .rows_affected();

    Ok(RetentionReport {
        crawls_trimmed,
        unclaimed_sites_deleted,
        tokens_deleted,
        sessions_deleted,
        failed_jobs_deleted,
    })
}

/// Drops `changes` and nulls `summary` for `done` crawls of accounts on `plan_value` whose
/// `finished_at` is at least `days` old. A crawl whose `summary` is already `NULL` (cleaned by
/// an earlier run) is left out of the count. Two statements against the same id list rather
/// than one combined query, since Postgres has no single-statement "delete from one table,
/// update another" form.
async fn trim_plan_history(pool: &PgPool, plan_value: &str, days: i64) -> Result<u64, sqlx::Error> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT c.id FROM crawls c \
           JOIN sites s ON s.id = c.site_id \
           JOIN accounts a ON a.id = s.account_id \
         WHERE a.plan = $1::plan AND c.status = 'done' \
           AND c.finished_at <= now() - ($2 || ' days')::interval \
           AND c.summary IS NOT NULL",
    )
    .bind(plan_value)
    .bind(days.to_string())
    .fetch_all(pool)
    .await?;

    if ids.is_empty() {
        return Ok(0);
    }

    sqlx::query("DELETE FROM changes WHERE crawl_id = ANY($1)")
        .bind(&ids)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE crawls SET summary = NULL WHERE id = ANY($1)")
        .bind(&ids)
        .execute(pool)
        .await?;
    Ok(ids.len() as u64)
}

fn plan_db_value(plan: Plan) -> &'static str {
    match plan {
        Plan::Free => "free",
        Plan::Pro => "pro",
        Plan::Agency => "agency",
        Plan::SelfHosted => "self_hosted",
    }
}
