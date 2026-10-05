//! T4.3: one-transaction finalize (pages/inlinks via `COPY`, site_files, changes, the crawl's
//! own done/summary update, 2-crawl retention cleanup, default-rule alert jobs), backed by
//! real Postgres.

mod support;

use codoseo_core::change::{Change, ChangeKind};
use codoseo_core::check::{IssueBits, Severity};
use codoseo_core::crawl::SitemapSummary;
use codoseo_core::output::{CrawlOutput, LinkGraph, StopReason};
use codoseo_core::page::{Indexability, JsonLdStatus, OgTags, PageFields, PageRecord};
use codoseo_core::report::{CrawlReport, CrawlSummary};
use sqlx::Row;
use support::TestDb;
use url::Url;
use uuid::Uuid;

use codoseo_store::crawl_queue::CrawlQueue;
use codoseo_store::finalize::finalize;

async fn make_site(pool: &sqlx::PgPool, domain: &str) -> Uuid {
    sqlx::query("INSERT INTO sites (domain, start_url) VALUES ($1, $2) RETURNING id")
        .bind(domain)
        .bind(format!("https://{domain}/"))
        .fetch_one(pool)
        .await
        .expect("insert site")
        .get(0)
}

/// The `worker_id` every test's `finalize()` call uses, matching the row `make_crawl` sets up
/// as the current owner — `finalize`'s ownership guard needs the two to agree.
const WORKER_ID: &str = "test-worker";

async fn make_crawl(pool: &sqlx::PgPool, site_id: Uuid, domain: &str) -> Uuid {
    sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, worker_id) \
         VALUES ($1, $2, 'manual', 2, 'running', $3) RETURNING id",
    )
    .bind(site_id)
    .bind(domain)
    .bind(WORKER_ID)
    .fetch_one(pool)
    .await
    .expect("insert crawl")
    .get(0)
}

fn sample_page(idx: u64) -> PageRecord {
    PageRecord {
        url: Url::parse(&format!("https://example.com/page{idx}")).unwrap(),
        url_hash: idx,
        status: 200,
        redirect_chain: Vec::new(),
        response_ms: 10,
        size_bytes: 100,
        content_type: Some("text/html".to_string()),
        depth: Some(0),
        in_sitemap: true,
        indexability: Indexability::Indexable,
        fields: PageFields {
            title: Some(format!("Page {idx}")),
            title_count: 1,
            meta_description: Some("desc".to_string()),
            meta_robots: None,
            x_robots_tag: None,
            canonical: None,
            hreflang: Vec::new(),
            h1: vec!["Heading".to_string()],
            h2: Vec::new(),
            word_count: 42,
            content_hash: idx + 1_000_000,
            images_missing_alt: 0,
            og: OgTags::default(),
            jsonld: JsonLdStatus::default(),
            mixed_content: 0,
        },
        inlinks: 0,
        outlinks_internal: 0,
        outlinks_external: 0,
        issues: IssueBits::default(),
        key_hash: idx + 2_000_000,
        redirect_target: None,
        outlinks_nofollow: 0,
        error: None,
    }
}

fn empty_output(pages: Vec<PageRecord>, stop: StopReason) -> CrawlOutput {
    CrawlOutput {
        origin: Url::parse("https://example.com/").unwrap(),
        pages,
        links: LinkGraph::default(),
        robots: None,
        sitemap: SitemapSummary::default(),
        stop,
        duration_ms: 500,
    }
}

fn empty_report() -> CrawlReport {
    CrawlReport {
        health_score: 95,
        checks_passed: 40,
        checks_total: 42,
        counts: Vec::new(),
        inlink_samples: Vec::new(),
        summary: CrawlSummary::default(),
    }
}

#[tokio::test]
async fn finalize_writes_pages_and_marks_the_crawl_done() {
    let db = TestDb::new().await;
    let site_id = make_site(&db.pool, "happy.example").await;
    let crawl_id = make_crawl(&db.pool, site_id, "happy.example").await;

    let out = empty_output(vec![sample_page(1), sample_page(2)], StopReason::Completed);
    let report = empty_report();
    let changes = vec![Change {
        kind: ChangeKind::ErrorSpike,
        severity: Severity::Critical,
        url: Some(Url::parse("https://example.com/page1").unwrap()),
        before: "0 errors".to_string(),
        after: "5 errors".to_string(),
    }];

    finalize(
        &db.pool, crawl_id, site_id, WORKER_ID, &out, &report, &changes,
    )
    .await
    .expect("finalize");

    let (status, health_score): (String, Option<i16>) =
        sqlx::query("SELECT status::text, health_score FROM crawls WHERE id = $1")
            .bind(crawl_id)
            .fetch_one(&db.pool)
            .await
            .map(|row| (row.get(0), row.get(1)))
            .expect("fetch crawl");
    assert_eq!(status, "done");
    assert_eq!(health_score, Some(95));

    let page_count: i64 = sqlx::query_scalar("SELECT count(*) FROM pages WHERE crawl_id = $1")
        .bind(crawl_id)
        .fetch_one(&db.pool)
        .await
        .expect("count pages");
    assert_eq!(page_count, 2);

    let change_count: i64 = sqlx::query_scalar("SELECT count(*) FROM changes WHERE crawl_id = $1")
        .bind(crawl_id)
        .fetch_one(&db.pool)
        .await
        .expect("count changes");
    assert_eq!(change_count, 1);

    // ErrorSpike matches the default instant-alert shape, so a job must be queued for it.
    let job_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = 'send_alert' AND payload->>'crawl_id' = $1",
    )
    .bind(crawl_id.to_string())
    .fetch_one(&db.pool)
    .await
    .expect("count jobs");
    assert_eq!(job_count, 1);
}

