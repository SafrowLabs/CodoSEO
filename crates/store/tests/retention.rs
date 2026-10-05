//! T4.5: boundary-date tests for `retention::run` against real Postgres. Every row is seeded
//! with an explicit `created_at`/`finished_at`/`expires_at` offset in seconds (not real sleeps),
//! using day-scale offsets for the plan-history tests (so millisecond test-execution jitter
//! can't flip the boundary) and second-scale offsets for the 7-day/token tests (which the plan
//! asks to check right at a ±1 second margin).

mod support;

use codoseo_store::retention;
use sqlx::PgPool;
use support::TestDb;
use uuid::Uuid;

async fn seed_account(pool: &PgPool, plan: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO accounts (email, plan) VALUES ($1, $2::plan) RETURNING id")
        .bind(format!("{}@example.com", Uuid::new_v4()))
        .bind(plan)
        .fetch_one(pool)
        .await
        .expect("insert account")
}

async fn seed_site(pool: &PgPool, account_id: Option<Uuid>, created_seconds_ago: f64) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO sites (account_id, domain, start_url, created_at) \
         VALUES ($1, $2, $3, now() - ($4 || ' seconds')::interval) RETURNING id",
    )
    .bind(account_id)
    .bind(format!("{}.example", Uuid::new_v4()))
    .bind("https://example.com/")
    .bind(created_seconds_ago.to_string())
    .fetch_one(pool)
    .await
    .expect("insert site")
}

/// A `done` crawl with a non-null `summary` and (optionally) one `changes` row, `finished_at`
/// set `finished_seconds_ago` in the past.
async fn seed_done_crawl(
    pool: &PgPool,
    site_id: Uuid,
    domain: &str,
    finished_seconds_ago: f64,
    with_change: bool,
) -> Uuid {
    let crawl_id: Uuid = sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, status, trigger, priority, finished_at, summary) \
         VALUES ($1, $2, 'done', 'manual', 3, now() - ($3 || ' seconds')::interval, \
                 '{\"stop_reason\":{\"kind\":\"completed\"}}'::jsonb) \
         RETURNING id",
    )
    .bind(site_id)
    .bind(domain)
    .bind(finished_seconds_ago.to_string())
    .fetch_one(pool)
    .await
    .expect("insert crawl");

    if with_change {
        sqlx::query(
            "INSERT INTO changes (crawl_id, site_id, kind, severity, before_value, after_value) \
             VALUES ($1, $2, 'title_changed', 'notice', 'old', 'new')",
        )
        .bind(crawl_id)
        .bind(site_id)
        .execute(pool)
        .await
        .expect("insert change");
    }
    crawl_id
}

async fn crawl_summary_and_changes(pool: &PgPool, crawl_id: Uuid) -> (bool, i64) {
    let summary: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT summary FROM crawls WHERE id = $1")
            .bind(crawl_id)
            .fetch_one(pool)
            .await
            .expect("fetch crawl");
    let changes: i64 = sqlx::query_scalar("SELECT count(*) FROM changes WHERE crawl_id = $1")
        .bind(crawl_id)
        .fetch_one(pool)
        .await
        .expect("count changes");
    (summary.is_some(), changes)
}

async fn crawl_exists(pool: &PgPool, crawl_id: Uuid) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crawls WHERE id = $1")
        .bind(crawl_id)
        .fetch_one(pool)
        .await
        .expect("count crawl")
        > 0
}

const DAY: f64 = 86_400.0;

#[tokio::test]
async fn free_plan_history_boundary_at_30_days() {
    let db = TestDb::new().await;
    let account = seed_account(&db.pool, "free").await;
    let site = seed_site(&db.pool, Some(account), 0.0).await;

    let young = seed_done_crawl(&db.pool, site, "a.example", 29.0 * DAY, true).await;
    let at_boundary = seed_done_crawl(&db.pool, site, "a.example", 30.0 * DAY, true).await;
    let old = seed_done_crawl(&db.pool, site, "a.example", 31.0 * DAY, true).await;

    let report = retention::run(&db.pool, None).await.expect("run retention");
    assert_eq!(report.crawls_trimmed, 2);

    let (has_summary, changes) = crawl_summary_and_changes(&db.pool, young).await;
    assert!(has_summary && changes == 1, "29 days: untouched");

    let (has_summary, changes) = crawl_summary_and_changes(&db.pool, at_boundary).await;
    assert!(!has_summary && changes == 0, "30 days: cleaned");

    let (has_summary, changes) = crawl_summary_and_changes(&db.pool, old).await;
    assert!(!has_summary && changes == 0, "31 days: cleaned");

    // The crawl rows themselves always survive retention; only pages/inlinks/site_files
    // (already capped to 2 by finalize) and now changes/summary are trimmed.
    assert!(crawl_exists(&db.pool, at_boundary).await);
}

