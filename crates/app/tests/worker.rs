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

#[tokio::test]
async fn a_starred_page_counts_as_a_key_page_in_the_diff() {
    use codoseo_testkit::{Page, html_page};

    // A 27-page site crawled twice. `/yyy` and `/zzz` sort last by URL, so they are outside the
    // top 20 by inlinks (all pages have one inlink); only `/zzz` is starred. Both titles change
    // between the crawls, and a key page's change is one severity step higher.
    let db = TestDb::new().await;
    let mut links: Vec<String> = (0..24).map(|i| format!("/p{i:02}")).collect();
    links.extend(["/yyy".to_owned(), "/zzz".to_owned()]);
    let link_refs: Vec<&str> = links.iter().map(String::as_str).collect();
    let changing = |path: &str| {
        Page::sequence(vec![
            Page::html(&html_page(&format!("{path} before"), &[])),
            Page::html(&html_page(&format!("{path} after"), &[])),
        ])
    };
    let mut builder = SiteBuilder::new()
        .html("/", "Home", &link_refs)
        .page("/yyy", changing("yyy"))
        .page("/zzz", changing("zzz"));
    for l in &links[..24] {
        builder = builder.html(l, l, &[]);
    }
    let site = builder.start().await;
    let site_id = db.seed_site("example.test", site.url("/").as_str()).await;
    let starred = codoseo_core::url::url_hash(&site.url("/zzz"));
    sqlx::query("UPDATE sites SET key_pages = $2 WHERE id = $1")
        .bind(site_id)
        .bind(vec![codoseo_store::hash::to_db(starred)])
        .execute(&db.pool)
        .await
        .unwrap();

    let crawl_queue = CrawlQueue::new(db.pool.clone());
    let job_queue = JobQueue::new(db.pool.clone());
    for _ in 0..2 {
        crawl_queue
            .enqueue(site_id, "example.test", CrawlTrigger::Manual, 2, None, None)
            .await
            .unwrap();
        assert!(
            worker_loop_once(
                &db.pool,
                &crawl_queue,
                &job_queue,
                "test-worker",
                DEFAULT_MEMORY_BUDGET,
                AddressPolicy::AllowPrivate,
            )
            .await
            .unwrap()
        );
    }
    let severity = |path: &'static str| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, String>(
                "SELECT severity::text FROM changes WHERE kind = 'title_changed' AND url LIKE $1",
            )
            .bind(format!("%{path}"))
            .fetch_one(&pool)
            .await
            .expect("the title change was recorded")
        }
    };
    assert_eq!(severity("/yyy").await, "notice");
    assert_eq!(severity("/zzz").await, "warning");
}

/// Runs one worker cycle against a fresh queue holding one crawl for `settings`, with a
/// recorder of its own installed, and returns what it rendered. The test runtime is
/// single-threaded, so the crawl task records into the same recorder.
async fn metrics_of_one_cycle(start_url: &str, settings: serde_json::Value) -> String {
    let recorder = codoseo::telemetry::build_recorder();
    let handle = recorder.handle();
    let _guard = metrics::set_default_local_recorder(&recorder);

    let db = TestDb::new().await;
    let site_id = db
        .seed_site_with_settings("metrics.test", start_url, settings)
        .await;
    let crawl_queue = CrawlQueue::new(db.pool.clone());
    let job_queue = JobQueue::new(db.pool.clone());
    crawl_queue
        .enqueue(site_id, "metrics.test", CrawlTrigger::Manual, 2, None, None)
        .await
        .expect("enqueue crawl");
    worker_loop_once(
        &db.pool,
        &crawl_queue,
        &job_queue,
        "test-worker",
        DEFAULT_MEMORY_BUDGET,
        AddressPolicy::AllowPrivate,
    )
    .await
    .expect("worker_loop_once");
    handle.render()
}