#[tokio::test]
async fn finalize_round_trips_the_stop_reason_for_previous_snapshot() {
    let db = TestDb::new().await;
    let site_id = make_site(&db.pool, "stopreason.example").await;
    let crawl_id = make_crawl(&db.pool, site_id, "stopreason.example").await;

    let out = empty_output(
        vec![sample_page(1)],
        StopReason::Blocked("site blocked our crawler".to_string()),
    );
    finalize(
        &db.pool,
        crawl_id,
        site_id,
        WORKER_ID,
        &out,
        &empty_report(),
        &[],
    )
    .await
    .expect("finalize");

    let queue = CrawlQueue::new(db.pool.clone());
    let snapshot = queue
        .previous_snapshot(site_id)
        .await
        .expect("query")
        .expect("a done crawl exists");
    assert_eq!(
        snapshot.stop,
        StopReason::Blocked("site blocked our crawler".to_string())
    );
}

#[tokio::test]
async fn finalize_is_atomic_when_a_change_fails_to_insert() {
    let db = TestDb::new().await;
    let site_id = make_site(&db.pool, "atomic.example").await;
    let crawl_id = make_crawl(&db.pool, site_id, "atomic.example").await;

    let out = empty_output(vec![sample_page(1), sample_page(2)], StopReason::Completed);
    // Postgres TEXT columns reject an embedded NUL byte outright, so this change insert fails
    // partway through the transaction, after pages/inlinks/site_files have already been copied.
    let changes = vec![Change {
        kind: ChangeKind::ErrorSpike,
        severity: Severity::Critical,
        url: None,
        before: "bad\u{0}value".to_string(),
        after: "after".to_string(),
    }];

    let result = finalize(
        &db.pool,
        crawl_id,
        site_id,
        WORKER_ID,
        &out,
        &empty_report(),
        &changes,
    )
    .await;
    assert!(result.is_err(), "the NUL byte must make finalize fail");

    for (table, query) in [
        ("pages", "SELECT count(*) FROM pages WHERE crawl_id = $1"),
        (
            "inlinks",
            "SELECT count(*) FROM inlinks WHERE crawl_id = $1",
        ),
        (
            "site_files",
            "SELECT count(*) FROM site_files WHERE crawl_id = $1",
        ),
        (
            "changes",
            "SELECT count(*) FROM changes WHERE crawl_id = $1",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(query)
            .bind(crawl_id)
            .fetch_one(&db.pool)
            .await
            .unwrap_or_else(|e| panic!("count {table}: {e}"));
        assert_eq!(
            count, 0,
            "{table} must be empty after a rolled-back finalize"
        );
    }

    let job_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE payload->>'crawl_id' = $1")
            .bind(crawl_id.to_string())
            .fetch_one(&db.pool)
            .await
            .expect("count jobs");
    assert_eq!(job_count, 0);

    let status: String = sqlx::query_scalar("SELECT status::text FROM crawls WHERE id = $1")
        .bind(crawl_id)
        .fetch_one(&db.pool)
        .await
        .expect("fetch crawl status");
    assert_eq!(status, "running", "the crawl row itself must be untouched");
}

#[tokio::test]
async fn finalize_keeps_only_the_latest_two_done_crawls_pages() {
    let db = TestDb::new().await;
    let site_id = make_site(&db.pool, "retention.example").await;

    // Seed 3 prior `done` crawls, each with one page, oldest first.
    let mut old_crawl_ids = Vec::new();
    for i in 0..3 {
        let crawl_id: Uuid = sqlx::query(
            "INSERT INTO crawls (site_id, domain, trigger, priority, status, finished_at) \
             VALUES ($1, $2, 'manual', 2, 'done', now() - ($3 || ' hours')::interval) \
             RETURNING id",
        )
        .bind(site_id)
        .bind("retention.example")
        .bind((10 - i).to_string())
        .fetch_one(&db.pool)
        .await
        .expect("insert seeded crawl")
        .get(0);
        sqlx::query(
            "INSERT INTO pages (crawl_id, site_id, url, url_hash, status, indexability) \
             VALUES ($1, $2, 'https://retention.example/', 1, 200, 'indexable')",
        )
        .bind(crawl_id)
        .bind(site_id)
        .execute(&db.pool)
        .await
        .expect("insert seeded page");
        old_crawl_ids.push(crawl_id);
    }

    let current_crawl_id = make_crawl(&db.pool, site_id, "retention.example").await;
    let out = empty_output(vec![sample_page(99)], StopReason::Completed);
    finalize(
        &db.pool,
        current_crawl_id,
        site_id,
        WORKER_ID,
        &out,
        &empty_report(),
        &[],
    )
    .await
    .expect("finalize");

    // Oldest 2 of the 4 total `done` crawls (seeded crawl 0 and 1) must have lost their pages;
    // the newest seeded one (crawl 2) and the one just finalized must keep theirs.
    let remaining: Vec<Uuid> = sqlx::query_scalar("SELECT DISTINCT crawl_id FROM pages")
        .fetch_all(&db.pool)
        .await
        .expect("remaining pages");
    assert!(!remaining.contains(&old_crawl_ids[0]));
    assert!(!remaining.contains(&old_crawl_ids[1]));
    assert!(remaining.contains(&old_crawl_ids[2]));
    assert!(remaining.contains(&current_crawl_id));
    assert_eq!(remaining.len(), 2);
}

#[tokio::test]
async fn finalize_handles_a_synthetic_fifty_thousand_page_crawl() {
    let db = TestDb::new().await;
    let site_id = make_site(&db.pool, "big.example").await;
    let crawl_id = make_crawl(&db.pool, site_id, "big.example").await;

    let pages: Vec<PageRecord> = (0..50_000u64).map(sample_page).collect();
    let out = empty_output(pages, StopReason::PageLimit);
    finalize(
        &db.pool,
        crawl_id,
        site_id,
        WORKER_ID,
        &out,
        &empty_report(),
        &[],
    )
    .await
    .expect("finalize 50k pages");

    let page_count: i64 = sqlx::query_scalar("SELECT count(*) FROM pages WHERE crawl_id = $1")
        .bind(crawl_id)
        .fetch_one(&db.pool)
        .await
        .expect("count pages");
    assert_eq!(page_count, 50_000);
}

#[tokio::test]
async fn finalize_by_a_stale_worker_rolls_back_and_does_not_touch_the_new_owners_row() {
    let db = TestDb::new().await;
    let site_id = make_site(&db.pool, "stale-finalize.example").await;
    let crawl_id = make_crawl(&db.pool, site_id, "stale-finalize.example").await;

    // Simulate requeue_stale + a reclaim by another worker: the row is now owned by
    // "worker-b", not the `WORKER_ID` ("test-worker") that originally claimed it and is
    // about to (stale-ly) call finalize.
    sqlx::query("UPDATE crawls SET worker_id = 'worker-b' WHERE id = $1")
        .bind(crawl_id)
        .execute(&db.pool)
        .await
        .expect("simulate a reclaim by worker-b");

    let out = empty_output(vec![sample_page(1)], StopReason::Completed);
    let result = finalize(
        &db.pool,
        crawl_id,
        site_id,
        WORKER_ID,
        &out,
        &empty_report(),
        &[],
    )
    .await;
    assert!(
        result.is_err(),
        "finalize must refuse to commit for a worker_id that no longer owns the row"
    );

    let (status, worker_id): (String, Option<String>) =
        sqlx::query("SELECT status::text, worker_id FROM crawls WHERE id = $1")
            .bind(crawl_id)
            .fetch_one(&db.pool)
            .await
            .map(|row| (row.get(0), row.get(1)))
            .expect("fetch crawl");
    assert_eq!(status, "running", "worker-b's row must still be running");
    assert_eq!(worker_id, Some("worker-b".to_string()));

    let page_count: i64 = sqlx::query_scalar("SELECT count(*) FROM pages WHERE crawl_id = $1")
        .bind(crawl_id)
        .fetch_one(&db.pool)
        .await
        .expect("count pages");
    assert_eq!(
        page_count, 0,
        "the rolled-back transaction must not leave pages behind"
    );
}

async fn make_crawl_with_trigger(
    pool: &sqlx::PgPool,
    site_id: Uuid,
    domain: &str,
    trigger: &str,
) -> Uuid {
    // First crawls in these tests come from an unlocked audit, like the ones the funnel counts.
    let source = (trigger == "first").then_some("audit");
    sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, worker_id, source) \
         VALUES ($1, $2, $3::crawl_trigger, 1, 'running', $4, $5) RETURNING id",
    )
    .bind(site_id)
    .bind(domain)
    .bind(trigger)
    .bind(WORKER_ID)
    .bind(source)
    .fetch_one(pool)
    .await
    .expect("insert crawl")
    .get(0)
}

