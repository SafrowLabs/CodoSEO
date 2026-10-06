//! T6.3: funnel events, the admin page's queries, and the RankOrg page list.

mod support;

use codoseo_store::events::{self, EventKind};
use codoseo_store::jobs::{self, JobKind, JobQueue};
use codoseo_store::{quick, reports};
use serde_json::json;
use support::TestDb;
use uuid::Uuid;

async fn age(pool: &sqlx::PgPool, kind: &str, days: i64) {
    sqlx::query("UPDATE events SET created_at = now() - ($2 || ' days')::interval WHERE kind = $1")
        .bind(kind)
        .bind(days.to_string())
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_funnel_lists_every_step_in_order_counting_events_and_unique_audits() {
    let db = TestDb::new().await;
    let crawl_a = Uuid::new_v4();
    let crawl_b = Uuid::new_v4();
    // Two visitors on audit A (one cached hit), one on B: 3 events, 2 audits.
    for crawl in [crawl_a, crawl_a, crawl_b] {
        events::record(
            &db.pool,
            EventKind::AuditStarted,
            None,
            None,
            Some(json!({ "crawl_id": crawl })),
        )
        .await
        .unwrap();
    }
    events::record(
        &db.pool,
        EventKind::AuditFinished,
        None,
        None,
        Some(json!({ "crawl_id": crawl_a })),
    )
    .await
    .unwrap();
    events::record(
        &db.pool,
        EventKind::EmailGiven,
        None,
        None,
        Some(json!({ "crawl_id": crawl_a })),
    )
    .await
    .unwrap();
    events::record(
        &db.pool,
        EventKind::RankorgClick,
        None,
        None,
        Some(json!({ "src": "audit" })),
    )
    .await
    .unwrap();

    let funnel = events::funnel_counts(&db.pool, 30).await.unwrap();
    let kinds: Vec<EventKind> = funnel.iter().map(|f| f.kind).collect();
    assert_eq!(
        kinds,
        EventKind::ALL,
        "every step, in funnel order, even with no events"
    );
    let by = |k: EventKind| funnel.iter().find(|f| f.kind == k).unwrap();
    assert_eq!(
        (
            by(EventKind::AuditStarted).events,
            by(EventKind::AuditStarted).unique
        ),
        (3, 2)
    );
    assert_eq!(
        (
            by(EventKind::AuditFinished).events,
            by(EventKind::AuditFinished).unique
        ),
        (1, 1)
    );
    assert_eq!(by(EventKind::EmailGiven).unique, 1);
    assert_eq!(
        (
            by(EventKind::LinkClicked).events,
            by(EventKind::LinkClicked).unique
        ),
        (0, 0)
    );
    assert_eq!(by(EventKind::RankorgClick).events, 1);
}

#[tokio::test]
async fn the_funnel_only_counts_the_window() {
    let db = TestDb::new().await;
    events::record(
        &db.pool,
        EventKind::AuditStarted,
        None,
        None,
        Some(json!({ "crawl_id": Uuid::new_v4() })),
    )
    .await
    .unwrap();
    events::record(
        &db.pool,
        EventKind::AuditFinished,
        None,
        None,
        Some(json!({ "crawl_id": Uuid::new_v4() })),
    )
    .await
    .unwrap();
    age(&db.pool, "audit_finished", 10).await;
    let week = events::funnel_counts(&db.pool, 7).await.unwrap();
    let month = events::funnel_counts(&db.pool, 30).await.unwrap();
    assert_eq!(week[1].events, 0);
    assert_eq!(month[1].events, 1);
    assert_eq!(week[0].events, 1);
}

#[tokio::test]
async fn agent_events_have_their_own_funnel_and_stay_out_of_the_websites() {
    let db = TestDb::new().await;
    let crawl = Uuid::new_v4();
    // The website: an audit started and an email given. Agents: two audits, an email, a click.
    events::record(
        &db.pool,
        EventKind::AuditStarted,
        None,
        None,
        Some(json!({ "crawl_id": crawl })),
    )
    .await
    .unwrap();
    events::record(&db.pool, EventKind::EmailGiven, None, None, None)
        .await
        .unwrap();
    for _ in 0..2 {
        events::record(
            &db.pool,
            EventKind::AuditStarted,
            None,
            None,
            Some(json!({ "crawl_id": Uuid::new_v4(), "source": "agent" })),
        )
        .await
        .unwrap();
    }
    for kind in [
        EventKind::EmailGiven,
        EventKind::LinkClicked,
        EventKind::FirstFullCrawl,
    ] {
        events::record(
            &db.pool,
            kind,
            None,
            None,
            Some(json!({ "source": "agent" })),
        )
        .await
        .unwrap();
    }
    // A website crawl's own payload names its source too ("web", or "audit" for a first crawl).
    events::record(
        &db.pool,
        EventKind::AuditFinished,
        None,
        None,
        Some(json!({ "crawl_id": crawl, "source": "web" })),
    )
    .await
    .unwrap();

    let by =
        |f: &[events::FunnelCount], k: EventKind| f.iter().find(|c| c.kind == k).unwrap().events;
    let web = events::funnel_counts(&db.pool, 30).await.unwrap();
    assert_eq!(by(&web, EventKind::AuditStarted), 1);
    assert_eq!(by(&web, EventKind::EmailGiven), 1);
    assert_eq!(by(&web, EventKind::LinkClicked), 0);
    assert_eq!(by(&web, EventKind::FirstFullCrawl), 0);
    assert_eq!(by(&web, EventKind::AuditFinished), 1);
    let agents = events::agent_funnel_counts(&db.pool, 30).await.unwrap();
    assert_eq!(by(&agents, EventKind::AuditStarted), 2);
    assert_eq!(by(&agents, EventKind::EmailGiven), 1);
    assert_eq!(by(&agents, EventKind::LinkClicked), 1);
    assert_eq!(by(&agents, EventKind::FirstFullCrawl), 1);
    assert_eq!(by(&agents, EventKind::AuditFinished), 0);
}

#[tokio::test]
async fn failed_jobs_come_back_newest_first_with_their_error() {
    let db = TestDb::new().await;
    let queue = JobQueue::new(db.pool.clone());
    for i in 0..3 {
        let id = queue
            .enqueue(JobKind::SendEmail, json!({ "n": i }))
            .await
            .unwrap();
        sqlx::query(
            "UPDATE jobs SET status = 'failed', attempt = 5, last_error = $2, \
             created_at = now() - ($3 || ' minutes')::interval WHERE id = $1",
        )
        .bind(id)
        .bind(format!("smtp down {i}"))
        .bind((30 - i * 10).to_string())
        .execute(&db.pool)
        .await
        .unwrap();
    }
    queue.enqueue(JobKind::Cleanup, json!({})).await.unwrap(); // queued, not failed

    let failed = jobs::failed_jobs(&db.pool, 2).await.unwrap();
    assert_eq!(failed.len(), 2, "limited");
    assert_eq!(
        failed[0].last_error.as_deref(),
        Some("smtp down 2"),
        "newest first"
    );
    assert_eq!(failed[0].kind, JobKind::SendEmail);
    assert_eq!(failed[0].attempt, 5);
}

#[tokio::test]
async fn queue_depth_counts_waiting_quick_audits_only() {
    let db = TestDb::new().await;
    assert_eq!(quick::queue_depth(&db.pool).await.unwrap(), 0);
    for d in ["a.com", "b.com"] {
        quick::start(
            &db.pool,
            &quick::StartRequest {
                domain: d,
                start_url: &format!("https://{d}/"),
                claim_hash: d.as_bytes(),
                ip_hash: None,
                limits: quick::Limits::NONE,
                source: quick::Source::Web,
                agent_daily_budget: None,
                previous_ip_hash: None,
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(quick::queue_depth(&db.pool).await.unwrap(), 2);
    sqlx::query("UPDATE crawls SET status = 'running' WHERE domain = 'a.com'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(quick::queue_depth(&db.pool).await.unwrap(), 1);
}

#[tokio::test]
async fn top_pages_are_the_indexable_pages_with_the_most_inlinks() {
    let db = TestDb::new().await;
    let site: Uuid = sqlx::query_scalar(
        "INSERT INTO sites (domain, start_url) VALUES ('example.com', 'https://example.com/') RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let crawl: Uuid = sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status) \
         VALUES ($1, 'example.com', 'quick', 0, 'done') RETURNING id",
    )
    .bind(site)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    // (path, status, indexability, inlinks)
    let pages = [
        ("/popular", 200, "indexable", 90),
        ("/second", 200, "indexable", 50),
        ("/third", 200, "indexable", 50), // ties keep crawl order
        ("/noindex-hub", 200, "noindex", 500),
        ("/gone", 404, "client_error", 400),
        ("/moved", 301, "redirected", 300),
        ("/quiet", 200, "indexable", 1),
    ];
    for (i, (path, status, indexability, inlinks)) in pages.iter().enumerate() {
        sqlx::query(
            "INSERT INTO pages (crawl_id, site_id, url, url_hash, status, indexability, inlinks) \
             VALUES ($1, $2, $3, $4, $5, $6::indexability, $7)",
        )
        .bind(crawl)
        .bind(site)
        .bind(format!("https://example.com{path}"))
        .bind(i as i64)
        .bind(*status as i16)
        .bind(indexability)
        .bind(*inlinks)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    let top = reports::top_pages_by_inlinks(&db.pool, crawl, 3)
        .await
        .unwrap();
    assert_eq!(
        top,
        vec![
            "https://example.com/popular",
            "https://example.com/second",
            "https://example.com/third"
        ]
    );
    let all = reports::top_pages_by_inlinks(&db.pool, crawl, 10)
        .await
        .unwrap();
    assert_eq!(all.len(), 4, "only indexable 200s: {all:?}");
    assert_eq!(
        all.last().map(String::as_str),
        Some("https://example.com/quiet")
    );
    assert!(
        reports::top_pages_by_inlinks(&db.pool, Uuid::new_v4(), 10)
            .await
            .unwrap()
            .is_empty()
    );
}

/// The admin funnel scans `events` for a whole month; production's web role gives every
/// statement 5 s. On a pool whose sessions have a 1 s limit and a table that is locked for 2 s,
/// both funnel queries must still answer: they lift the limit for their own transaction.
#[tokio::test]
async fn the_funnels_outlast_a_short_statement_timeout() {
    use std::time::Duration;

    use sqlx::postgres::PgPoolOptions;

    let db = TestDb::new().await;
    events::record(&db.pool, EventKind::AuditStarted, None, None, None)
        .await
        .unwrap();
    let options = (*db.pool.connect_options())
        .clone()
        .options([("statement_timeout", "1000")]);
    let limited = PgPoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .unwrap();

    let mut lock = db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE events IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *lock)
        .await
        .unwrap();
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(2000)).await;
        lock.commit().await.unwrap();
    });
    let web = events::funnel_counts(&limited, 30).await.unwrap();
    let agents = events::agent_funnel_counts(&limited, 30).await.unwrap();
    release.await.unwrap();
    assert_eq!(web[0].events, 1);
    assert_eq!(agents[0].events, 0);
}
