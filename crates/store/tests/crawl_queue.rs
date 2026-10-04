//! T4.2: enqueue/claim/heartbeat/requeue_stale/previous_snapshot/finish_failed, backed by
//! real Postgres so `SKIP LOCKED` races are tested for real, not simulated.

mod support;

use std::collections::HashSet;
use std::time::Duration as StdDuration;

use codoseo_core::output::Progress;
use sqlx::Row;
use support::TestDb;
use time::OffsetDateTime;
use uuid::Uuid;

use codoseo_store::crawl_queue::{CrawlQueue, CrawlTrigger};

async fn make_site(pool: &sqlx::PgPool, domain: &str) -> Uuid {
    sqlx::query("INSERT INTO sites (domain, start_url) VALUES ($1, $2) RETURNING id")
        .bind(domain)
        .bind(format!("https://{domain}/"))
        .fetch_one(pool)
        .await
        .expect("insert site")
        .get(0)
}

#[tokio::test]
async fn claims_each_of_many_rows_exactly_once_under_concurrency() {
    let db = TestDb::new().await;
    let queue = CrawlQueue::new(db.pool.clone());

    let mut expected_ids = HashSet::new();
    for i in 0..10 {
        let domain = format!("site{i}.example");
        let site_id = make_site(&db.pool, &domain).await;
        let id = queue
            .enqueue(site_id, &domain, CrawlTrigger::Manual, 2, None, None)
            .await
            .expect("enqueue");
        expected_ids.insert(id);
    }

    // 20 concurrent claimers against 10 queued rows: each row must be claimed exactly once,
    // and the other 10 claimers must cleanly see `None`.
    let mut handles = Vec::new();
    for i in 0..20 {
        let queue = CrawlQueue::new(db.pool.clone());
        handles.push(tokio::spawn(async move {
            queue.claim(&format!("worker-{i}")).await.expect("claim")
        }));
    }
    let mut claimed_ids = HashSet::new();
    for h in handles {
        if let Some(claimed) = h.await.expect("task join") {
            assert!(
                claimed_ids.insert(claimed.id),
                "row {} was claimed more than once",
                claimed.id
            );
        }
    }
    assert_eq!(
        claimed_ids, expected_ids,
        "every queued row claimed exactly once"
    );
}

#[tokio::test]
async fn second_claim_on_same_domain_waits_for_the_first_to_finish() {
    let db = TestDb::new().await;
    let queue = CrawlQueue::new(db.pool.clone());
    let site_id = make_site(&db.pool, "same-domain.example").await;

    queue
        .enqueue(
            site_id,
            "same-domain.example",
            CrawlTrigger::Manual,
            2,
            None,
            None,
        )
        .await
        .expect("enqueue first");
    queue
        .enqueue(
            site_id,
            "same-domain.example",
            CrawlTrigger::Manual,
            2,
            None,
            None,
        )
        .await
        .expect("enqueue second");

    let first = queue.claim("worker-a").await.expect("claim first");
    assert!(first.is_some(), "first crawl on the domain is claimable");

    let second = queue.claim("worker-b").await.expect("claim second");
    assert!(
        second.is_none(),
        "a second crawl on the same domain must not be claimable while one is running"
    );
}

