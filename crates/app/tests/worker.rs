//! End-to-end worker tests against a real throwaway Postgres database and a `codoseo_testkit`
//! fixture HTTP server.

mod support;

use codoseo::worker::{DEFAULT_MEMORY_BUDGET, address_policy_for_mode, worker_loop_once};
use codoseo_core::crawl::AddressPolicy;
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

    let claimed = worker_loop_once(
        &db.pool,
        &crawl_queue,
        &job_queue,
        "test-worker",
        DEFAULT_MEMORY_BUDGET,
        AddressPolicy::AllowPrivate,
    )
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
    let claimed_again = worker_loop_once(
        &db.pool,
        &crawl_queue,
        &job_queue,
        "test-worker",
        DEFAULT_MEMORY_BUDGET,
        AddressPolicy::AllowPrivate,
    )
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

    let claimed = worker_loop_once(
        &db.pool,
        &crawl_queue,
        &job_queue,
        "test-worker",
        DEFAULT_MEMORY_BUDGET,
        AddressPolicy::AllowPrivate,
    )
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

    let claimed = worker_loop_once(
        &db.pool,
        &crawl_queue,
        &job_queue,
        "test-worker",
        DEFAULT_MEMORY_BUDGET,
        AddressPolicy::AllowPrivate,
    )
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

#[tokio::test]
async fn a_crawl_that_exceeds_the_memory_budget_is_not_run() {
    let db = TestDb::new().await;
    let crawl_queue = CrawlQueue::new(db.pool.clone());
    let job_queue = JobQueue::new(db.pool.clone());

    // No account -> `PlanLimits::quick_audit()` (max_pages = 100). A 1-byte budget can't fit
    // even one page (1536 bytes/page), so the crawl must be rejected before it ever runs.
    let site_id = db.seed_site("toobig.test", "http://127.0.0.1:1/").await;
    crawl_queue
        .enqueue(site_id, "toobig.test", CrawlTrigger::Manual, 2, None, None)
        .await
        .expect("enqueue crawl");

    let claimed = worker_loop_once(
        &db.pool,
        &crawl_queue,
        &job_queue,
        "test-worker",
        1,
        AddressPolicy::AllowPrivate,
    )
    .await
    .expect("worker_loop_once must not itself error on a budget rejection");
    assert!(claimed);

    let (status, failure_reason): (String, Option<String>) =
        sqlx::query_as("SELECT status::text, failure_reason FROM crawls WHERE site_id = $1")
            .bind(site_id)
            .fetch_one(&db.pool)
            .await
            .expect("read crawl row");
    assert_eq!(
        status, "queued",
        "a budget rejection is a normal first failure, not a crash"
    );
    assert!(
        failure_reason
            .as_deref()
            .unwrap_or_default()
            .contains("memory budget"),
        "failure_reason should explain the rejection, got {failure_reason:?}"
    );

    let page_count: i64 = sqlx::query_scalar("SELECT count(*) FROM pages WHERE site_id = $1")
        .bind(site_id)
        .fetch_one(&db.pool)
        .await
        .expect("count pages");
    assert_eq!(page_count, 0, "a rejected crawl must never have run");
}

#[tokio::test]
async fn resolve_limits_uses_the_sites_account_plan_not_the_free_default() {
    let db = TestDb::new().await;

    let account_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO accounts (email, plan) VALUES ('pro@example.com', 'pro') RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .expect("insert pro account");
    let site_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO sites (account_id, domain, start_url) \
         VALUES ($1, 'pro.example', 'https://pro.example/') RETURNING id",
    )
    .bind(account_id)
    .fetch_one(&db.pool)
    .await
    .expect("insert site under the pro account");

    let limits = codoseo::worker::resolve_limits(&db.pool, site_id, &serde_json::json!({}))
        .await
        .expect("resolve_limits");
    assert_eq!(
        limits.max_pages,
        Some(10_000),
        "a Pro-plan site must use the Pro plan's page cap, not the Free-shaped default"
    );

    // An unclaimed site (no account) must fall back to the quick-audit limits, not Pro's.
    let unclaimed_site = db
        .seed_site("unclaimed.example", "https://unclaimed.example/")
        .await;
    let quick_limits =
        codoseo::worker::resolve_limits(&db.pool, unclaimed_site, &serde_json::json!({}))
            .await
            .expect("resolve_limits for an unclaimed site");
    assert_eq!(quick_limits.max_pages, Some(100));

    // crawl_settings may narrow the plan cap but never exceed it.
    let narrowed =
        codoseo::worker::resolve_limits(&db.pool, site_id, &serde_json::json!({ "max_pages": 50 }))
            .await
            .expect("resolve_limits with a narrowing override");
    assert_eq!(narrowed.max_pages, Some(50));

    let ignored_widening = codoseo::worker::resolve_limits(
        &db.pool,
        site_id,
        &serde_json::json!({ "max_pages": 999_999 }),
    )
    .await
    .expect("resolve_limits with a widening override");
    assert_eq!(
        ignored_widening.max_pages,
        Some(10_000),
        "crawl_settings must not be able to exceed the plan's cap"
    );
}

#[tokio::test]
async fn a_cloud_worker_never_connects_to_a_private_address() {
    let db = TestDb::new().await;
    // The fixture site listens on 127.0.0.1, which the cloud must refuse to crawl.
    let site = SiteBuilder::new()
        .html("/", "Home", &["/a"])
        .html("/a", "A", &[])
        .start()
        .await;
    let site_id = db.seed_site("internal.test", site.url("/").as_str()).await;
    let crawl_queue = CrawlQueue::new(db.pool.clone());
    let job_queue = JobQueue::new(db.pool.clone());
    crawl_queue
        .enqueue(
            site_id,
            "internal.test",
            CrawlTrigger::Quick,
            0,
            Some("web"),
            None,
        )
        .await
        .expect("enqueue crawl");

    let claimed = worker_loop_once(
        &db.pool,
        &crawl_queue,
        &job_queue,
        "test-worker",
        DEFAULT_MEMORY_BUDGET,
        AddressPolicy::Public,
    )
    .await
    .expect("worker_loop_once");
    assert!(claimed);

    assert_eq!(
        site.hits(),
        0,
        "the cloud worker must not send a single request to 127.0.0.1"
    );
    let (reason, status, pages): (Option<String>, String, i64) = sqlx::query_as(
        "SELECT c.failure_reason, c.status::text, (SELECT count(*) FROM pages p WHERE p.crawl_id = c.id) \
         FROM crawls c WHERE c.site_id = $1",
    )
    .bind(site_id)
    .fetch_one(&db.pool)
    .await
    .expect("read crawl row");
    // A quick audit fails for good (no 15-minute retry): the visitor is watching it.
    let reason = reason.expect("a refused address records why the crawl failed");
    assert!(
        reason.contains("address not allowed"),
        "unexpected reason: {reason}"
    );
    assert_eq!(status, "failed");
    assert_eq!(pages, 0);
}

#[test]
fn the_worker_policy_follows_codoseo_mode() {
    assert_eq!(
        address_policy_for_mode(Some("cloud")),
        AddressPolicy::Public
    );
    assert_eq!(
        address_policy_for_mode(Some("selfhost")),
        AddressPolicy::AllowPrivate
    );
    assert_eq!(address_policy_for_mode(None), AddressPolicy::AllowPrivate);
}
