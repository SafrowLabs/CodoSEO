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