#[tokio::test]
async fn a_finished_crawl_is_counted_with_its_pages_wait_and_memory() {
    let site = SiteBuilder::new()
        .html("/", "Home", &["/a", "/b"])
        .html("/a", "A", &[])
        .html("/b", "B", &[])
        .start()
        .await;
    let body = metrics_of_one_cycle(site.url("/").as_str(), json!({})).await;
    for expected in [
        r#"codoseo_crawls_finished_total{outcome="completed"} 1"#,
        "codoseo_pages_crawled_total 3",
        r#"codoseo_crawl_queue_wait_seconds_count{lane="2"} 1"#,
        "codoseo_crawls_running 0",
        "codoseo_worker_memory_reserved_bytes 0",
        "codoseo_worker_memory_budget_bytes ",
    ] {
        assert!(body.contains(expected), "{expected} missing from:\n{body}");
    }
}

#[tokio::test]
async fn a_panicking_crawl_counts_as_failed_and_releases_what_it_held() {
    let body = metrics_of_one_cycle(
        "http://127.0.0.1:1/",
        json!({ "test_panic_before_crawl": true }),
    )
    .await;
    assert!(
        body.contains(r#"codoseo_crawls_finished_total{outcome="failed"} 1"#),
        "{body}"
    );
    assert!(!body.contains("codoseo_crawls_running 1"), "{body}");
}

// ---- AI access (GEO) ---------------------------------------------------------------------

/// What the changing fake site answers: robots.txt, and a `robots` meta on every page.
#[derive(Clone)]
struct Knobs {
    robots_status: u16,
    robots_body: String,
    meta_robots: Option<String>,
}

/// A small site whose robots.txt and page markup the test can change between crawls.
async fn changing_site() -> (
    codoseo_testkit::TestServer,
    std::sync::Arc<std::sync::Mutex<Knobs>>,
) {
    use axum::http::{StatusCode, Uri, header};
    use axum::response::IntoResponse;

    let knobs = std::sync::Arc::new(std::sync::Mutex::new(Knobs {
        robots_status: 200,
        robots_body: "User-agent: *\nAllow: /\n".to_owned(),
        meta_robots: None,
    }));
    let state = knobs.clone();
    let router = axum::Router::new().fallback(axum::routing::any(move |uri: Uri| {
        let k = state.lock().unwrap().clone();
        async move {
            match uri.path() {
                "/robots.txt" => (
                    StatusCode::from_u16(k.robots_status).unwrap(),
                    [(header::CONTENT_TYPE, "text/plain")],
                    k.robots_body,
                )
                    .into_response(),
                "/" | "/a" => {
                    let meta = k
                        .meta_robots
                        .map(|m| format!("<meta name=\"robots\" content=\"{m}\">"))
                        .unwrap_or_default();
                    let links = if uri.path() == "/" {
                        "<a href=\"/a\">A</a>"
                    } else {
                        ""
                    };
                    (
                        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                        format!(
                            "<!doctype html><html lang=\"en\"><head><title>Page {}</title>\
                             <meta name=\"description\" content=\"A page.\">{meta}</head>\
                             <body><h1>Page</h1>{links}</body></html>",
                            uri.path()
                        ),
                    )
                        .into_response()
                }
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }
    }));
    (codoseo_testkit::TestServer::start(router).await, knobs)
}

async fn run_crawl(db: &TestDb, site_id: uuid::Uuid, trigger: CrawlTrigger) -> uuid::Uuid {
    let crawl_queue = CrawlQueue::new(db.pool.clone());
    let job_queue = JobQueue::new(db.pool.clone());
    let id = crawl_queue
        .enqueue(site_id, "example.test", trigger, 2, None, None)
        .await
        .expect("enqueue crawl");
    assert!(
        worker_loop_once(
            &db.pool,
            &crawl_queue,
            &job_queue,
            "test-worker",
            DEFAULT_MEMORY_BUDGET,
            AddressPolicy::AllowPrivate,
        )
        .await
        .expect("worker_loop_once")
    );
    id
}

async fn ai_changes(db: &TestDb, crawl: uuid::Uuid) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT kind::text, severity::text FROM changes \
         WHERE crawl_id = $1 AND kind::text LIKE 'ai_%' ORDER BY id",
    )
    .bind(crawl)
    .fetch_all(&db.pool)
    .await
    .unwrap()
}