#[tokio::test]
async fn finalize_records_the_funnel_events_for_quick_and_first_crawls_only() {
    let db = TestDb::new().await;
    for (trigger, expected) in [
        ("quick", Some("audit_finished")),
        ("first", Some("first_full_crawl")),
        ("manual", None),
        ("schedule", None),
    ] {
        let domain = format!("{trigger}.example");
        let site_id = make_site(&db.pool, &domain).await;
        let crawl_id = make_crawl_with_trigger(&db.pool, site_id, &domain, trigger).await;
        let out = empty_output(vec![sample_page(1)], StopReason::Completed);
        finalize(
            &db.pool,
            crawl_id,
            site_id,
            WORKER_ID,
            &out,
            &empty_report(),
            &[],
        )
        .await
        .expect("finalize");

        let events: Vec<(String, Option<Uuid>, serde_json::Value)> = sqlx::query_as(
            "SELECT kind, site_id, payload FROM events WHERE site_id = $1 ORDER BY id",
        )
        .bind(site_id)
        .fetch_all(&db.pool)
        .await
        .expect("events");
        match expected {
            Some(kind) => {
                assert_eq!(events.len(), 1, "{trigger}: {events:?}");
                assert_eq!(events[0].0, kind);
                assert_eq!(events[0].2["crawl_id"], crawl_id.to_string());
                assert_eq!(events[0].2["score"], 95);
            }
            None => assert!(events.is_empty(), "{trigger}: {events:?}"),
        }
    }
}