#[tokio::test]
async fn pro_plan_history_boundary_at_365_days() {
    let db = TestDb::new().await;
    let account = seed_account(&db.pool, "pro").await;
    let site = seed_site(&db.pool, Some(account), 0.0).await;

    let young = seed_done_crawl(&db.pool, site, "b.example", 364.0 * DAY, true).await;
    let at_boundary = seed_done_crawl(&db.pool, site, "b.example", 365.0 * DAY, true).await;
    let old = seed_done_crawl(&db.pool, site, "b.example", 366.0 * DAY, true).await;

    retention::run(&db.pool, None).await.expect("run retention");

    let (has_summary, _) = crawl_summary_and_changes(&db.pool, young).await;
    assert!(has_summary, "364 days: untouched");
    let (has_summary, _) = crawl_summary_and_changes(&db.pool, at_boundary).await;
    assert!(!has_summary, "365 days: cleaned");
    let (has_summary, _) = crawl_summary_and_changes(&db.pool, old).await;
    assert!(!has_summary, "366 days: cleaned");
}

#[tokio::test]
async fn self_hosted_history_days_is_configurable() {
    let db = TestDb::new().await;
    let account = seed_account(&db.pool, "self_hosted").await;
    let site = seed_site(&db.pool, Some(account), 0.0).await;

    let young = seed_done_crawl(&db.pool, site, "c.example", 9.0 * DAY, true).await;
    let old = seed_done_crawl(&db.pool, site, "c.example", 11.0 * DAY, true).await;

    retention::run(&db.pool, Some(10))
        .await
        .expect("run retention");

    let (has_summary, _) = crawl_summary_and_changes(&db.pool, young).await;
    assert!(has_summary, "9 days with a 10-day override: untouched");
    let (has_summary, _) = crawl_summary_and_changes(&db.pool, old).await;
    assert!(!has_summary, "11 days with a 10-day override: cleaned");
}