async fn alert_planning_jobs(db: &TestDb, crawl: uuid::Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = 'send_alert' AND payload->>'crawl_id' = $1 \
         AND payload->>'channel_id' IS NULL AND payload->>'unreachable' IS NULL",
    )
    .bind(crawl.to_string())
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn ai_access_incidents_open_and_resolve_across_real_crawls() {
    let db = TestDb::new().await;
    let (server, knobs) = changing_site().await;
    let site_id = db.seed_site("example.test", server.url("/").as_str()).await;

    // The first crawl is the baseline: a blocked search bot would open quietly.
    knobs.lock().unwrap().robots_body =
        "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /\n".to_owned();
    let first = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    let open = codoseo_store::geo::open_incidents(&db.pool, site_id)
        .await
        .unwrap();
    assert_eq!(open.len(), 1);
    assert!(open[0].quiet);
    assert!(ai_changes(&db, first).await.is_empty());
    assert_eq!(alert_planning_jobs(&db, first).await, 0);

    // (b) The rule goes: resolved, with a change and an alert job.
    knobs.lock().unwrap().robots_body = "User-agent: *\nAllow: /\n".to_owned();
    let second = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    assert!(
        codoseo_store::geo::open_incidents(&db.pool, site_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        ai_changes(&db, second).await.is_empty(),
        "a quiet incident resolves quietly"
    );

    // (a) The rule comes back: one critical incident, one change, one alert job.
    knobs.lock().unwrap().robots_body =
        "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /\n".to_owned();
    let third = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    let open = codoseo_store::geo::open_incidents(&db.pool, site_id)
        .await
        .unwrap();
    assert_eq!(open.len(), 1);
    assert!(!open[0].quiet);
    assert_eq!(
        ai_changes(&db, third).await,
        vec![("ai_bot_blocked".to_owned(), "critical".to_owned())]
    );
    assert_eq!(alert_planning_jobs(&db, third).await, 1);

    // The rule is removed again: resolved with an `ai_issue_resolved` change.
    knobs.lock().unwrap().robots_body = "User-agent: *\nAllow: /\n".to_owned();
    let fourth = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    assert!(
        codoseo_store::geo::open_incidents(&db.pool, site_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        ai_changes(&db, fourth).await,
        vec![("ai_issue_resolved".to_owned(), "notice".to_owned())]
    );
    assert_eq!(alert_planning_jobs(&db, fourth).await, 1);

    // (d) A site-wide nosnippet is one incident.
    knobs.lock().unwrap().meta_robots = Some("nosnippet".to_owned());
    let fifth = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    let open = codoseo_store::geo::open_incidents(&db.pool, site_id)
        .await
        .unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].subject, "nosnippet");
    assert_eq!(
        ai_changes(&db, fifth).await,
        vec![("ai_answers_restricted".to_owned(), "critical".to_owned())]
    );

    // Only the newest two reports are kept.
    let reports: i64 = sqlx::query_scalar("SELECT count(*) FROM ai_reports WHERE site_id = $1")
        .bind(site_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(reports, 2);
}

#[tokio::test]
async fn a_failing_robots_txt_opens_an_incident_through_the_failed_crawl_path() {
    let db = TestDb::new().await;
    let (server, knobs) = changing_site().await;
    let site_id = db.seed_site("example.test", server.url("/").as_str()).await;
    // A site with an owner, so the unreachable alert has someone to go to.
    let account: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO accounts (email, email_canonical) VALUES ('o@example.com', 'o@example.com') RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE sites SET account_id = $2 WHERE id = $1")
        .bind(site_id)
        .bind(account)
        .execute(&db.pool)
        .await
        .unwrap();

    let baseline = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    assert!(ai_changes(&db, baseline).await.is_empty());

    // (c) robots.txt answers 503: the crawl fails as before and waits for its retry, and
    // nothing is recorded yet: a blip that the retry doesn't see is not worth an alert.
    {
        let mut k = knobs.lock().unwrap();
        k.robots_status = 503;
        k.robots_body = String::new();
    }
    let crawl = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    let (status, reason): (String, Option<String>) =
        sqlx::query_as("SELECT status::text, failure_reason FROM crawls WHERE id = $1")
            .bind(crawl)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(status, "queued", "the first failure waits for its retry");
    assert!(
        reason
            .as_deref()
            .is_some_and(|r| r.contains("robots.txt returned HTTP 503")),
        "{reason:?}"
    );
    assert!(
        codoseo_store::geo::open_incidents(&db.pool, site_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(ai_changes(&db, crawl).await.is_empty());
    assert_eq!(alert_planning_jobs(&db, crawl).await, 0);
    assert_eq!(ai_report_count(&db, site_id).await, 1);

    // The retry fails too: the crawl ends failed, the incident is recorded and announced once,
    // and the generic "couldn't reach your site" alert stays out of the way of the precise one.
    retry(&db).await;
    let status: String = sqlx::query_scalar("SELECT status::text FROM crawls WHERE id = $1")
        .bind(crawl)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(status, "failed");
    let open = codoseo_store::geo::open_incidents(&db.pool, site_id)
        .await
        .unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(
        open[0].kind,
        codoseo_geo::findings::FindingKind::RobotsUnavailable
    );
    assert!(!open[0].quiet);
    assert_eq!(
        ai_changes(&db, crawl).await,
        vec![("ai_bot_blocked".to_owned(), "critical".to_owned())]
    );
    assert_eq!(alert_planning_jobs(&db, crawl).await, 1);
    assert_eq!(unreachable_jobs(&db).await, 0);
    assert_eq!(ai_report_count(&db, site_id).await, 2);
}

/// Lets the crawl waiting for its retry go now, and runs it.
async fn retry(db: &TestDb) {
    sqlx::query("UPDATE crawls SET queued_at = now() WHERE status = 'queued'")
        .execute(&db.pool)
        .await
        .unwrap();
    let crawl_queue = CrawlQueue::new(db.pool.clone());
    let job_queue = JobQueue::new(db.pool.clone());
    assert!(
        worker_loop_once(
            &db.pool,
            &crawl_queue,
            &job_queue,
            "test-worker",
            DEFAULT_MEMORY_BUDGET,
            AddressPolicy::AllowPrivate,
        )
        .await
        .unwrap()
    );
}

async fn unreachable_jobs(db: &TestDb) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = 'send_alert' AND payload->>'unreachable' = 'true'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

async fn ai_report_count(db: &TestDb, site_id: uuid::Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ai_reports WHERE site_id = $1")
        .bind(site_id)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_robots_txt_blip_the_retry_does_not_see_says_nothing() {
    let db = TestDb::new().await;
    let (server, knobs) = changing_site().await;
    let site_id = db.seed_site("example.test", server.url("/").as_str()).await;
    run_crawl(&db, site_id, CrawlTrigger::Manual).await;

    knobs.lock().unwrap().robots_status = 503;
    let crawl = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    knobs.lock().unwrap().robots_status = 200;
    retry(&db).await;
    let status: String = sqlx::query_scalar("SELECT status::text FROM crawls WHERE id = $1")
        .bind(crawl)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(status, "done");
    assert!(ai_changes(&db, crawl).await.is_empty());
    let incidents: i64 = sqlx::query_scalar("SELECT count(*) FROM ai_incidents WHERE site_id = $1")
        .bind(site_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(incidents, 0, "never opened, so never resolved either");
}

#[tokio::test]
async fn a_quick_audit_has_no_ai_access_state() {
    let db = TestDb::new().await;
    let (server, knobs) = changing_site().await;
    knobs.lock().unwrap().robots_body =
        "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /\n".to_owned();
    let site_id = db.seed_site("example.test", server.url("/").as_str()).await;
    run_crawl(&db, site_id, CrawlTrigger::Quick).await;
    let reports: i64 = sqlx::query_scalar("SELECT count(*) FROM ai_reports")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let incidents: i64 = sqlx::query_scalar("SELECT count(*) FROM ai_incidents")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!((reports, incidents), (0, 0));
}
