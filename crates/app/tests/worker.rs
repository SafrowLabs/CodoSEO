//! End-to-end worker tests against a real throwaway Postgres database and a `codoseo_testkit`
//! fixture HTTP server.

mod support;

use codoseo::worker::{
    DEFAULT_MEMORY_BUDGET, ProbeSettings, WorkerSettings, address_policy_for_mode,
    worker_loop_once, worker_loop_once_with,
};
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
        "INSERT INTO accounts (email, plan) VALUES ('lite@example.com', 'lite') RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .expect("insert lite account");
    let site_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO sites (account_id, domain, start_url) \
         VALUES ($1, 'lite.example', 'https://lite.example/') RETURNING id",
    )
    .bind(account_id)
    .fetch_one(&db.pool)
    .await
    .expect("insert site under the lite account");

    let limits = codoseo::worker::resolve_limits(&db.pool, site_id, &serde_json::json!({}))
        .await
        .expect("resolve_limits");
    assert_eq!(
        limits.max_pages,
        Some(5_000),
        "a Lite-plan site must use the Lite plan's page cap, not the Free-shaped default"
    );

    // An unclaimed site (no account) must fall back to the quick-audit limits, not Lite's.
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
        Some(5_000),
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

/// Runs one crawl of `site_id`, with short probe gaps for sites whose plan probes.
async fn run_crawl(db: &TestDb, site_id: uuid::Uuid, trigger: CrawlTrigger) -> uuid::Uuid {
    run_crawl_with(db, site_id, trigger, &fast_probes()).await
}