#[tokio::test]
async fn unclaimed_site_deleted_after_7_days_with_cascade() {
    let db = TestDb::new().await;

    let fresh = seed_site(&db.pool, None, 7.0 * DAY - 1.0).await;
    let aged = seed_site(&db.pool, None, 7.0 * DAY + 1.0).await;
    let crawl_id = seed_done_crawl(&db.pool, aged, "unclaimed.example", 0.0, true).await;
    sqlx::query(
        "INSERT INTO pages (crawl_id, site_id, url, url_hash, status, indexability) \
         VALUES ($1, $2, 'https://unclaimed.example/', 1, 200, 'indexable')",
    )
    .bind(crawl_id)
    .bind(aged)
    .execute(&db.pool)
    .await
    .expect("insert page");

    let report = retention::run(&db.pool, None).await.expect("run retention");
    assert_eq!(report.unclaimed_sites_deleted, 1);

    let fresh_count: i64 = sqlx::query_scalar("SELECT count(*) FROM sites WHERE id = $1")
        .bind(fresh)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(fresh_count, 1, "7 days - 1s: kept");

    let aged_count: i64 = sqlx::query_scalar("SELECT count(*) FROM sites WHERE id = $1")
        .bind(aged)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(aged_count, 0, "7 days + 1s: deleted");

    assert!(
        !crawl_exists(&db.pool, crawl_id).await,
        "cascade deleted the crawl"
    );
    let page_count: i64 = sqlx::query_scalar("SELECT count(*) FROM pages WHERE crawl_id = $1")
        .bind(crawl_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(page_count, 0, "cascade deleted the page");
}

#[tokio::test]
async fn expired_tokens_and_sessions_are_deleted() {
    let db = TestDb::new().await;
    let account = seed_account(&db.pool, "free").await;

    sqlx::query(
        "INSERT INTO login_tokens (account_id, purpose, token_hash, expires_at) \
         VALUES ($1, 'magic_link', $2, now() - interval '1 second')",
    )
    .bind(account)
    .bind(b"expired-token".as_slice())
    .execute(&db.pool)
    .await
    .expect("insert expired token");
    sqlx::query(
        "INSERT INTO login_tokens (account_id, purpose, token_hash, expires_at) \
         VALUES ($1, 'magic_link', $2, now() + interval '1 hour')",
    )
    .bind(account)
    .bind(b"live-token".as_slice())
    .execute(&db.pool)
    .await
    .expect("insert live token");

    sqlx::query(
        "INSERT INTO sessions (account_id, session_hash, expires_at) \
         VALUES ($1, $2, now() - interval '1 second')",
    )
    .bind(account)
    .bind(b"expired-session".as_slice())
    .execute(&db.pool)
    .await
    .expect("insert expired session");
    sqlx::query(
        "INSERT INTO sessions (account_id, session_hash, expires_at) \
         VALUES ($1, $2, now() + interval '1 hour')",
    )
    .bind(account)
    .bind(b"live-session".as_slice())
    .execute(&db.pool)
    .await
    .expect("insert live session");

    let report = retention::run(&db.pool, None).await.expect("run retention");
    assert_eq!(report.tokens_deleted, 1);
    assert_eq!(report.sessions_deleted, 1);

    let remaining_tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM login_tokens")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(remaining_tokens, 1);
    let remaining_sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(remaining_sessions, 1);
}

#[tokio::test]
async fn old_failed_jobs_are_deleted_after_30_days() {
    let db = TestDb::new().await;

    sqlx::query(
        "INSERT INTO jobs (kind, payload, status, claimed_at) \
         VALUES ('cleanup', '{}'::jsonb, 'failed', now() - interval '31 days')",
    )
    .execute(&db.pool)
    .await
    .expect("insert old failed job");
    sqlx::query(
        "INSERT INTO jobs (kind, payload, status, claimed_at) \
         VALUES ('cleanup', '{}'::jsonb, 'failed', now() - interval '29 days')",
    )
    .execute(&db.pool)
    .await
    .expect("insert recent failed job");

    let report = retention::run(&db.pool, None).await.expect("run retention");
    assert_eq!(report.failed_jobs_deleted, 1);

    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(remaining, 1);
}

/// A `done` job of `kind` that finished `days` days ago, optionally a "Keep monitoring?" warning
/// for `warned`.
async fn seed_done_job(pool: &PgPool, kind: &str, days: i32, warned: Option<Uuid>) {
    let payload = match warned {
        Some(id) => serde_json::json!({ "keep_monitoring_for": id }),
        None => serde_json::json!({}),
    };
    sqlx::query(
        "INSERT INTO jobs (kind, payload, status, created_at, completed_at) \
         VALUES ($1::job_kind, $2, 'done', now() - make_interval(days => $3), \
                 now() - make_interval(days => $3))",
    )
    .bind(kind)
    .bind(payload)
    .bind(days)
    .execute(pool)
    .await
    .expect("insert done job");
}

async fn job_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn done_jobs_are_deleted_after_14_days() {
    let db = TestDb::new().await;
    seed_done_job(&db.pool, "send_alert", 15, None).await;
    seed_done_job(&db.pool, "send_email", 15, None).await;
    seed_done_job(&db.pool, "send_alert", 13, None).await;
    // Queued and running work is never touched, however old.
    sqlx::query(
        "INSERT INTO jobs (kind, payload, status, created_at) \
         VALUES ('cleanup', '{}', 'queued', now() - interval '40 days'), \
                ('cleanup', '{}', 'running', now() - interval '40 days')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let report = retention::run(&db.pool, None).await.expect("run retention");
    assert_eq!(report.done_jobs_deleted, 2);
    assert_eq!(job_count(&db.pool).await, 3);
}

#[tokio::test]
async fn a_pending_keep_monitoring_warning_job_outlives_the_14_days() {
    let db = TestDb::new().await;
    // The pause rule reads the latest warning job of a warned, unpaused Free account, so that
    // job stays until the account is paused or has answered.
    let waiting = seed_account(&db.pool, "free").await;
    let paused = seed_account(&db.pool, "free").await;
    let answered = seed_account(&db.pool, "free").await;
    sqlx::query(
        "UPDATE accounts SET keep_monitoring_sent_at = now() - interval '20 days' \
         WHERE id = ANY($1)",
    )
    .bind(vec![waiting, paused])
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE accounts SET paused = true WHERE id = $1")
        .bind(paused)
        .execute(&db.pool)
        .await
        .unwrap();
    for id in [waiting, paused, answered] {
        seed_done_job(&db.pool, "send_email", 20, Some(id)).await;
    }

    let report = retention::run(&db.pool, None).await.expect("run retention");
    assert_eq!(
        report.done_jobs_deleted, 2,
        "the paused and the answered account's"
    );
    let left: Vec<serde_json::Value> = sqlx::query_scalar("SELECT payload FROM jobs")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        left,
        vec![serde_json::json!({ "keep_monitoring_for": waiting })]
    );
}

#[tokio::test]
async fn the_week_long_pause_window_is_inside_the_14_days() {
    use time::OffsetDateTime;
    let db = TestDb::new().await;
    let id = seed_account(&db.pool, "free").await;
    // Warned 8 days ago, the mail went out then: pause is due, the job is 8 days old.
    sqlx::query(
        "UPDATE accounts SET keep_monitoring_sent_at = now() - interval '8 days', \
                created_at = now() - interval '60 days' WHERE id = $1",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    seed_done_job(&db.pool, "send_email", 8, Some(id)).await;

    retention::run(&db.pool, None).await.expect("run retention");
    assert_eq!(job_count(&db.pool).await, 1, "the warning is still there");
    let paused = codoseo_store::schedule::pause_unresponsive(&db.pool, OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert_eq!(paused, 1);
}
