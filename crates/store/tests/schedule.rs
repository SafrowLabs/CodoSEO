//! The scheduler's store side: which sites are due, that two schedulers never share a site,
//! and the once-only guards for digests and the daily cleanup.

mod support;

use codoseo_core::plan::{Plan, Schedule};
use codoseo_store::schedule::{DueBatch, claim_due_sites};
use support::TestDb;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

async fn account(db: &TestDb, email: &str, plan: &str, paused: bool) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO accounts (email, plan, paused, timezone) \
         VALUES ($1, $2::plan, $3, 'Asia/Kolkata') RETURNING id",
    )
    .bind(email)
    .bind(plan)
    .bind(paused)
    .fetch_one(&db.pool)
    .await
    .expect("insert account")
}

/// A monitored site due `due_in` from now (`None`: never given a slot).
async fn site(
    db: &TestDb,
    account: Option<Uuid>,
    domain: &str,
    schedule: Option<&str>,
    due_in: Option<Duration>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO sites (account_id, domain, start_url, schedule, scheduled_hour, next_crawl_at) \
         VALUES ($1, $2, 'https://x/', $3, 7, $4) RETURNING id",
    )
    .bind(account)
    .bind(domain)
    .bind(schedule)
    .bind(due_in.map(|d| OffsetDateTime::now_utc() + d))
    .fetch_one(&db.pool)
    .await
    .expect("insert site")
}

fn ids(batch: &DueBatch) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = batch.sites.iter().map(|s| s.id).collect();
    ids.sort();
    ids
}