#[tokio::test]
async fn aging_lets_an_older_lower_priority_crawl_overtake_a_newer_higher_one() {
    let db = TestDb::new().await;
    let queue = CrawlQueue::new(db.pool.clone());

    let old_low_priority_site = make_site(&db.pool, "old-low.example").await;
    let fresh_high_priority_site = make_site(&db.pool, "fresh-high.example").await;
    let other_site = make_site(&db.pool, "other.example").await;

    // priority 5 (lowest lane) queued 4 hours ago: effective priority ages to roughly 1.
    let old_low = queue
        .enqueue(
            old_low_priority_site,
            "old-low.example",
            CrawlTrigger::Manual,
            5,
            None,
            None,
        )
        .await
        .expect("enqueue old low-priority");
    sqlx::query("UPDATE crawls SET queued_at = now() - interval '4 hours' WHERE id = $1")
        .bind(old_low)
        .execute(&db.pool)
        .await
        .expect("backdate old crawl");

    // priority 2, queued just now: effective priority stays 2, which is still worse (higher
    // number = lower urgency) than the aged ~1 of the older low-priority crawl above.
    let fresh_high = queue
        .enqueue(
            fresh_high_priority_site,
            "fresh-high.example",
            CrawlTrigger::Manual,
            2,
            None,
            None,
        )
        .await
        .expect("enqueue fresh high-priority");

    // A third, unrelated row on its own domain so the claim always has something to pick.
    let _ = queue
        .enqueue(
            other_site,
            "other.example",
            CrawlTrigger::Manual,
            5,
            None,
            None,
        )
        .await
        .expect("enqueue other");
    sqlx::query("UPDATE crawls SET queued_at = now() - interval '1 minute' WHERE id = $1")
        .bind(fresh_high)
        .execute(&db.pool)
        .await
        .expect("set fresh crawl queued_at just now");

    let claimed = queue
        .claim("worker-a")
        .await
        .expect("claim")
        .expect("a row is claimable");
    assert_eq!(
        claimed.id, old_low,
        "the aged older low-priority crawl must be claimed before the fresher higher-priority one"
    );
}

#[tokio::test]
async fn requeue_stale_moves_only_crawls_with_an_old_heartbeat() {
    let db = TestDb::new().await;
    let queue = CrawlQueue::new(db.pool.clone());
    let site_id = make_site(&db.pool, "stale.example").await;

    let stale = queue
        .enqueue(
            site_id,
            "stale.example",
            CrawlTrigger::Manual,
            2,
            None,
            None,
        )
        .await
        .expect("enqueue stale");
    sqlx::query(
        "UPDATE crawls SET status = 'running', heartbeat_at = now() - interval '2 minutes', worker_id = 'dead-worker' WHERE id = $1",
    )
    .bind(stale)
    .execute(&db.pool)
    .await
    .expect("mark stale running");

    let fresh_site = make_site(&db.pool, "fresh.example").await;
    let fresh = queue
        .enqueue(
            fresh_site,
            "fresh.example",
            CrawlTrigger::Manual,
            2,
            None,
            None,
        )
        .await
        .expect("enqueue fresh");
    sqlx::query(
        "UPDATE crawls SET status = 'running', heartbeat_at = now(), worker_id = 'alive-worker' WHERE id = $1",
    )
    .bind(fresh)
    .execute(&db.pool)
    .await
    .expect("mark fresh running");

    let moved = queue
        .requeue_stale(StdDuration::from_secs(60))
        .await
        .expect("requeue_stale");
    assert_eq!(moved, 1);

    let stale_status: String = sqlx::query("SELECT status::text FROM crawls WHERE id = $1")
        .bind(stale)
        .fetch_one(&db.pool)
        .await
        .expect("fetch stale status")
        .get(0);
    assert_eq!(stale_status, "queued");

    let fresh_status: String = sqlx::query("SELECT status::text FROM crawls WHERE id = $1")
        .bind(fresh)
        .fetch_one(&db.pool)
        .await
        .expect("fetch fresh status")
        .get(0);
    assert_eq!(fresh_status, "running");
}

