//! End-to-end worker tests against a real throwaway Postgres database and a `codoseo_testkit`
//! fixture HTTP server.

mod support;

use codoseo::worker::worker_loop_once;
use codoseo_store::crawl_queue::{CrawlQueue, CrawlTrigger};
use codoseo_store::jobs::JobQueue;
use codoseo_testkit::SiteBuilder;
use serde_json::json;
use support::TestDb;

#[tokio::test]
async fn worker_loop_once_crawls_and_finalizes_a_claimed_site() {
    let db = TestDb::new().await;
    let site = SiteBuilder::new()
        .html("/", "Home", &["/a", "/b"])
        .html("/a", "A", &[])
        .html("/b", "B", &[])
        .start()
        .await;
    let start_url = site.url("/");

    let site_id = db.seed_site("example.test", start_url.as_str()).await;

    let crawl_queue = CrawlQueue::new(db.pool.clone());
    let job_queue = JobQueue::new(db.pool.clone());
    crawl_queue
        .enqueue(site_id, "example.test", CrawlTrigger::Manual, 2, None, None)
        .await
        .expect("enqueue crawl");

    let claimed = worker_loop_once(&db.pool, &crawl_queue, &job_queue, "test-worker")
        .await
        .expect("worker_loop_once");
    assert!(claimed, "a queued crawl should have been claimed");

    let (status, pages_written): (String, i64) = sqlx::query_as(
        "SELECT c.status::text, (SELECT count(*) FROM pages p WHERE p.crawl_id = c.id) \
         FROM crawls c WHERE c.site_id = $1",
    )
    .bind(site_id)
    .fetch_one(&db.pool)
    .await
    .expect("read crawl row");

    assert_eq!(status, "done", "crawl should have finished successfully");
    assert_eq!(pages_written, 3, "all 3 fixture pages should be written");

    let health_score: Option<i16> =
        sqlx::query_scalar("SELECT health_score FROM crawls WHERE site_id = $1")
            .bind(site_id)
            .fetch_one(&db.pool)
            .await
            .expect("read health score");
    assert!(health_score.is_some(), "finalize should set a health score");

    // The queue is now empty: a second call finds nothing to do.
    let claimed_again = worker_loop_once(&db.pool, &crawl_queue, &job_queue, "test-worker")
        .await
        .expect("worker_loop_once");
    assert!(!claimed_again, "the queue should be empty after one crawl");
}

#[tokio::test]
async fn a_panicking_crawl_is_isolated_and_the_loop_keeps_serving_later_crawls() {
    let db = TestDb::new().await;
    let crawl_queue = CrawlQueue::new(db.pool.clone());
    let job_queue = JobQueue::new(db.pool.clone());

    // First site: its crawl_settings carry the test-only panic seam (see
    // `crates/app/src/worker/run.rs::run_one_crawl`), so its crawl panics before doing any
    // real work.
    let panicking_site = db
        .seed_site_with_settings(
            "panics.test",
            "http://127.0.0.1:1/",
            json!({ "test_panic_before_crawl": true }),
        )
        .await;
    crawl_queue
        .enqueue(
            panicking_site,
            "panics.test",
            CrawlTrigger::Manual,
            2,
            None,
            None,
        )
        .await
        .expect("enqueue panicking crawl");

    let claimed = worker_loop_once(&db.pool, &crawl_queue, &job_queue, "test-worker")
        .await
        .expect("worker_loop_once must not itself error on a panicking crawl");
    assert!(claimed);

    let status: String = sqlx::query_scalar("SELECT status::text FROM crawls WHERE site_id = $1")
        .bind(panicking_site)
        .fetch_one(&db.pool)
        .await
        .expect("read crawl row");
    // First failure requeues (attempt 0 -> 1) rather than failing outright — see
    // `CrawlQueue::finish_failed`.
    assert_eq!(
        status, "queued",
        "a panic is treated as a normal first failure"
    );

    // Second site: a real, successful crawl on the same worker loop, proving the panic above
    // didn't take the loop down.
    let site = SiteBuilder::new().html("/", "Home", &[]).start().await;
    let healthy_site = db.seed_site("healthy.test", site.url("/").as_str()).await;
    crawl_queue
        .enqueue(
            healthy_site,
            "healthy.test",
            CrawlTrigger::Manual,
            2,
            None,
            None,
        )
        .await
        .expect("enqueue healthy crawl");

    let claimed = worker_loop_once(&db.pool, &crawl_queue, &job_queue, "test-worker")
        .await
        .expect("worker_loop_once");
    assert!(claimed);

    let status: String = sqlx::query_scalar("SELECT status::text FROM crawls WHERE site_id = $1")
        .bind(healthy_site)
        .fetch_one(&db.pool)
        .await
        .expect("read crawl row");
    assert_eq!(
        status, "done",
        "the loop still serves a later, healthy crawl"
    );
}
