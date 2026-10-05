//! Plan changes that touch sites: what happens when a plan ends or changes. Shared by the
//! scheduler (plans that ran out) and billing (a subscription that started, changed or ended).
//!
//! Nothing here deletes data. Sites beyond the new limit stop being monitored, and the owner
//! chooses which ones stay. An upgrade does not turn stopped sites back on; it only moves the
//! sites that are monitored to the faster schedule.

use codoseo_core::plan::{Plan, PlanLimits, Schedule};
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::accounts::parse_plan;

/// Moves every paid account whose `plan_expires_at` is before `now` to Free (clearing the
/// expiry) and applies the Free limits to its sites. Returns how many accounts moved.
/// Self-hosted accounts have no expiry and are never touched.
pub async fn downgrade_expired(pool: &PgPool, now: OffsetDateTime) -> Result<u64, sqlx::Error> {
    let due: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM accounts \
         WHERE plan IN ('pro', 'agency') AND plan_expires_at < $1 ORDER BY plan_expires_at",
    )
    .bind(now)
    .fetch_all(pool)
    .await?;
    let mut moved = 0;
    for id in due {
        let mut tx = pool.begin().await?;
        // The same conditions again, so a second scheduler (or a renewal that landed since the
        // list was read) leaves the account alone.
        let changed = sqlx::query(
            "UPDATE accounts SET plan = 'free', plan_expires_at = NULL \
             WHERE id = $1 AND plan IN ('pro', 'agency') AND plan_expires_at < $2",
        )
        .bind(id)
        .bind(now)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed == 1 {
            apply_plan_limits(&mut tx, id).await?;
            moved += 1;
        }
        tx.commit().await?;
    }
    Ok(moved)
}

/// Brings an account's sites in line with its current plan (read from the account row):
/// monitored sites beyond `max_sites` are deactivated, keeping the oldest; schedules faster
/// than the plan allows are capped; and on a paid plan the monitored sites move to daily (their
/// next crawl is recomputed by the scheduler). Run it in the same transaction as the plan
/// change.
pub async fn apply_plan_limits(
    conn: &mut PgConnection,
    account_id: Uuid,
) -> Result<(), sqlx::Error> {
    let plan: String = sqlx::query_scalar("SELECT plan::text FROM accounts WHERE id = $1")
        .bind(account_id)
        .fetch_one(&mut *conn)
        .await?;
    let plan = parse_plan(&plan);
    let limits = PlanLimits::for_plan(plan);

    if let Some(max) = limits.max_sites {
        sqlx::query(
            "UPDATE sites SET monitoring_active = false WHERE id IN ( \
               SELECT id FROM sites WHERE account_id = $1 AND monitoring_active \
               ORDER BY created_at, id OFFSET $2)",
        )
        .bind(account_id)
        .bind(i64::from(max))
        .execute(&mut *conn)
        .await?;
    }

    // `next_crawl_at = NULL` makes the scheduler pick the next slot of the new schedule.
    match limits.fastest_schedule {
        // Paying is what makes a site daily (a Free site is weekly even though the Free
        // limits are checked here too), so only the paid plans are moved up. Self-hosted
        // sites keep whatever schedule their owner chose.
        Some(Schedule::Daily) if matches!(plan, Plan::Pro | Plan::Agency) => {
            sqlx::query(
                "UPDATE sites SET schedule = 'daily', next_crawl_at = NULL \
                 WHERE account_id = $1 AND monitoring_active AND schedule IS DISTINCT FROM 'daily'",
            )
            .bind(account_id)
            .execute(&mut *conn)
            .await?;
        }
        Some(Schedule::Daily) => {}
        Some(Schedule::Weekly) => {
            sqlx::query(
                "UPDATE sites SET schedule = 'weekly', next_crawl_at = NULL \
                 WHERE account_id = $1 AND schedule = 'daily'",
            )
            .bind(account_id)
            .execute(&mut *conn)
            .await?;
        }
        None => {
            sqlx::query(
                "UPDATE sites SET schedule = NULL, next_crawl_at = NULL \
                 WHERE account_id = $1 AND schedule IS NOT NULL",
            )
            .bind(account_id)
            .execute(&mut *conn)
            .await?;
        }
    }
    Ok(())
}
