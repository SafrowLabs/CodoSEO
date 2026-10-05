//! `digest::site_week`: the latest finished crawl, the crawl to compare it with, new and
//! resolved issues from the per-check counts, and the week's changes, against real Postgres
//! with every timestamp set explicitly.

mod support;

use codoseo_core::check::Severity;
use codoseo_store::digest::{self, SiteWeek};
use serde_json::json;
use sqlx::PgPool;
use support::TestDb;
use time::macros::datetime;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// Monday 2026-10-05 08:00 UTC, the moment the digest is built.
fn now() -> OffsetDateTime {
    datetime!(2026-10-05 08:00 UTC)
}

async fn site(pool: &PgPool) -> Uuid {
    let account: Uuid = sqlx::query_scalar(
        "INSERT INTO accounts (email, email_canonical) VALUES ($1, $1) RETURNING id",
    )
    .bind(format!("{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO sites (account_id, domain, start_url) \
         VALUES ($1, 'example.com', 'https://example.com/') RETURNING id",
    )
    .bind(account)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A done crawl that finished `days_ago` days before `now()`.
async fn crawl(
    pool: &PgPool,
    site: Uuid,
    days_ago: f64,
    score: i16,
    passed: i16,
    counts: &[(&str, u32)],
) -> Uuid {
    let finished = now() - Duration::seconds_f64(days_ago * 86_400.0);
    let summary = json!({
        "stop_reason": {"kind": "completed"},
        "report_summary": {},
        "counts": counts.iter().map(|(s, n)| json!([s, n])).collect::<Vec<_>>(),
    });
    sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, status, trigger, priority, queued_at, started_at, \
                             finished_at, health_score, checks_passed, checks_total, summary) \
         VALUES ($1, 'example.com', 'done', 'schedule', 3, $2, $2, $2, $3, $4, 40, $5) \
         RETURNING id",
    )
    .bind(site)
    .bind(finished)
    .bind(score)
    .bind(passed)
    .bind(summary)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn change(
    pool: &PgPool,
    crawl: Uuid,
    site: Uuid,
    severity: &str,
    url: &str,
    days_ago: f64,
    alerted: bool,
) {
    let at = now() - Duration::seconds_f64(days_ago * 86_400.0);
    sqlx::query(
        "INSERT INTO changes (crawl_id, site_id, kind, severity, url, before_value, after_value, \
                              created_at, alerted_at) \
         VALUES ($1, $2, 'status_changed', $3::severity, $4, '200', '404', $5, \
                 CASE WHEN $6 THEN $5 END)",
    )
    .bind(crawl)
    .bind(site)
    .bind(severity)
    .bind(url)
    .bind(at)
    .bind(alerted)
    .execute(pool)
    .await
    .unwrap();
}

async fn week(pool: &PgPool, site: Uuid) -> Option<SiteWeek> {
    digest::site_week(pool, site, now()).await.unwrap()
}

#[tokio::test]
async fn a_site_without_a_finished_crawl_has_no_week() {
    let db = TestDb::new().await;
    let s = site(&db.pool).await;
    assert!(week(&db.pool, s).await.is_none());
    // Queued, running and failed crawls don't count.
    for status in ["queued", "running", "failed"] {
        sqlx::query(
            "INSERT INTO crawls (site_id, domain, status, trigger, priority) \
             VALUES ($1, 'example.com', $2::crawl_status, 'schedule', 3)",
        )
        .bind(s)
        .bind(status)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    assert!(week(&db.pool, s).await.is_none());
}

#[tokio::test]
async fn the_first_week_has_no_baseline_and_no_delta() {
    let db = TestDb::new().await;
    let s = site(&db.pool).await;
    crawl(&db.pool, s, 0.5, 71, 30, &[("title_too_long", 4)]).await;
    let w = week(&db.pool, s).await.unwrap();
    assert_eq!(w.domain, "example.com");
    assert_eq!(
        (
            w.latest.score,
            w.latest.checks_passed,
            w.latest.checks_total
        ),
        (71, 30, 40)
    );
    assert!(w.baseline.is_none());
    assert_eq!(w.score_delta(), None);
    assert!(w.new_issues.is_empty() && w.resolved_issues.is_empty());
}

#[tokio::test]
async fn the_baseline_is_the_latest_crawl_a_week_or_more_before_the_latest() {
    let db = TestDb::new().await;
    let s = site(&db.pool).await;
    let old = crawl(&db.pool, s, 20.0, 50, 20, &[]).await;
    let week_ago = crawl(&db.pool, s, 9.0, 80, 35, &[]).await;
    crawl(&db.pool, s, 5.0, 85, 36, &[]).await;
    let latest = crawl(&db.pool, s, 1.0, 90, 38, &[]).await;
    let w = week(&db.pool, s).await.unwrap();
    assert_eq!(w.latest.crawl_id, latest);
    // 9 days ago is 8 days before the latest, the nearest one at least 7 days back.
    assert_eq!(w.baseline.as_ref().unwrap().crawl_id, week_ago);
    assert_ne!(w.baseline.as_ref().unwrap().crawl_id, old);
    assert_eq!(w.score_delta(), Some(10));
}

#[tokio::test]
async fn without_a_crawl_a_week_back_the_oldest_one_in_the_window_is_the_baseline() {
    let db = TestDb::new().await;
    let s = site(&db.pool).await;
    let oldest = crawl(&db.pool, s, 5.0, 60, 30, &[]).await;
    crawl(&db.pool, s, 3.0, 62, 31, &[]).await;
    crawl(&db.pool, s, 1.0, 58, 29, &[]).await;
    let w = week(&db.pool, s).await.unwrap();
    assert_eq!(w.baseline.as_ref().unwrap().crawl_id, oldest);
    // A drop reads as negative.
    assert_eq!(w.score_delta(), Some(-2));
}

#[tokio::test]
async fn crawls_outside_the_window_are_not_a_baseline_unless_a_week_before_the_latest() {
    let db = TestDb::new().await;
    let s = site(&db.pool).await;
    // The site went quiet: its latest crawl is 10 days old and nothing else is in the window.
    crawl(&db.pool, s, 10.0, 70, 30, &[]).await;
    let w = week(&db.pool, s).await.unwrap();
    assert!(w.baseline.is_none());
}

#[tokio::test]
async fn new_and_resolved_issues_come_from_comparing_the_per_check_counts() {
    let db = TestDb::new().await;
    let s = site(&db.pool).await;
    crawl(
        &db.pool,
        s,
        8.0,
        80,
        34,
        &[
            ("title_too_long", 9),
            ("description_missing", 6),
            ("http_404_stays", 0),
            ("images_missing_alt", 3),
        ],
    )
    .await;
    crawl(
        &db.pool,
        s,
        0.5,
        85,
        36,
        &[
            ("title_too_long", 11),
            ("http_5xx", 2),
            ("images_missing_alt", 1),
        ],
    )
    .await;
    let w = week(&db.pool, s).await.unwrap();
    // Failing now but not then: new, with this week's page count.
    assert_eq!(w.new_issues, vec![("http_5xx".to_owned(), 2)]);
    // Failing then but not now: resolved, with last week's page count. A count of 0 never
    // failed in the first place.
    assert_eq!(
        w.resolved_issues,
        vec![("description_missing".to_owned(), 6)]
    );
}

#[tokio::test]
async fn changes_cover_the_last_seven_days_whether_or_not_they_were_alerted() {
    let db = TestDb::new().await;
    let s = site(&db.pool).await;
    let c = crawl(&db.pool, s, 0.5, 85, 36, &[]).await;
    // In the window: 2 critical (one already sent instantly), 1 warning, 3 notices.
    change(
        &db.pool,
        c,
        s,
        "critical",
        "https://example.com/a",
        0.5,
        true,
    )
    .await;
    change(
        &db.pool,
        c,
        s,
        "critical",
        "https://example.com/b",
        0.4,
        false,
    )
    .await;
    change(
        &db.pool,
        c,
        s,
        "warning",
        "https://example.com/c",
        3.0,
        false,
    )
    .await;
    for i in 0..3 {
        change(
            &db.pool,
            c,
            s,
            "notice",
            &format!("https://example.com/n{i}"),
            6.5,
            false,
        )
        .await;
    }
    // Out of the window, or about another site.
    change(
        &db.pool,
        c,
        s,
        "critical",
        "https://example.com/old",
        7.5,
        false,
    )
    .await;
    let other = site(&db.pool).await;
    let oc = crawl(&db.pool, other, 0.5, 85, 36, &[]).await;
    change(
        &db.pool,
        oc,
        other,
        "critical",
        "https://other.test/x",
        0.5,
        false,
    )
    .await;

    let w = week(&db.pool, s).await.unwrap();
    assert_eq!(
        (w.changes.critical, w.changes.warning, w.changes.notice),
        (2, 1, 3)
    );
    assert_eq!(w.changes.total(), 6);
}

#[tokio::test]
async fn the_examples_are_the_five_most_severe_changes() {
    let db = TestDb::new().await;
    let s = site(&db.pool).await;
    let c = crawl(&db.pool, s, 0.5, 85, 36, &[]).await;
    for i in 0..4 {
        change(
            &db.pool,
            c,
            s,
            "notice",
            &format!("https://example.com/n{i}"),
            1.0,
            false,
        )
        .await;
    }
    for i in 0..3 {
        change(
            &db.pool,
            c,
            s,
            "warning",
            &format!("https://example.com/w{i}"),
            1.0,
            false,
        )
        .await;
    }
    change(
        &db.pool,
        c,
        s,
        "critical",
        "https://example.com/crit",
        1.0,
        true,
    )
    .await;
    let w = week(&db.pool, s).await.unwrap();
    assert_eq!(w.top_changes.len(), 5);
    let severities: Vec<Severity> = w.top_changes.iter().map(|c| c.severity).collect();
    assert_eq!(
        severities,
        [
            Severity::Critical,
            Severity::Warning,
            Severity::Warning,
            Severity::Warning,
            Severity::Notice
        ]
    );
    assert_eq!(
        w.top_changes[0].url.as_deref(),
        Some("https://example.com/crit")
    );
    assert_eq!(w.changes.total(), 8);
}