#[tokio::test]
async fn a_failed_finalize_leaves_no_funnel_event() {
    let db = TestDb::new().await;
    let site_id = make_site(&db.pool, "stolen.example").await;
    let crawl_id = make_crawl_with_trigger(&db.pool, site_id, "stolen.example", "quick").await;
    let out = empty_output(vec![sample_page(1)], StopReason::Completed);
    // Another worker owns the crawl, so finalize rolls back.
    let result = finalize(
        &db.pool,
        crawl_id,
        site_id,
        "someone-else",
        &out,
        &empty_report(),
        &[],
    )
    .await;
    assert!(result.is_err());
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(events, 0);
}

#[tokio::test]
async fn a_first_crawl_added_directly_is_not_a_funnel_step() {
    let db = TestDb::new().await;
    let site_id = make_site(&db.pool, "direct.example").await;
    // `source` is NULL for a site added through the add-site form.
    let crawl_id: Uuid = sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, worker_id) \
         VALUES ($1, 'direct.example', 'first', 1, 'running', $2) RETURNING id",
    )
    .bind(site_id)
    .bind(WORKER_ID)
    .fetch_one(&db.pool)
    .await
    .unwrap()
    .get(0);
    finalize(
        &db.pool,
        crawl_id,
        site_id,
        WORKER_ID,
        &empty_output(vec![sample_page(1)], StopReason::Completed),
        &empty_report(),
        &[],
    )
    .await
    .expect("finalize");
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(events, 0);
}

#[tokio::test]
async fn the_diff_baseline_skips_quick_audits() {
    let db = TestDb::new().await;
    let queue = CrawlQueue::new(db.pool.clone());
    let site_id = make_site(&db.pool, "baseline.example").await;

    // A finished 100-page quick audit is not what the 500-page first crawl is compared with.
    let quick = make_crawl_with_trigger(&db.pool, site_id, "baseline.example", "quick").await;
    finalize(
        &db.pool,
        quick,
        site_id,
        WORKER_ID,
        &empty_output(vec![sample_page(1)], StopReason::PageLimit),
        &empty_report(),
        &[],
    )
    .await
    .expect("finalize quick");
    assert!(queue.previous_snapshot(site_id).await.unwrap().is_none());

    // The first full crawl is.
    let first = make_crawl_with_trigger(&db.pool, site_id, "baseline.example", "first").await;
    finalize(
        &db.pool,
        first,
        site_id,
        WORKER_ID,
        &empty_output(vec![sample_page(1), sample_page(2)], StopReason::Completed),
        &empty_report(),
        &[],
    )
    .await
    .expect("finalize first");
    let snapshot = queue
        .previous_snapshot(site_id)
        .await
        .unwrap()
        .expect("a baseline");
    assert_eq!(snapshot.pages.len(), 2);
}
