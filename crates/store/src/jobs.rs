//! The generic jobs queue: non-crawl work (`send_alert`, `send_digest`, `send_email`,
//! `cleanup`), claimed the same way as the crawl queue (`SKIP LOCKED`), with exponential
//! backoff on retry.

use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// Mirrors the `job_kind` Postgres enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "job_kind", rename_all = "snake_case")]
pub enum JobKind {
    SendAlert,
    SendDigest,
    SendEmail,
    Cleanup,
}

impl JobKind {
    /// The `job_kind` label.
    pub fn slug(self) -> &'static str {
        match self {
            JobKind::SendAlert => "send_alert",
            JobKind::SendDigest => "send_digest",
            JobKind::SendEmail => "send_email",
            JobKind::Cleanup => "cleanup",
        }
    }
}

#[derive(Debug, FromRow)]
pub struct ClaimedJob {
    pub id: Uuid,
    pub kind: JobKind,
    pub payload: serde_json::Value,
    pub attempt: i16,
    pub max_attempts: i16,
}

#[derive(Clone)]
pub struct JobQueue {
    pool: PgPool,
}

impl JobQueue {
    pub fn new(pool: PgPool) -> JobQueue {
        JobQueue { pool }
    }

    pub async fn enqueue(
        &self,
        kind: JobKind,
        payload: serde_json::Value,
    ) -> Result<Uuid, sqlx::Error> {
        sqlx::query_scalar("INSERT INTO jobs (kind, payload) VALUES ($1, $2) RETURNING id")
            .bind(kind)
            .bind(payload)
            .fetch_one(&self.pool)
            .await
    }

    /// Claims the next due job with `SKIP LOCKED`, marking it `running`.
    pub async fn claim(&self, worker_id: &str) -> Result<Option<ClaimedJob>, sqlx::Error> {
        sqlx::query_as(
            "UPDATE jobs SET status = 'running', claimed_by = $1, claimed_at = now() \
             WHERE id = ( \
               SELECT id FROM jobs WHERE status = 'queued' AND run_after <= now() \
               ORDER BY run_after FOR UPDATE SKIP LOCKED LIMIT 1 \
             ) \
             RETURNING id, kind, payload, attempt, max_attempts",
        )
        .bind(worker_id)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn complete(&self, id: Uuid) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE jobs SET status = 'done', completed_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Backs off `2^attempt` minutes (1, 2, 4, 8, 16 for attempts 1..=5), failing for good once
    /// `attempt >= max_attempts`. Failed jobs are left in place for retention (T4.5) to delete
    /// after 30 days, not deleted here.
    pub async fn retry(&self, id: Uuid, error: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE jobs SET \
               attempt = attempt + 1, \
               last_error = $2, \
               status = CASE WHEN attempt + 1 >= max_attempts THEN 'failed'::job_status \
                             ELSE 'queued'::job_status END, \
               run_after = now() + (power(2, attempt) * interval '1 minute'), \
               claimed_by = NULL, \
               claimed_at = NULL \
             WHERE id = $1",
        )
        .bind(id)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Moves `running` jobs claimed longer than `older_than` ago back to `queued` (the
    /// dead-worker path) and returns how many moved. It counts as an attempt and backs off like
    /// [`JobQueue::retry`], so a job that kills its worker every time still ends `failed`.
    pub async fn requeue_stale(&self, older_than: std::time::Duration) -> Result<u64, sqlx::Error> {
        let done = sqlx::query(
            "UPDATE jobs SET \
               attempt = attempt + 1, \
               last_error = 'the worker stopped before the job finished', \
               status = CASE WHEN attempt + 1 >= max_attempts THEN 'failed'::job_status \
                             ELSE 'queued'::job_status END, \
               run_after = now() + (power(2, attempt) * interval '1 minute'), \
               claimed_by = NULL, \
               claimed_at = NULL \
             WHERE status = 'running' AND claimed_at < now() - make_interval(secs => $1)",
        )
        .bind(older_than.as_secs_f64())
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }
}

/// A job that ran out of attempts, for the admin page.
#[derive(Debug, FromRow)]
pub struct FailedJob {
    pub id: Uuid,
    pub kind: JobKind,
    pub attempt: i16,
    pub last_error: Option<String>,
    pub created_at: time::OffsetDateTime,
}

/// The most recent failed jobs (kept 30 days), newest first.
pub async fn failed_jobs(pool: &PgPool, limit: i64) -> Result<Vec<FailedJob>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, kind, attempt, last_error, created_at FROM jobs \
         WHERE status = 'failed' ORDER BY created_at DESC, id LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}
