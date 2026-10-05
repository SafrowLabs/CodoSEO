//! The worker's job runner against a real throwaway Postgres database: `send_email` and
//! `cleanup` run, bad payloads and panics fail only their own job, and the loop keeps going.

#[allow(dead_code)]
mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use codoseo::jobs::{JobContext, job_loop};
use codoseo_core::crawl::AddressPolicy;
use codoseo_notify::{ChannelKey, Email, GuardedHttp, Mailer};
use codoseo_store::jobs::{JobKind, JobQueue};
use serde_json::json;
use sqlx::PgPool;
use support::TestDb;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn context(pool: &PgPool) -> (JobContext, Arc<Mutex<Vec<Email>>>) {
    let (mailer, sent) = Mailer::capture();
    let ctx = JobContext {
        pool: pool.clone(),
        mailer,
        channel_key: ChannelKey::derive("test secret"),
        base_url: url::Url::parse("http://localhost:8080").unwrap(),
        http: GuardedHttp::new(AddressPolicy::AllowPrivate).unwrap(),
        rankorg_url: None,
    };
    (ctx, sent)
}

/// Runs `job_loop` in the background until the returned guard is stopped.
struct RunningLoop {
    shutdown: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

impl RunningLoop {
    fn start(ctx: JobContext, pool: &PgPool) -> RunningLoop {
        let shutdown = CancellationToken::new();
        let handle = tokio::spawn(job_loop(
            ctx,
            JobQueue::new(pool.clone()),
            "test-jobs".to_owned(),
            shutdown.clone(),
        ));
        RunningLoop { shutdown, handle }
    }

    /// Cancels the loop and checks it exits cleanly (a panic would surface here).
    async fn stop(self) {
        self.shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), self.handle)
            .await
            .expect("the loop stops on shutdown")
            .expect("the loop itself never panics");
    }
}

async fn job_status(pool: &PgPool, id: Uuid) -> String {
    sqlx::query_scalar("SELECT status::text FROM jobs WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read job")
}

/// Waits until the job leaves `queued`/`running`, or until it has been retried once.
async fn wait_settled(pool: &PgPool, id: Uuid) {
    for _ in 0..100 {
        let (status, attempt): (String, i16) =
            sqlx::query_as("SELECT status::text, attempt FROM jobs WHERE id = $1")
                .bind(id)
                .fetch_one(pool)
                .await
                .expect("read job");
        if status == "done" || status == "failed" || attempt > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("job {id} did not settle");
}

#[tokio::test]
async fn a_send_email_job_is_sent_and_marked_done() {
    let db = TestDb::new().await;
    let (ctx, sent) = context(&db.pool);
    let queue = JobQueue::new(db.pool.clone());
    let id = queue
        .enqueue(
            JobKind::SendEmail,
            json!({"to": "a@example.com", "subject": "Hello", "text": "plain", "html": "<p>html</p>"}),
        )
        .await
        .unwrap();

    let running = RunningLoop::start(ctx, &db.pool);
    wait_settled(&db.pool, id).await;
    running.stop().await;

    assert_eq!(job_status(&db.pool, id).await, "done");
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "a@example.com");
    assert_eq!(sent[0].subject, "Hello");
    assert_eq!(sent[0].text, "plain");
    assert_eq!(sent[0].html.as_deref(), Some("<p>html</p>"));
}

#[tokio::test]
async fn html_is_optional_in_a_send_email_payload() {
    let db = TestDb::new().await;
    let (ctx, sent) = context(&db.pool);
    let queue = JobQueue::new(db.pool.clone());
    let id = queue
        .enqueue(
            JobKind::SendEmail,
            json!({"to": "a@example.com", "subject": "S", "text": "T"}),
        )
        .await
        .unwrap();
    let running = RunningLoop::start(ctx, &db.pool);
    wait_settled(&db.pool, id).await;
    running.stop().await;
    assert_eq!(job_status(&db.pool, id).await, "done");
    assert_eq!(sent.lock().unwrap()[0].html, None);
}

#[tokio::test]
async fn a_cleanup_job_is_marked_done() {
    let db = TestDb::new().await;
    let (ctx, _sent) = context(&db.pool);
    let queue = JobQueue::new(db.pool.clone());
    let id = queue.enqueue(JobKind::Cleanup, json!({})).await.unwrap();
    let running = RunningLoop::start(ctx, &db.pool);
    wait_settled(&db.pool, id).await;
    running.stop().await;
    assert_eq!(job_status(&db.pool, id).await, "done");
}

#[tokio::test]
async fn a_bad_payload_fails_with_a_readable_error_and_is_retried() {
    let db = TestDb::new().await;
    let (ctx, sent) = context(&db.pool);
    let queue = JobQueue::new(db.pool.clone());
    let id = queue
        .enqueue(JobKind::SendEmail, json!({"to": "a@example.com"}))
        .await
        .unwrap();

    let running = RunningLoop::start(ctx, &db.pool);
    wait_settled(&db.pool, id).await;
    running.stop().await;

    let (status, attempt, in_future, error): (String, i16, bool, Option<String>) = sqlx::query_as(
        "SELECT status::text, attempt, run_after > now(), last_error FROM jobs WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(status, "queued");
    assert_eq!(attempt, 1);
    assert!(in_future, "the retry is backed off");
    let error = error.expect("last_error is set");
    assert!(error.contains("send_email"), "{error}");
    assert!(
        error.contains("subject"),
        "names the missing field: {error}"
    );
    assert!(sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_handler_panic_fails_only_that_job_and_the_loop_keeps_running() {
    let db = TestDb::new().await;
    let (ctx, sent) = context(&db.pool);
    let queue = JobQueue::new(db.pool.clone());
    let panicking = queue
        .enqueue(
            JobKind::SendEmail,
            json!({"to": "a@example.com", "subject": "S", "text": "T", "test_panic_before_job": true}),
        )
        .await
        .unwrap();
    let after = queue
        .enqueue(
            JobKind::SendEmail,
            json!({"to": "b@example.com", "subject": "S2", "text": "T2"}),
        )
        .await
        .unwrap();

    let running = RunningLoop::start(ctx, &db.pool);
    wait_settled(&db.pool, panicking).await;
    wait_settled(&db.pool, after).await;
    running.stop().await;

    let (status, attempt, error): (String, i16, Option<String>) =
        sqlx::query_as("SELECT status::text, attempt, last_error FROM jobs WHERE id = $1")
            .bind(panicking)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!((status.as_str(), attempt), ("queued", 1));
    assert!(error.unwrap().contains("panicked"));
    assert_eq!(job_status(&db.pool, after).await, "done");
    assert_eq!(sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn the_loop_stops_promptly_when_idle() {
    let db = TestDb::new().await;
    let (ctx, _sent) = context(&db.pool);
    let running = RunningLoop::start(ctx, &db.pool);
    tokio::time::sleep(Duration::from_millis(200)).await;
    running.stop().await;
}
