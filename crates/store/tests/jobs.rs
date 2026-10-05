//! T4.3: the generic jobs queue (`enqueue`/`claim`/`complete`/`retry`), backed by real Postgres.

mod support;

use serde_json::json;
use sqlx::Row;
use support::TestDb;
use time::OffsetDateTime;

use codoseo_store::jobs::{JobKind, JobQueue};

#[tokio::test]
async fn enqueue_claim_and_complete_happy_path() {
    let db = TestDb::new().await;
    let queue = JobQueue::new(db.pool.clone());

    let id = queue
        .enqueue(JobKind::SendAlert, json!({"crawl_id": "x"}))
        .await
        .expect("enqueue");

    let claimed = queue
        .claim("worker-a")
        .await
        .expect("claim")
        .expect("a job is claimable");
    assert_eq!(claimed.id, id);
    assert_eq!(claimed.payload["crawl_id"], "x");

    queue.complete(id).await.expect("complete");
    let status: String = sqlx::query("SELECT status::text FROM jobs WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .map(|row| row.get(0))
        .expect("fetch status");
    assert_eq!(status, "done");
}

#[tokio::test]
async fn claim_is_skip_locked_between_two_claimers() {
    let db = TestDb::new().await;
    let queue_a = JobQueue::new(db.pool.clone());
    let queue_b = JobQueue::new(db.pool.clone());

    queue_a
        .enqueue(JobKind::Cleanup, json!({}))
        .await
        .expect("enqueue");

    let first = queue_a.claim("worker-a").await.expect("claim a");
    assert!(first.is_some());
    let second = queue_b.claim("worker-b").await.expect("claim b");
    assert!(
        second.is_none(),
        "a claimed job must not be claimable again"
    );
}

#[tokio::test]
async fn retry_backs_off_1_2_4_8_16_minutes_then_fails() {
    let db = TestDb::new().await;
    let queue = JobQueue::new(db.pool.clone());
    let id = queue
        .enqueue(JobKind::SendEmail, json!({}))
        .await
        .expect("enqueue");

    let expected_minutes = [1.0, 2.0, 4.0, 8.0, 16.0];
    for (i, expected) in expected_minutes.iter().enumerate() {
        let before = OffsetDateTime::now_utc();
        queue
            .retry(id, &format!("failure {i}"))
            .await
            .expect("retry");

        let (status, run_after, attempt): (String, OffsetDateTime, i16) =
            sqlx::query("SELECT status::text, run_after, attempt FROM jobs WHERE id = $1")
                .bind(id)
                .fetch_one(&db.pool)
                .await
                .map(|row| (row.get(0), row.get(1), row.get(2)))
                .expect("fetch after retry");

        assert_eq!(attempt as usize, i + 1);
        let delta_minutes = (run_after - before).as_seconds_f64() / 60.0;
        assert!(
            (delta_minutes - expected).abs() < 0.05,
            "retry {i}: expected ~{expected} minutes of backoff, got {delta_minutes}"
        );

        if i < 4 {
            assert_eq!(status, "queued", "retry {i} should still be queued");
        } else {
            assert_eq!(status, "failed", "the 5th retry must fail for good");
        }
    }
}

async fn age_claim(db: &TestDb, id: uuid::Uuid, minutes: i32) {
    sqlx::query("UPDATE jobs SET claimed_at = now() - make_interval(mins => $2) WHERE id = $1")
        .bind(id)
        .bind(minutes)
        .execute(&db.pool)
        .await
        .expect("age the claim");
}

async fn job_state(db: &TestDb, id: uuid::Uuid) -> (String, i16, Option<String>) {
    sqlx::query("SELECT status::text, attempt, claimed_by FROM jobs WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .expect("fetch job")
}

#[tokio::test]
async fn requeue_stale_moves_old_running_jobs_back_and_counts_an_attempt() {
    let db = TestDb::new().await;
    let queue = JobQueue::new(db.pool.clone());
    let stale = queue.enqueue(JobKind::SendEmail, json!({})).await.unwrap();
    let fresh = queue.enqueue(JobKind::SendEmail, json!({})).await.unwrap();
    let waiting = queue.enqueue(JobKind::Cleanup, json!({})).await.unwrap();
    queue.claim("w").await.unwrap().unwrap();
    queue.claim("w").await.unwrap().unwrap();
    age_claim(&db, stale, 11).await;
    age_claim(&db, fresh, 9).await;

    let moved = queue
        .requeue_stale(std::time::Duration::from_secs(600))
        .await
        .expect("requeue");
    assert_eq!(moved, 1);

    let (status, attempt, claimed_by) = job_state(&db, stale).await;
    assert_eq!((status.as_str(), attempt, claimed_by), ("queued", 1, None));
    assert_eq!(
        job_state(&db, fresh).await.0,
        "running",
        "9 minutes is not stale"
    );
    assert_eq!(job_state(&db, waiting).await.0, "queued");
    let run_after: OffsetDateTime = sqlx::query_scalar("SELECT run_after FROM jobs WHERE id = $1")
        .bind(stale)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(
        run_after > OffsetDateTime::now_utc(),
        "it backs off like a failed attempt"
    );
    let error: Option<String> = sqlx::query_scalar("SELECT last_error FROM jobs WHERE id = $1")
        .bind(stale)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(error.unwrap().contains("stopped"));
}

#[tokio::test]
async fn a_job_that_always_kills_its_worker_ends_failed() {
    let db = TestDb::new().await;
    let queue = JobQueue::new(db.pool.clone());
    let id = queue.enqueue(JobKind::SendEmail, json!({})).await.unwrap();
    for round in 1..=5 {
        sqlx::query("UPDATE jobs SET run_after = now() WHERE id = $1")
            .bind(id)
            .execute(&db.pool)
            .await
            .unwrap();
        queue.claim("doomed").await.unwrap().expect("claimable");
        age_claim(&db, id, 11).await;
        let moved = queue
            .requeue_stale(std::time::Duration::from_secs(600))
            .await
            .unwrap();
        assert_eq!(moved, 1, "round {round}");
    }
    let (status, attempt, _) = job_state(&db, id).await;
    assert_eq!((status.as_str(), attempt), ("failed", 5));
    assert!(queue.claim("w").await.unwrap().is_none());
}