/// The open incidents, without the advisory ones (Content Signals, Markdown) the test sites raise.
async fn open_incidents(
    pool: &sqlx::PgPool,
    site_id: uuid::Uuid,
) -> Result<Vec<codoseo_store::geo::Incident>, sqlx::Error> {
    Ok(codoseo_store::geo::open_incidents(pool, site_id)
        .await?
        .into_iter()
        .filter(|i| !i.kind.is_advisory())
        .collect())
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
    // Leaving out the advisory ones (Content Signals, Markdown) the test site raises.
    let open: Vec<_> = open_incidents(&db.pool, site_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|i| !i.kind.is_advisory())
        .collect();
    assert_eq!(open.len(), 1);
    assert!(open[0].quiet);
    assert!(ai_changes(&db, first).await.is_empty());
    assert_eq!(alert_planning_jobs(&db, first).await, 0);

    // (b) The rule goes: resolved, with a change and an alert job.
    knobs.lock().unwrap().robots_body = "User-agent: *\nAllow: /\n".to_owned();
    let second = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    assert!(
        open_incidents(&db.pool, site_id)
            .await
            .unwrap()
            .iter()
            .all(|i| i.kind.is_advisory())
    );
    assert!(
        ai_changes(&db, second).await.is_empty(),
        "a quiet incident resolves quietly"
    );

    // (a) The rule comes back: one critical incident, one change, one alert job.
    knobs.lock().unwrap().robots_body =
        "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /\n".to_owned();
    let third = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    let open = open_incidents(&db.pool, site_id).await.unwrap();
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
    assert!(open_incidents(&db.pool, site_id).await.unwrap().is_empty());
    assert_eq!(
        ai_changes(&db, fourth).await,
        vec![("ai_issue_resolved".to_owned(), "notice".to_owned())]
    );
    assert_eq!(alert_planning_jobs(&db, fourth).await, 1);

    // (d) A site-wide nosnippet is one incident.
    knobs.lock().unwrap().meta_robots = Some("nosnippet".to_owned());
    let fifth = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    let open = open_incidents(&db.pool, site_id).await.unwrap();
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
    assert!(open_incidents(&db.pool, site_id).await.unwrap().is_empty());
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
    let open = open_incidents(&db.pool, site_id).await.unwrap();
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
        worker_loop_once_with(
            &db.pool,
            &crawl_queue,
            &job_queue,
            "test-worker",
            DEFAULT_MEMORY_BUDGET,
            AddressPolicy::AllowPrivate,
            &fast_probes(),
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
    let incidents: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ai_incidents WHERE site_id = $1 \
         AND kind NOT IN ('content_signals_missing', 'markdown_unavailable')",
    )
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

#[tokio::test]
async fn a_quick_audit_does_not_fetch_tdmrep_and_a_full_crawl_does() {
    let db = TestDb::new().await;
    let site = SiteBuilder::new()
        .html("/", "Home", &[])
        .page(
            "/.well-known/tdmrep.json",
            codoseo_testkit::Page::html("[]"),
        )
        .start()
        .await;
    let site_id = db.seed_site("example.test", site.url("/").as_str()).await;
    run_crawl(&db, site_id, CrawlTrigger::Quick).await;
    assert_eq!(site.path_hits("/.well-known/tdmrep.json"), 0);
    run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    assert_eq!(site.path_hits("/.well-known/tdmrep.json"), 1);
}

// ---- Synthetic AI-agent fetch probes ------------------------------------------------------

/// What the fake WAF in front of the probe site does.
#[derive(Clone, Default)]
struct Waf {
    /// User agents containing one of these get a 403.
    blocked: Vec<&'static str>,
    /// Every AI user agent gets a 429 with `Retry-After: 0`.
    throttle_bots: bool,
}

/// One request the probe site saw: its path and user agent.
type SeenLog = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

/// A four-page site behind a fake WAF the test can change between crawls, logging every request.
async fn waf_site() -> (
    codoseo_testkit::TestServer,
    std::sync::Arc<std::sync::Mutex<Waf>>,
    SeenLog,
) {
    use axum::http::{HeaderMap, StatusCode, Uri, header};
    use axum::response::IntoResponse;

    let waf = std::sync::Arc::new(std::sync::Mutex::new(Waf::default()));
    let seen: SeenLog = Default::default();
    let (state, log) = (waf.clone(), seen.clone());
    let router =
        axum::Router::new().fallback(axum::routing::any(move |uri: Uri, headers: HeaderMap| {
            let w = state.lock().unwrap().clone();
            let ua = headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            log.lock()
                .unwrap()
                .push((uri.path().to_owned(), ua.clone()));
            async move {
                if w.blocked.iter().any(|b| ua.contains(b)) {
                    return (
                        StatusCode::FORBIDDEN,
                        [(header::CONTENT_TYPE, "text/html")],
                        "<html><head><title>Forbidden</title></head><body>No.</body></html>",
                    )
                        .into_response();
                }
                if w.throttle_bots && is_ai_user_agent(&ua) {
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        [(header::RETRY_AFTER, "0")],
                        "slow down",
                    )
                        .into_response();
                }
                match uri.path() {
                    "/robots.txt" => (
                        [(header::CONTENT_TYPE, "text/plain")],
                        "User-agent: *\nAllow: /\n",
                    )
                        .into_response(),
                    "/" | "/a" | "/b" | "/c" => {
                        let links = if uri.path() == "/" {
                            "<a href=\"/a\">A</a><a href=\"/b\">B</a><a href=\"/c\">C</a>"
                        } else {
                            "<a href=\"/\">Home</a>"
                        };
                        (
                            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                            format!(
                                "<!doctype html><html lang=\"en\"><head><title>Page {}</title>\
                                 <meta name=\"description\" content=\"A page.\"></head>\
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
    (codoseo_testkit::TestServer::start(router).await, waf, seen)
}

/// A user agent of a bot the probes impersonate.
fn is_ai_user_agent(ua: &str) -> bool {
    codoseo_geo::probe::candidates()
        .0
        .iter()
        .any(|b| ua == b.user_agent)
}

fn ai_requests(seen: &SeenLog) -> usize {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|(_, ua)| is_ai_user_agent(ua))
        .count()
}

/// Worker settings with short probe gaps, so a probe run takes milliseconds.
fn fast_probes() -> WorkerSettings {
    WorkerSettings {
        probes: ProbeSettings {
            min_gap: std::time::Duration::from_millis(5),
            request_timeout: std::time::Duration::from_secs(5),
            ..ProbeSettings::default()
        },
        ..WorkerSettings::default()
    }
}

/// A site owned by an account on `plan`.
async fn seed_owned_site(db: &TestDb, start_url: &str, plan: &str) -> uuid::Uuid {
    let account: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO accounts (email, email_canonical, plan) VALUES ($1, $1, $2::plan) RETURNING id",
    )
    .bind(format!("{}@example.com", uuid::Uuid::new_v4().simple()))
    .bind(plan)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO sites (account_id, domain, start_url) VALUES ($1, 'example.test', $2) \
         RETURNING id",
    )
    .bind(account)
    .bind(start_url)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

async fn run_crawl_with(
    db: &TestDb,
    site_id: uuid::Uuid,
    trigger: CrawlTrigger,
    settings: &WorkerSettings,
) -> uuid::Uuid {
    let crawl_queue = CrawlQueue::new(db.pool.clone());
    let job_queue = JobQueue::new(db.pool.clone());
    let id = crawl_queue
        .enqueue(site_id, "example.test", trigger, 2, None, None)
        .await
        .expect("enqueue crawl");
    assert!(
        worker_loop_once_with(
            &db.pool,
            &crawl_queue,
            &job_queue,
            "test-worker",
            DEFAULT_MEMORY_BUDGET,
            AddressPolicy::AllowPrivate,
            settings,
        )
        .await
        .expect("worker_loop_once_with")
    );
    id
}

async fn latest_probes(
    db: &TestDb,
    site_id: uuid::Uuid,
) -> Option<codoseo_geo::probe::ProbeReport> {
    codoseo_store::geo::latest_report(&db.pool, site_id)
        .await
        .unwrap()
        .expect("a stored AI access report")
        .report
        .probes
}

async fn crawl_status(db: &TestDb, crawl: uuid::Uuid) -> String {
    sqlx::query_scalar("SELECT status::text FROM crawls WHERE id = $1")
        .bind(crawl)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn probes_open_a_quiet_incident_first_and_a_new_blocked_bot_writes_one_change() {
    use codoseo_geo::findings::FindingKind;

    let db = TestDb::new().await;
    let (server, waf, seen) = waf_site().await;
    let site_id = seed_owned_site(&db, server.url("/").as_str(), "free").await;

    waf.lock().unwrap().blocked = vec!["OAI-SearchBot"];
    let first = run_crawl_with(&db, site_id, CrawlTrigger::Manual, &fast_probes()).await;
    assert_eq!(crawl_status(&db, first).await, "done");
    let probes = latest_probes(&db, site_id).await.expect("probes stored");
    assert!(probes.synthetic);
    assert_eq!(
        probes.urls.len(),
        1,
        "the free plan probes the home page only"
    );
    assert!(ai_requests(&seen) > 0);
    let open = open_incidents(&db.pool, site_id).await.unwrap();
    let fetch: Vec<_> = open
        .iter()
        .filter(|i| i.kind == FindingKind::FetchBlocked)
        .collect();
    assert_eq!(fetch.len(), 1, "{open:?}");
    assert_eq!(fetch[0].subject, "blocked_403");
    assert!(fetch[0].quiet, "the first evaluation is the baseline");
    assert!(ai_changes(&db, first).await.is_empty());

    waf.lock().unwrap().blocked = vec!["OAI-SearchBot", "PerplexityBot"];
    let second = run_crawl_with(&db, site_id, CrawlTrigger::Manual, &fast_probes()).await;
    assert_eq!(
        ai_changes(&db, second).await,
        vec![("ai_fetch_blocked".to_owned(), "warning".to_owned())]
    );
}

#[tokio::test]
async fn quick_audits_and_opted_out_sites_send_no_ai_requests() {
    let db = TestDb::new().await;
    let (server, _waf, seen) = waf_site().await;

    // A no-signup audit: no account, quick limits.
    let unclaimed = db.seed_site("example.test", server.url("/").as_str()).await;
    run_crawl_with(&db, unclaimed, CrawlTrigger::Quick, &fast_probes()).await;
    // A quick crawl of an owned site.
    let owned = seed_owned_site(&db, server.url("/").as_str(), "lite").await;
    run_crawl_with(&db, owned, CrawlTrigger::Quick, &fast_probes()).await;
    assert_eq!(ai_requests(&seen), 0, "quick audits never probe");

    // An owner who switched the probes off.
    codoseo_store::geo::set_ai_probes(&db.pool, owned, false)
        .await
        .unwrap();
    let crawl = run_crawl_with(&db, owned, CrawlTrigger::Manual, &fast_probes()).await;
    assert_eq!(crawl_status(&db, crawl).await, "done");
    assert_eq!(ai_requests(&seen), 0, "ai_probes = false never probes");
    assert_eq!(latest_probes(&db, owned).await, None);
}

#[tokio::test]
async fn probes_leave_the_health_score_alone() {
    let db = TestDb::new().await;
    let (server, waf, _seen) = waf_site().await;
    waf.lock().unwrap().blocked = vec!["OAI-SearchBot"];
    let with = seed_owned_site(&db, server.url("/").as_str(), "free").await;
    let without = seed_owned_site(&db, server.url("/").as_str(), "free").await;
    codoseo_store::geo::set_ai_probes(&db.pool, without, false)
        .await
        .unwrap();
    let a = run_crawl_with(&db, with, CrawlTrigger::Manual, &fast_probes()).await;
    let b = run_crawl_with(&db, without, CrawlTrigger::Manual, &fast_probes()).await;
    assert!(latest_probes(&db, with).await.is_some());
    assert!(latest_probes(&db, without).await.is_none());
    let score = |crawl: uuid::Uuid| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, Option<i16>>("SELECT health_score FROM crawls WHERE id = $1")
                .bind(crawl)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    let (sa, sb) = (score(a).await, score(b).await);
    assert!(sa.is_some());
    assert_eq!(sa, sb);
}

#[tokio::test]
async fn a_throttling_waf_cannot_push_the_probes_past_the_plan_cap() {
    let db = TestDb::new().await;
    let (server, waf, seen) = waf_site().await;
    waf.lock().unwrap().throttle_bots = true;
    let site_id = seed_owned_site(&db, server.url("/").as_str(), "lite").await;
    let before = seen.lock().unwrap().len();
    run_crawl_with(&db, site_id, CrawlTrigger::Manual, &fast_probes()).await;
    let probes = latest_probes(&db, site_id).await.expect("probes stored");
    let cap =
        codoseo_core::plan::PlanLimits::for_plan(codoseo_core::plan::Plan::Lite).probe_max_requests;
    assert!(probes.requests <= cap, "{} > {cap}", probes.requests);
    assert!(
        ai_requests(&seen) <= usize::from(cap),
        "{} AI requests",
        ai_requests(&seen)
    );
    assert!(seen.lock().unwrap().len() > before);
    // Retries and confirmations ate the cap: the run went further than one request per pair.
    assert!(ai_requests(&seen) > probes.bots.len());
}

#[tokio::test]
async fn heartbeats_continue_through_a_slow_probe_run() {
    let db = TestDb::new().await;
    let (server, _waf, seen) = waf_site().await;
    let site_id = seed_owned_site(&db, server.url("/").as_str(), "free").await;
    // About 10 probe requests 300 ms apart: 3 s with no crawl progress, against a 1 s stale limit
    // and a 100 ms liveness heartbeat.
    let settings = WorkerSettings {
        probes: ProbeSettings {
            min_gap: std::time::Duration::from_millis(300),
            ..fast_probes().probes
        },
        liveness_interval: std::time::Duration::from_millis(100),
    };
    let crawl_queue = CrawlQueue::new(db.pool.clone());
    let stop = tokio_util::sync::CancellationToken::new();
    let sweeper = {
        let crawl_queue = crawl_queue.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            let mut moved = 0;
            while !stop.is_cancelled() {
                moved += crawl_queue
                    .requeue_stale(std::time::Duration::from_secs(1))
                    .await
                    .unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
            moved
        })
    };
    let started = std::time::Instant::now();
    let crawl = run_crawl_with(&db, site_id, CrawlTrigger::Manual, &settings).await;
    let elapsed = started.elapsed();
    stop.cancel();
    assert_eq!(sweeper.await.unwrap(), 0, "the crawl never looked dead");
    assert!(elapsed > std::time::Duration::from_secs(2), "{elapsed:?}");
    assert_eq!(crawl_status(&db, crawl).await, "done");
    assert!(latest_probes(&db, site_id).await.is_some());
    assert!(ai_requests(&seen) > 0);
}

// ---- Content in the HTML and llms.txt through real crawls (B2.11) ------------------------------

#[derive(Clone)]
struct ContentKnobs {
    /// The home page is an empty client-side app shell (Create React App) instead of real text.
    shell: bool,
    /// The status `/llms.txt` answers with; 404 for none.
    llms_status: u16,
    /// robots.txt disallows `/llms.txt`, so the crawler does not fetch it.
    robots_blocks_llms: bool,
}

async fn content_site() -> (
    codoseo_testkit::TestServer,
    std::sync::Arc<std::sync::Mutex<ContentKnobs>>,
) {
    use axum::http::{StatusCode, Uri, header};
    use axum::response::IntoResponse;

    let knobs = std::sync::Arc::new(std::sync::Mutex::new(ContentKnobs {
        shell: false,
        llms_status: 404,
        robots_blocks_llms: false,
    }));
    let state = knobs.clone();
    let router = axum::Router::new().fallback(axum::routing::any(move |uri: Uri| {
        let k = state.lock().unwrap().clone();
        async move {
            match uri.path() {
                "/robots.txt" => (
                    [(header::CONTENT_TYPE, "text/plain")],
                    if k.robots_blocks_llms {
                        "User-agent: *\nAllow: /\nDisallow: /llms.txt\n"
                    } else {
                        "User-agent: *\nAllow: /\n"
                    },
                )
                    .into_response(),
                "/llms.txt" => match k.llms_status {
                    200 => (
                        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                        "# Example\n\n> A small site.\n\n## Pages\n\n- [Home](/): the home page\n",
                    )
                        .into_response(),
                    s => StatusCode::from_u16(s).unwrap().into_response(),
                },
                "/" => {
                    let body = if k.shell {
                        "<div id=\"root\"></div>\
                         <noscript>You need to enable JavaScript to run this app.</noscript>\
                         <script src=\"/static/js/main.js\"></script>"
                            .to_owned()
                    } else {
                        format!(
                            "<h1>Home</h1><p>{}</p>",
                            "Plenty of server rendered words for a crawler to read. ".repeat(12)
                        )
                    };
                    (
                        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                        format!(
                            "<!doctype html><html lang=\"en\"><head><title>Home page</title>\
                             <meta name=\"description\" content=\"A page.\"></head>\
                             <body>{body}</body></html>"
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

async fn open_kinds(db: &TestDb, site_id: uuid::Uuid, slug: &str) -> Vec<(String, bool)> {
    sqlx::query_as(
        "SELECT severity::text, quiet FROM ai_incidents \
         WHERE site_id = $1 AND kind = $2 AND resolved_at IS NULL",
    )
    .bind(site_id)
    .bind(slug)
    .fetch_all(&db.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_client_side_shell_is_a_quiet_baseline_and_a_later_regression_alerts_instantly() {
    use codoseo_core::change::ChangeKind;

    let db = TestDb::new().await;
    let (server, knobs) = content_site().await;
    let site_id = seed_owned_site(&db, server.url("/").as_str(), "free").await;
    let account: uuid::Uuid = sqlx::query_scalar("SELECT account_id FROM sites WHERE id = $1")
        .bind(site_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    codoseo_store::channels::ensure_default_email(
        &db.pool,
        &codoseo_notify::ChannelKey::derive("k"),
        account,
    )
    .await
    .unwrap();
    codoseo_store::alert_rules::create_defaults_for_site(&db.pool, account, site_id)
        .await
        .unwrap();

    // An app that was never server rendered: the first report is the baseline.
    knobs.lock().unwrap().shell = true;
    let first = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    let open = open_kinds(&db, site_id, "content_missing").await;
    assert_eq!(open.len(), 1);
    assert!(open[0].1, "quiet");
    assert!(ai_changes(&db, first).await.is_empty());
    assert_eq!(alert_planning_jobs(&db, first).await, 0);

    // Server rendered again: resolved.
    knobs.lock().unwrap().shell = false;
    run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    assert!(open_kinds(&db, site_id, "content_missing").await.is_empty());

    // The regression: SSR to client-side rendering raises an instant alert.
    knobs.lock().unwrap().shell = true;
    let third = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    let changes = ai_changes(&db, third).await;
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].0, "ai_content_missing");
    assert_eq!(alert_planning_jobs(&db, third).await, 1);
    assert!(codoseo_store::alert_rules::DEFAULT_INSTANT.contains(&ChangeKind::AiContentMissing));
    let channels = codoseo_store::alert_rules::instant_channels_for(
        &db.pool,
        site_id,
        ChangeKind::AiContentMissing,
    )
    .await
    .unwrap();
    assert_eq!(channels.len(), 1, "an instant rule on the default channel");
}

#[tokio::test]
async fn an_adopted_llms_txt_that_goes_down_opens_a_digest_only_warning() {
    use codoseo_core::change::ChangeKind;
    use codoseo_geo::llms::LlmsState;

    let db = TestDb::new().await;
    let (server, knobs) = content_site().await;
    let site_id = seed_owned_site(&db, server.url("/").as_str(), "free").await;

    knobs.lock().unwrap().llms_status = 200;
    let first = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    let stored = codoseo_store::geo::latest_llms_txt(&db.pool, site_id)
        .await
        .unwrap()
        .expect("llms.txt was fetched");
    assert_eq!(stored.state, LlmsState::Available);
    assert!(stored.adopted);
    assert!(ai_changes(&db, first).await.is_empty());

    knobs.lock().unwrap().llms_status = 503;
    let second = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    let open = open_kinds(&db, site_id, "llms_txt_unavailable").await;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].0, "warning");
    // The first judged report was the 200: the incident is new, so it is announced.
    assert!(!open[0].1);
    assert_eq!(
        ai_changes(&db, second).await,
        vec![("ai_llms_txt_unavailable".to_owned(), "warning".to_owned())]
    );
    assert!(
        !codoseo_store::alert_rules::DEFAULT_INSTANT.contains(&ChangeKind::AiLlmsTxtUnavailable)
    );
}

#[tokio::test]
async fn adoption_is_carried_across_a_crawl_that_did_not_fetch_llms_txt() {
    let db = TestDb::new().await;
    let (server, knobs) = content_site().await;
    let site_id = seed_owned_site(&db, server.url("/").as_str(), "free").await;

    knobs.lock().unwrap().llms_status = 200;
    run_crawl(&db, site_id, CrawlTrigger::Manual).await;

    // robots.txt now keeps the crawler off /llms.txt: the report has no llms_txt at all.
    {
        let mut k = knobs.lock().unwrap();
        k.robots_blocks_llms = true;
        k.llms_status = 503;
    }
    run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    let latest = codoseo_store::geo::latest_report(&db.pool, site_id)
        .await
        .unwrap()
        .unwrap();
    assert!(latest.report.llms_txt.is_none(), "not fetched this time");
    assert!(
        open_kinds(&db, site_id, "llms_txt_unavailable")
            .await
            .is_empty()
    );
    assert!(
        codoseo_store::geo::latest_llms_txt(&db.pool, site_id)
            .await
            .unwrap()
            .unwrap()
            .adopted,
        "the newest report that fetched it is carried, not the latest report"
    );

    // Fetched again and failing: still adopted, so the incident opens.
    knobs.lock().unwrap().robots_blocks_llms = false;
    let third = run_crawl(&db, site_id, CrawlTrigger::Manual).await;
    assert_eq!(
        open_kinds(&db, site_id, "llms_txt_unavailable").await.len(),
        1
    );
    assert_eq!(
        ai_changes(&db, third).await,
        vec![("ai_llms_txt_unavailable".to_owned(), "warning".to_owned())]
    );
}

#[tokio::test]
async fn content_and_llms_findings_leave_the_health_score_alone() {
    let db = TestDb::new().await;
    let (server, knobs) = content_site().await;
    let score = |crawl: uuid::Uuid| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, Option<i16>>("SELECT health_score FROM crawls WHERE id = $1")
                .bind(crawl)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };

    // The same markup, so the same score, whether or not the AI findings fire: only the
    // findings differ, by giving one site an llms.txt that then fails.
    let calm = seed_owned_site(&db, server.url("/").as_str(), "free").await;
    let a = run_crawl(&db, calm, CrawlTrigger::Manual).await;
    let noisy = seed_owned_site(&db, server.url("/").as_str(), "free").await;
    knobs.lock().unwrap().llms_status = 200;
    run_crawl(&db, noisy, CrawlTrigger::Manual).await;
    knobs.lock().unwrap().llms_status = 503;
    let b = run_crawl(&db, noisy, CrawlTrigger::Manual).await;
    assert_eq!(
        open_kinds(&db, noisy, "llms_txt_unavailable").await.len(),
        1
    );
    assert!(score(a).await.is_some());
    assert_eq!(score(a).await, score(b).await);

    // And a shell page that raises content_missing scores like the page it replaces would,
    // given the checks look at the page itself, so compare the same shell with and without
    // the finding (the owner accepts none, so use the store's incident as the proof).
    knobs.lock().unwrap().shell = true;
    let shell = seed_owned_site(&db, server.url("/").as_str(), "free").await;
    let c = run_crawl(&db, shell, CrawlTrigger::Manual).await;
    assert_eq!(open_kinds(&db, shell, "content_missing").await.len(), 1);
    let d = run_crawl(&db, shell, CrawlTrigger::Manual).await;
    assert_eq!(score(c).await, score(d).await);
}

/// A one-page client-side shell. With `noisy` the site also trips every AI finding: robots.txt
/// shuts OAI-SearchBot out, the AI user agents get a Cloudflare challenge, and `/llms.txt` answers
/// the status in `llms`. Without it the same page is served with none of that.
async fn leak_site(
    noisy: bool,
    llms: std::sync::Arc<std::sync::atomic::AtomicU16>,
) -> codoseo_testkit::TestServer {
    use axum::http::{HeaderMap, StatusCode, Uri, header};
    use axum::response::IntoResponse;
    use std::sync::atomic::Ordering;

    let router =
        axum::Router::new().fallback(axum::routing::any(move |uri: Uri, headers: HeaderMap| {
            let ua = headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let llms = llms.load(Ordering::SeqCst);
            async move {
                if noisy && is_ai_user_agent(&ua) {
                    return (
                        StatusCode::FORBIDDEN,
                        [(header::CONTENT_TYPE, "text/html")],
                        [("cf-mitigated", "challenge")],
                        "<html><head><title>Just a moment...</title></head></html>",
                    )
                        .into_response();
                }
                match uri.path() {
                    "/robots.txt" => (
                        [(header::CONTENT_TYPE, "text/plain")],
                        if noisy {
                            "User-agent: OAI-SearchBot\nDisallow: /\n\nUser-agent: *\nAllow: /\n"
                        } else {
                            "User-agent: *\nAllow: /\n"
                        },
                    )
                        .into_response(),
                    "/llms.txt" if noisy && llms == 200 => (
                        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                        "# Example\n\n> A small site.\n\n## Pages\n\n- [Home](/): the home page\n",
                    )
                        .into_response(),
                    "/llms.txt" if noisy => StatusCode::from_u16(llms).unwrap().into_response(),
                    "/" => (
                        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                        "<!doctype html><html lang=\"en\"><head><title>Home page</title>\
                         <meta name=\"description\" content=\"A page.\"></head>\
                         <body><div id=\"root\"></div>\
                         <noscript>You need to enable JavaScript to run this app.</noscript>\
                         <script src=\"/static/js/main.js\"></script></body></html>",
                    )
                        .into_response(),
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            }
        }));
    codoseo_testkit::TestServer::start(router).await
}

#[tokio::test]
async fn every_kind_of_ai_finding_together_leaves_the_stored_health_score_alone() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU16, Ordering};

    let db = TestDb::new().await;
    let llms = Arc::new(AtomicU16::new(200));
    let quiet = leak_site(false, llms.clone()).await;
    let noisy = leak_site(true, llms.clone()).await;
    let a_site = seed_owned_site(&db, quiet.url("/").as_str(), "free").await;
    let b_site = seed_owned_site(&db, noisy.url("/").as_str(), "free").await;

    // llms.txt is adopted on the first crawl and fails on the second.
    run_crawl(&db, b_site, CrawlTrigger::Manual).await;
    llms.store(503, Ordering::SeqCst);
    let a = run_crawl(&db, a_site, CrawlTrigger::Manual).await;
    let b = run_crawl(&db, b_site, CrawlTrigger::Manual).await;

    let kinds: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT kind FROM ai_incidents WHERE site_id = $1 AND resolved_at IS NULL \
         ORDER BY kind",
    )
    .bind(b_site)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    for expected in [
        "bots_blocked",
        "content_missing",
        "fetch_blocked",
        "llms_txt_unavailable",
    ] {
        assert!(
            kinds.iter().any(|k| k == expected),
            "{expected} in {kinds:?}"
        );
    }

    let score = |crawl: uuid::Uuid| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, Option<i16>>("SELECT health_score FROM crawls WHERE id = $1")
                .bind(crawl)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert!(score(a).await.is_some());
    assert_eq!(score(a).await, score(b).await);
}