#[tokio::test]
async fn finish_failed_retries_once_then_fails_for_good() {
    let db = TestDb::new().await;
    let queue = CrawlQueue::new(db.pool.clone());
    let site_id = make_site(&db.pool, "flaky.example").await;

    let id = queue
        .enqueue(
            site_id,
            "flaky.example",
            CrawlTrigger::Manual,
            2,
            None,
            None,
        )
        .await
        .expect("enqueue");
    queue
        .claim("worker-a")
        .await
        .expect("claim")
        .expect("claimed");

    queue
        .finish_failed(id, "site unreachable")
        .await
        .expect("first failure");

    let (status, attempt): (String, i16) =
        sqlx::query("SELECT status::text, attempt FROM crawls WHERE id = $1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .map(|row| (row.get(0), row.get(1)))
            .expect("fetch after first failure");
    assert_eq!(status, "queued");
    assert_eq!(attempt, 1);

    // The 15-minute retry gate must be honored: immediately after the first failure, the row
    // is not claimable yet.
    assert!(
        queue
            .claim("worker-b")
            .await
            .expect("claim after first failure")
            .is_none(),
        "retry gate must block an immediate re-claim"
    );

    // Pull the gate back so we can observe the row land in `queued` with attempt = 1 before
    // simulating the second failure (claim would otherwise require waiting 15 real minutes).
    sqlx::query("UPDATE crawls SET queued_at = now() - interval '1 minute' WHERE id = $1")
        .bind(id)
        .execute(&db.pool)
        .await
        .expect("open the retry gate for the test");
    let reclaimed = queue
        .claim("worker-b")
        .await
        .expect("claim")
        .expect("claimable again");
    assert_eq!(reclaimed.id, id);

    queue
        .finish_failed(id, "site unreachable again")
        .await
        .expect("second failure");
    let (status, reason): (String, Option<String>) =
        sqlx::query("SELECT status::text, failure_reason FROM crawls WHERE id = $1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .map(|row| (row.get(0), row.get(1)))
            .expect("fetch after second failure");
    assert_eq!(status, "failed");
    assert_eq!(reason, Some("site unreachable again".to_string()));
}

#[tokio::test]
async fn previous_snapshot_is_none_without_a_done_crawl() {
    let db = TestDb::new().await;
    let queue = CrawlQueue::new(db.pool.clone());
    let site_id = make_site(&db.pool, "no-history.example").await;

    assert!(
        queue
            .previous_snapshot(site_id)
            .await
            .expect("query")
            .is_none()
    );
}

#[tokio::test]
async fn previous_snapshot_rebuilds_pages_from_the_latest_done_crawl() {
    let db = TestDb::new().await;
    let queue = CrawlQueue::new(db.pool.clone());
    let site_id = make_site(&db.pool, "history.example").await;

    let crawl_id: Uuid = sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, finished_at, summary) \
         VALUES ($1, $2, 'manual', 2, 'done', now(), $3) RETURNING id",
    )
    .bind(site_id)
    .bind("history.example")
    .bind(serde_json::json!({ "stop_reason": { "kind": "completed" } }))
    .fetch_one(&db.pool)
    .await
    .expect("insert done crawl")
    .get(0);

    sqlx::query(
        "INSERT INTO pages (crawl_id, site_id, url, url_hash, status, indexability, title, key_hash, issues) \
         VALUES ($1, $2, 'https://history.example/', $3, 200, 'indexable', 'Home', $4, $5)",
    )
    .bind(crawl_id)
    .bind(site_id)
    .bind(codoseo_store::hash::to_db(42))
    .bind(codoseo_store::hash::to_db(1234))
    .bind(codoseo_store::hash::to_db(0))
    .execute(&db.pool)
    .await
    .expect("insert page");

    let snapshot = queue
        .previous_snapshot(site_id)
        .await
        .expect("query")
        .expect("a done crawl exists");
    assert_eq!(snapshot.pages.len(), 1);
    assert_eq!(snapshot.pages[0].url.as_str(), "https://history.example/");
    assert_eq!(snapshot.pages[0].url_hash, 42);
    assert_eq!(snapshot.pages[0].fields.title.as_deref(), Some("Home"));
    assert!(snapshot.stop.is_complete());
}

#[tokio::test]
async fn heartbeat_updates_progress_on_a_running_crawl() {
    let db = TestDb::new().await;
    let queue = CrawlQueue::new(db.pool.clone());
    let site_id = make_site(&db.pool, "progress.example").await;
    let id = queue
        .enqueue(
            site_id,
            "progress.example",
            CrawlTrigger::Manual,
            2,
            None,
            None,
        )
        .await
        .expect("enqueue");
    queue
        .claim("worker-a")
        .await
        .expect("claim")
        .expect("claimed");

    let progress = Progress {
        pages_done: 7,
        queued: 3,
        failures: 0,
        depth: 2,
        elapsed_ms: 1500,
    };
    queue.heartbeat(id, &progress).await.expect("heartbeat");

    let (heartbeat_at, stored): (OffsetDateTime, serde_json::Value) =
        sqlx::query("SELECT heartbeat_at, progress FROM crawls WHERE id = $1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .map(|row| (row.get(0), row.get(1)))
            .expect("fetch after heartbeat");
    assert!(heartbeat_at <= OffsetDateTime::now_utc());
    assert_eq!(stored["pages_done"], 7);
}