async fn crawls_of(db: &TestDb, site: Uuid) -> Vec<(String, String, i16)> {
    sqlx::query_as(
        "SELECT trigger::text, status::text, priority FROM crawls WHERE site_id = $1 ORDER BY created_at",
    )
    .bind(site)
    .fetch_all(&db.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn only_monitored_sites_of_active_accounts_with_a_schedule_are_due() {
    let db = TestDb::new().await;
    let a = account(&db, "a@example.test", "pro", false).await;
    let paused = account(&db, "p@example.test", "free", true).await;
    let due = site(
        &db,
        Some(a),
        "due.test",
        Some("daily"),
        Some(Duration::minutes(-5)),
    )
    .await;
    let never = site(&db, Some(a), "never.test", Some("weekly"), None).await;
    let _later = site(
        &db,
        Some(a),
        "later.test",
        Some("daily"),
        Some(Duration::hours(2)),
    )
    .await;
    let _no_schedule = site(&db, Some(a), "manual.test", None, Some(Duration::hours(-1))).await;
    let _paused = site(
        &db,
        Some(paused),
        "paused.test",
        Some("weekly"),
        Some(Duration::hours(-1)),
    )
    .await;
    let _unowned = site(
        &db,
        None,
        "quick.test",
        Some("weekly"),
        Some(Duration::hours(-1)),
    )
    .await;
    let off = site(
        &db,
        Some(a),
        "off.test",
        Some("weekly"),
        Some(Duration::hours(-1)),
    )
    .await;
    sqlx::query("UPDATE sites SET monitoring_active = false WHERE id = $1")
        .bind(off)
        .execute(&db.pool)
        .await
        .unwrap();

    let batch = claim_due_sites(&db.pool, OffsetDateTime::now_utc(), 100)
        .await
        .expect("claim");
    let mut expected = vec![due, never];
    expected.sort();
    assert_eq!(ids(&batch), expected);

    let found = batch.sites.iter().find(|s| s.id == due).unwrap();
    assert_eq!(found.domain, "due.test");
    assert_eq!(found.plan, Plan::Pro);
    assert_eq!(found.schedule, Some(Schedule::Daily));
    assert_eq!(found.scheduled_hour, Some(7));
    assert_eq!(found.timezone, "Asia/Kolkata");
    assert!(found.next_crawl_at.is_some());
    let fresh = batch.sites.iter().find(|s| s.id == never).unwrap();
    assert_eq!(fresh.next_crawl_at, None);
}

#[tokio::test]
async fn the_limit_caps_a_batch() {
    let db = TestDb::new().await;
    let a = account(&db, "a@example.test", "pro", false).await;
    for i in 0..5 {
        site(
            &db,
            Some(a),
            &format!("s{i}.test"),
            Some("daily"),
            Some(Duration::minutes(-1)),
        )
        .await;
    }
    let batch = claim_due_sites(&db.pool, OffsetDateTime::now_utc(), 3)
        .await
        .unwrap();
    assert_eq!(batch.sites.len(), 3);
}

#[tokio::test]
async fn two_schedulers_never_get_the_same_site() {
    let db = TestDb::new().await;
    let a = account(&db, "a@example.test", "pro", false).await;
    for i in 0..6 {
        site(
            &db,
            Some(a),
            &format!("s{i}.test"),
            Some("daily"),
            Some(Duration::minutes(-1)),
        )
        .await;
    }
    let now = OffsetDateTime::now_utc();
    // The first batch is still open (uncommitted) while the second one claims.
    let (first, second) = tokio::join!(
        async {
            let b = claim_due_sites(&db.pool, now, 4).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            b
        },
        async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            claim_due_sites(&db.pool, now, 100).await.unwrap()
        }
    );
    let first_ids = ids(&first);
    let second_ids = ids(&second);
    assert_eq!(first_ids.len(), 4);
    assert_eq!(second_ids.len(), 2, "only what the first left over");
    assert!(first_ids.iter().all(|id| !second_ids.contains(id)));
}

#[tokio::test]
async fn advance_queues_one_crawl_and_moves_the_slot_into_the_future() {
    let db = TestDb::new().await;
    let a = account(&db, "a@example.test", "pro", false).await;
    let s = site(
        &db,
        Some(a),
        "late.test",
        Some("weekly"),
        Some(Duration::days(-21)),
    )
    .await;
    let now = OffsetDateTime::now_utc();
    let next = now + Duration::days(3);

    let mut batch = claim_due_sites(&db.pool, now, 100).await.unwrap();
    let site_row = batch.sites[0].clone();
    assert!(batch.advance(&site_row, Some(3), next).await.unwrap());
    batch.commit().await.unwrap();

    assert_eq!(
        crawls_of(&db, s).await,
        vec![("schedule".to_owned(), "queued".to_owned(), 3)]
    );
    let stored: OffsetDateTime =
        sqlx::query_scalar("SELECT next_crawl_at FROM sites WHERE id = $1")
            .bind(s)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!((stored - next).abs() < Duration::milliseconds(5));
    let again = claim_due_sites(&db.pool, now, 100).await.unwrap();
    assert!(
        again.sites.is_empty(),
        "no back-fill: nothing is due any more"
    );
}

#[tokio::test]
async fn a_site_with_a_queued_or_running_crawl_gets_no_second_one() {
    let db = TestDb::new().await;
    let a = account(&db, "a@example.test", "pro", false).await;
    let queued = site(
        &db,
        Some(a),
        "q.test",
        Some("daily"),
        Some(Duration::minutes(-1)),
    )
    .await;
    let running = site(
        &db,
        Some(a),
        "r.test",
        Some("daily"),
        Some(Duration::minutes(-1)),
    )
    .await;
    sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status) \
         VALUES ($1, 'q.test', 'manual', 2, 'queued'), ($2, 'r.test', 'manual', 2, 'running')",
    )
    .bind(queued)
    .bind(running)
    .execute(&db.pool)
    .await
    .unwrap();
    let now = OffsetDateTime::now_utc();
    let mut batch = claim_due_sites(&db.pool, now, 100).await.unwrap();
    let rows = batch.sites.clone();
    for s in &rows {
        let queued_one = batch
            .advance(s, Some(3), now + Duration::days(1))
            .await
            .unwrap();
        assert!(!queued_one, "{} already has a crawl", s.domain);
    }
    batch.commit().await.unwrap();
    assert_eq!(crawls_of(&db, queued).await.len(), 1);
    assert_eq!(crawls_of(&db, running).await.len(), 1);
    // The slot still moved on.
    let due_now = claim_due_sites(&db.pool, now, 100).await.unwrap();
    assert!(due_now.sites.is_empty());
}

