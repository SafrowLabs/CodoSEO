//! Plan expiry: paid accounts whose time ran out drop to Free, extra sites stop being
//! monitored (oldest stay), nothing is deleted.

mod support;

use codoseo_store::plans::{apply_plan_limits, downgrade_expired};
use support::TestDb;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

async fn account(db: &TestDb, email: &str, plan: &str, expires_in_days: Option<i64>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO accounts (email, plan, plan_expires_at) \
         VALUES ($1, $2::plan, now() + make_interval(days => $3)) RETURNING id",
    )
    .bind(email)
    .bind(plan)
    .bind(expires_in_days.map(|d| d as i32))
    .fetch_one(&db.pool)
    .await
    .expect("insert account")
}

/// Sites created `age_days` ago, so "oldest" is well defined.
async fn site(db: &TestDb, account: Uuid, domain: &str, age_days: i32, schedule: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO sites (account_id, domain, start_url, schedule, next_crawl_at, created_at) \
         VALUES ($1, $2, 'https://x/', $3, now() + interval '1 day', now() - make_interval(days => $4)) \
         RETURNING id",
    )
    .bind(account)
    .bind(domain)
    .bind(schedule)
    .bind(age_days)
    .fetch_one(&db.pool)
    .await
    .expect("insert site")
}

async fn site_state(db: &TestDb, id: Uuid) -> (bool, Option<String>, Option<OffsetDateTime>) {
    sqlx::query_as("SELECT monitoring_active, schedule, next_crawl_at FROM sites WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .expect("site state")
}

async fn plan_of(db: &TestDb, id: Uuid) -> (String, Option<OffsetDateTime>) {
    sqlx::query_as("SELECT plan::text, plan_expires_at FROM accounts WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .expect("plan")
}

#[tokio::test]
async fn an_expired_plan_drops_to_free_and_keeps_only_the_oldest_site_monitored() {
    let db = TestDb::new().await;
    let pro = account(&db, "pro@example.test", "pro", Some(-1)).await;
    let oldest = site(&db, pro, "a.test", 30, "daily").await;
    let middle = site(&db, pro, "b.test", 20, "daily").await;
    let newest = site(&db, pro, "c.test", 10, "weekly").await;

    let moved = downgrade_expired(&db.pool, OffsetDateTime::now_utc())
        .await
        .expect("downgrade");
    assert_eq!(moved, 1);

    assert_eq!(plan_of(&db, pro).await, ("free".to_owned(), None));
    let (active, schedule, next) = site_state(&db, oldest).await;
    assert!(active);
    assert_eq!(
        schedule.as_deref(),
        Some("weekly"),
        "daily is capped to weekly"
    );
    assert_eq!(next, None, "the scheduler recomputes the slot");
    assert!(!site_state(&db, middle).await.0);
    assert!(!site_state(&db, newest).await.0);
    let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM sites WHERE account_id = $1")
        .bind(pro)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(kept, 3, "no site is deleted");
}

#[tokio::test]
async fn only_expired_paid_plans_are_downgraded() {
    let db = TestDb::new().await;
    let running = account(&db, "running@example.test", "agency", Some(5)).await;
    let selfhost = account(&db, "self@example.test", "self_hosted", None).await;
    let free = account(&db, "free@example.test", "free", None).await;
    let gone = account(&db, "gone@example.test", "agency", Some(-2)).await;
    let s1 = site(&db, running, "r.test", 3, "daily").await;
    let s2 = site(&db, running, "r2.test", 2, "daily").await;

    let moved = downgrade_expired(&db.pool, OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert_eq!(moved, 1);
    assert_eq!(plan_of(&db, gone).await.0, "free");
    assert_eq!(plan_of(&db, running).await.0, "agency");
    assert_eq!(plan_of(&db, selfhost).await.0, "self_hosted");
    assert_eq!(plan_of(&db, free).await.0, "free");
    assert!(site_state(&db, s1).await.0 && site_state(&db, s2).await.0);

    // Time is the caller's: six days on, the running plan has expired too.
    let again = downgrade_expired(&db.pool, OffsetDateTime::now_utc() + Duration::days(6))
        .await
        .unwrap();
    assert_eq!(again, 1);
    assert_eq!(plan_of(&db, running).await.0, "free");
    assert!(site_state(&db, s1).await.0);
    assert!(!site_state(&db, s2).await.0);
}

#[tokio::test]
async fn limits_keep_the_oldest_active_sites_and_leave_inactive_ones_alone() {
    let db = TestDb::new().await;
    let id = account(&db, "free@example.test", "free", None).await;
    let paused_old = site(&db, id, "old.test", 40, "weekly").await;
    sqlx::query("UPDATE sites SET monitoring_active = false WHERE id = $1")
        .bind(paused_old)
        .execute(&db.pool)
        .await
        .unwrap();
    let kept = site(&db, id, "kept.test", 30, "weekly").await;
    let dropped = site(&db, id, "dropped.test", 5, "weekly").await;

    let mut conn = db.pool.acquire().await.unwrap();
    apply_plan_limits(&mut conn, id).await.expect("apply");
    drop(conn);

    assert!(!site_state(&db, paused_old).await.0, "stays inactive");
    assert!(site_state(&db, kept).await.0);
    assert!(!site_state(&db, dropped).await.0);
}

#[tokio::test]
async fn limits_do_nothing_within_the_plan() {
    let db = TestDb::new().await;
    let id = account(&db, "pro@example.test", "pro", Some(10)).await;
    let a = site(&db, id, "a.test", 3, "daily").await;
    let b = site(&db, id, "b.test", 2, "daily").await;
    let mut tx = db.pool.begin().await.unwrap();
    apply_plan_limits(&mut tx, id).await.unwrap();
    tx.commit().await.unwrap();
    for s in [a, b] {
        let (active, schedule, next) = site_state(&db, s).await;
        assert!(active);
        assert_eq!(schedule.as_deref(), Some("daily"));
        assert!(next.is_some());
    }
}