#[tokio::test]
async fn advance_without_a_priority_only_sets_the_slot() {
    let db = TestDb::new().await;
    let a = account(&db, "a@example.test", "free", false).await;
    let s = site(&db, Some(a), "new.test", Some("weekly"), None).await;
    let now = OffsetDateTime::now_utc();
    let mut batch = claim_due_sites(&db.pool, now, 100).await.unwrap();
    let row = batch.sites[0].clone();
    assert!(
        !batch
            .advance(&row, None, now + Duration::days(2))
            .await
            .unwrap()
    );
    batch.commit().await.unwrap();
    assert!(crawls_of(&db, s).await.is_empty());
    let next: Option<OffsetDateTime> =
        sqlx::query_scalar("SELECT next_crawl_at FROM sites WHERE id = $1")
            .bind(s)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(next.is_some());
}

async fn done_crawl(db: &TestDb, site: Uuid, domain: &str, finished_days_ago: i32) {
    sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, finished_at) \
         VALUES ($1, $2, 'first', 1, 'done', now() - make_interval(days => $3))",
    )
    .bind(site)
    .bind(domain)
    .bind(finished_days_ago)
    .execute(&db.pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn digests_go_to_unpaused_accounts_with_a_finished_crawl_once() {
    use codoseo_store::schedule::{digest_candidates, enqueue_digest};
    let db = TestDb::new().await;
    let ready = account(&db, "ready@example.test", "pro", false).await;
    let no_crawl = account(&db, "none@example.test", "pro", false).await;
    let paused = account(&db, "paused@example.test", "free", true).await;
    let recent = account(&db, "recent@example.test", "pro", false).await;
    for (acct, domain) in [
        (ready, "a.test"),
        (no_crawl, "b.test"),
        (paused, "c.test"),
        (recent, "d.test"),
    ] {
        let s = site(
            &db,
            Some(acct),
            domain,
            Some("weekly"),
            Some(Duration::days(1)),
        )
        .await;
        if acct != no_crawl {
            done_crawl(&db, s, domain, 1).await;
        }
    }
    sqlx::query("UPDATE accounts SET last_digest_at = now() - interval '1 day' WHERE id = $1")
        .bind(recent)
        .execute(&db.pool)
        .await
        .unwrap();

    let now = OffsetDateTime::now_utc();
    let found = digest_candidates(&db.pool, now).await.unwrap();
    assert_eq!(found.iter().map(|c| c.id).collect::<Vec<_>>(), vec![ready]);
    assert_eq!(found[0].timezone, "Asia/Kolkata");
    assert_eq!(found[0].last_digest_at, None);

    // Two schedulers hold the same candidate: only one enqueues.
    let candidate = found[0].clone();
    assert!(enqueue_digest(&db.pool, &candidate, now).await.unwrap());
    assert!(!enqueue_digest(&db.pool, &candidate, now).await.unwrap());
    let jobs: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT kind::text, payload FROM jobs")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        jobs,
        vec![(
            "send_digest".to_owned(),
            serde_json::json!({"account_id": ready})
        )]
    );
    let stamped: Option<OffsetDateTime> =
        sqlx::query_scalar("SELECT last_digest_at FROM accounts WHERE id = $1")
            .bind(ready)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(stamped.is_some_and(|t| (t - now).abs() < Duration::milliseconds(5)));
    assert!(digest_candidates(&db.pool, now).await.unwrap().is_empty());
}

#[tokio::test]
async fn the_daily_cleanup_is_enqueued_once_per_day() {
    use codoseo_store::schedule::enqueue_daily_cleanup;
    let db = TestDb::new().await;
    let now = OffsetDateTime::now_utc();
    assert!(
        enqueue_daily_cleanup(&db.pool, "2026-10-05", now)
            .await
            .unwrap()
    );
    assert!(
        !enqueue_daily_cleanup(&db.pool, "2026-10-05", now)
            .await
            .unwrap()
    );
    assert!(
        enqueue_daily_cleanup(&db.pool, "2026-10-06", now)
            .await
            .unwrap()
    );
    assert!(
        !enqueue_daily_cleanup(&db.pool, "2026-10-06", now)
            .await
            .unwrap()
    );
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'cleanup'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(queued, 2);
}

#[tokio::test]
async fn the_cleanup_day_only_moves_forward() {
    use codoseo_store::schedule::enqueue_daily_cleanup;
    let db = TestDb::new().await;
    let now = OffsetDateTime::now_utc();
    assert!(
        enqueue_daily_cleanup(&db.pool, "2026-10-06", now)
            .await
            .unwrap()
    );
    // A scheduler whose clock is still on the 5th must not flip the day back (which would let
    // the 6th queue a second cleanup).
    assert!(
        !enqueue_daily_cleanup(&db.pool, "2026-10-05", now)
            .await
            .unwrap()
    );
    assert!(
        !enqueue_daily_cleanup(&db.pool, "2026-10-06", now)
            .await
            .unwrap()
    );
    let day: serde_json::Value =
        sqlx::query_scalar("SELECT value FROM instance_settings WHERE key = 'last_cleanup_on'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(day, "2026-10-06");
    assert!(
        enqueue_daily_cleanup(&db.pool, "2026-10-07", now)
            .await
            .unwrap()
    );
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'cleanup'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(queued, 2);
}

#[tokio::test]
async fn signing_in_again_clears_the_warning_and_unpauses_free_accounts_only() {
    let db = TestDb::new().await;
    let free = account(&db, "free@example.test", "free", true).await;
    let pro = account(&db, "pro@example.test", "pro", true).await;
    sqlx::query("UPDATE accounts SET keep_monitoring_sent_at = now() - interval '9 days'")
        .execute(&db.pool)
        .await
        .unwrap();
    codoseo_store::accounts::touch_login(&db.pool, free)
        .await
        .unwrap();
    codoseo_store::accounts::touch_login(&db.pool, pro)
        .await
        .unwrap();
    let state = |id| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_as::<_, (bool, Option<OffsetDateTime>, Option<OffsetDateTime>)>(
                "SELECT paused, keep_monitoring_sent_at, last_login_at FROM accounts WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let (paused, sent, login) = state(free).await;
    assert!(!paused && sent.is_none() && login.is_some());
    let (paused, sent, login) = state(pro).await;
    assert!(paused, "paid accounts are never unpaused by a login");
    assert!(sent.is_none() && login.is_some());
}

#[tokio::test]
async fn resuming_monitoring_unpauses_and_counts_as_an_email_click() {
    use codoseo_store::accounts::resume_monitoring;
    let db = TestDb::new().await;
    let id = account(&db, "free@example.test", "free", true).await;
    sqlx::query(
        "UPDATE accounts SET keep_monitoring_sent_at = now() - interval '8 days' WHERE id = $1",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    let mut tx = db.pool.begin().await.unwrap();
    resume_monitoring(&mut *tx, id).await.unwrap();
    tx.commit().await.unwrap();
    let (paused, sent, click): (bool, Option<OffsetDateTime>, Option<OffsetDateTime>) =
        sqlx::query_as("SELECT paused, keep_monitoring_sent_at, last_email_click_at FROM accounts WHERE id = $1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(!paused && sent.is_none() && click.is_some());
}

#[tokio::test]
async fn a_resume_token_works_once() {
    use codoseo_store::auth::{TokenPurpose, consume_token, create_token};
    let db = TestDb::new().await;
    let id = account(&db, "free@example.test", "free", false).await;
    create_token(
        &db.pool,
        TokenPurpose::ResumeMonitoring,
        b"hash-1",
        Some(id),
        None,
        Duration::days(7),
    )
    .await
    .unwrap();
    let wrong = consume_token(&db.pool, TokenPurpose::MagicLink, b"hash-1")
        .await
        .unwrap();
    assert!(wrong.is_none(), "another purpose can't use it");
    let used = consume_token(&db.pool, TokenPurpose::ResumeMonitoring, b"hash-1")
        .await
        .unwrap();
    assert_eq!(used.and_then(|t| t.account_id), Some(id));
    assert!(
        consume_token(&db.pool, TokenPurpose::ResumeMonitoring, b"hash-1")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_used_token_can_be_made_usable_again_until_it_expires() {
    use codoseo_store::auth::{
        TokenPurpose, consume_token, create_token, token_is_live, unconsume_token,
    };
    let db = TestDb::new().await;
    let purpose = TokenPurpose::StartMonitoring;
    create_token(&db.pool, purpose, b"h", None, None, Duration::hours(24))
        .await
        .unwrap();
    // Not used yet: nothing to undo.
    assert!(!unconsume_token(&db.pool, purpose, b"h").await.unwrap());
    consume_token(&db.pool, purpose, b"h")
        .await
        .unwrap()
        .unwrap();
    assert!(!token_is_live(&db.pool, purpose, b"h").await.unwrap());
    // Another purpose can't undo it.
    assert!(
        !unconsume_token(&db.pool, TokenPurpose::MagicLink, b"h")
            .await
            .unwrap()
    );
    assert!(unconsume_token(&db.pool, purpose, b"h").await.unwrap());
    assert!(token_is_live(&db.pool, purpose, b"h").await.unwrap());
    // And it works once again, once.
    assert!(
        consume_token(&db.pool, purpose, b"h")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        consume_token(&db.pool, purpose, b"h")
            .await
            .unwrap()
            .is_none()
    );
    // An expired token stays dead.
    sqlx::query("UPDATE login_tokens SET expires_at = now() - interval '1 minute'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(!unconsume_token(&db.pool, purpose, b"h").await.unwrap());
}

// ---- the warning bookkeeping ---------------------------------------------------------------

/// A warned Free account whose latest warning email job ended `status`.
async fn warned_account(db: &TestDb, email: &str, paused: bool, status: &str) -> Uuid {
    let id = account(db, email, "free", paused).await;
    sqlx::query(
        "UPDATE accounts SET keep_monitoring_sent_at = now() - interval '2 days' WHERE id = $1",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO jobs (kind, payload, status) \
         VALUES ('send_email', jsonb_build_object('keep_monitoring_for', $1::uuid), $2::job_status)",
    )
    .bind(id)
    .bind(status)
    .execute(&db.pool)
    .await
    .unwrap();
    id
}

async fn warned_at(db: &TestDb, id: Uuid) -> Option<OffsetDateTime> {
    sqlx::query_scalar("SELECT keep_monitoring_sent_at FROM accounts WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_failed_warning_is_reset_for_active_accounts_but_not_paused_ones() {
    let db = TestDb::new().await;
    let active = warned_account(&db, "active@example.test", false, "failed").await;
    let paused = warned_account(&db, "paused@example.test", true, "failed").await;
    let delivered = warned_account(&db, "ok@example.test", false, "done").await;

    let reset = codoseo_store::schedule::reset_failed_warnings(&db.pool)
        .await
        .unwrap();
    assert_eq!(reset, 1);
    assert!(
        warned_at(&db, active).await.is_none(),
        "warned again next tick"
    );
    assert!(
        warned_at(&db, paused).await.is_some(),
        "a paused account is left alone"
    );
    assert!(warned_at(&db, delivered).await.is_some());
}
