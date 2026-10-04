//! T6.1: no-signup audits in the store: starting one (with the 24 h reuse window), reading one
//! back, claiming one for an account, and the no-retry rule for failed quick crawls.

mod support;

use codoseo_store::crawl_queue::CrawlQueue;
use codoseo_store::quick::{self, ClaimOutcome, StartOutcome, StartRequest};
use sqlx::PgPool;
use support::TestDb;
use uuid::Uuid;

fn request<'a>(domain: &'a str, start_url: &'a str, claim: &'a [u8]) -> StartRequest<'a> {
    StartRequest {
        domain,
        start_url,
        claim_hash: claim,
        ip_hash: Some(b"ip-hash-1"),
    }
}

async fn start(pool: &PgPool, domain: &str, claim: &[u8]) -> StartOutcome {
    let url = format!("https://{domain}/");
    quick::start(pool, &request(domain, &url, claim))
        .await
        .expect("start")
}

fn crawl_id(outcome: &StartOutcome) -> Uuid {
    match outcome {
        StartOutcome::Started { crawl_id }
        | StartOutcome::Cached { crawl_id }
        | StartOutcome::Joined { crawl_id } => *crawl_id,
    }
}

async fn set_status(pool: &PgPool, crawl: Uuid, status: &str, age: &str) {
    sqlx::query(
        "UPDATE crawls SET status = $2::crawl_status, created_at = now() - $3::interval, \
         finished_at = CASE WHEN $2 = 'done' THEN now() - $3::interval END WHERE id = $1",
    )
    .bind(crawl)
    .bind(status)
    .bind(age)
    .execute(pool)
    .await
    .expect("set status");
}

async fn account(pool: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO accounts (email, email_canonical) VALUES ($1, $1) RETURNING id")
        .bind(email)
        .fetch_one(pool)
        .await
        .expect("account")
}

async fn crawl_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM crawls")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_fresh_audit_is_an_accountless_site_with_a_lane_zero_quick_crawl() {
    let db = TestDb::new().await;
    let outcome = start(&db.pool, "example.com", b"claim-1").await;
    let StartOutcome::Started { crawl_id } = outcome else {
        panic!("expected Started, got {outcome:?}");
    };

    let (account, claim, domain): (Option<Uuid>, Option<Vec<u8>>, String) = sqlx::query_as(
        "SELECT s.account_id, s.claim_token_hash, s.domain FROM sites s \
         JOIN crawls c ON c.site_id = s.id WHERE c.id = $1",
    )
    .bind(crawl_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(account, None);
    assert_eq!(claim.as_deref(), Some(&b"claim-1"[..]));
    assert_eq!(domain, "example.com");

    let (trigger, priority, source, ip, status): (
        String,
        i16,
        Option<String>,
        Option<Vec<u8>>,
        String,
    ) = sqlx::query_as(
        "SELECT trigger::text, priority, source, requester_ip_hash, status::text \
             FROM crawls WHERE id = $1",
    )
    .bind(crawl_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(trigger, "quick");
    assert_eq!(priority, 0);
    assert_eq!(source.as_deref(), Some("web"));
    assert_eq!(ip.as_deref(), Some(&b"ip-hash-1"[..]));
    assert_eq!(status, "queued");
}

#[tokio::test]
async fn the_same_domain_joins_a_running_audit_then_reuses_the_finished_one() {
    let db = TestDb::new().await;
    let first = crawl_id(&start(&db.pool, "example.com", b"a").await);

    // Still queued: a second visitor joins it.
    let second = start(&db.pool, "example.com", b"b").await;
    assert!(matches!(second, StartOutcome::Joined { crawl_id } if crawl_id == first));

    // Running counts as in flight too.
    set_status(&db.pool, first, "running", "1 minute").await;
    let third = start(&db.pool, "example.com", b"c").await;
    assert!(matches!(third, StartOutcome::Joined { crawl_id } if crawl_id == first));

    // Done within 24 h: the cached report.
    set_status(&db.pool, first, "done", "23 hours").await;
    let fourth = start(&db.pool, "example.com", b"d").await;
    assert!(matches!(fourth, StartOutcome::Cached { crawl_id } if crawl_id == first));

    assert_eq!(
        crawl_count(&db.pool).await,
        1,
        "nobody started a second crawl"
    );
}

#[tokio::test]
async fn an_old_or_failed_audit_is_not_reused() {
    let db = TestDb::new().await;
    let first = crawl_id(&start(&db.pool, "example.com", b"a").await);
    set_status(&db.pool, first, "done", "25 hours").await;
    let second = start(&db.pool, "example.com", b"b").await;
    assert!(matches!(second, StartOutcome::Started { .. }), "{second:?}");

    set_status(&db.pool, crawl_id(&second), "failed", "1 minute").await;
    let third = start(&db.pool, "example.com", b"c").await;
    assert!(matches!(third, StartOutcome::Started { .. }), "{third:?}");

    // A different domain is a different audit.
    let other = start(&db.pool, "other.com", b"d").await;
    assert!(matches!(other, StartOutcome::Started { .. }));
}

#[tokio::test]
async fn two_submits_at_the_same_moment_start_one_crawl() {
    let db = TestDb::new().await;
    let (a, b) = tokio::join!(
        start(&db.pool, "example.com", b"a"),
        start(&db.pool, "example.com", b"b")
    );
    assert_eq!(crawl_id(&a), crawl_id(&b));
    assert_eq!(crawl_count(&db.pool).await, 1);
    let started = [&a, &b]
        .iter()
        .filter(|o| matches!(o, StartOutcome::Started { .. }))
        .count();
    assert_eq!(started, 1, "exactly one of them started it: {a:?} {b:?}");
}

#[tokio::test]
async fn get_serves_quick_crawls_only() {
    let db = TestDb::new().await;
    let quick_id = crawl_id(&start(&db.pool, "example.com", b"a").await);
    let audit = quick::get(&db.pool, quick_id)
        .await
        .unwrap()
        .expect("found");
    assert_eq!(audit.domain, "example.com");
    assert!(!audit.claimed);
    assert_eq!(audit.crawl.id, quick_id);

    // An ordinary crawl of a signed-in user's site is invisible, as is a random id.
    let owner = account(&db.pool, "owner@example.com").await;
    let site: Uuid = sqlx::query_scalar(
        "INSERT INTO sites (account_id, domain, start_url) VALUES ($1, 'mine.com', 'https://mine.com/') RETURNING id",
    )
    .bind(owner)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let first: Uuid = sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, trigger, priority) VALUES ($1, 'mine.com', 'first', 1) RETURNING id",
    )
    .bind(site)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(quick::get(&db.pool, first).await.unwrap().is_none());
    assert!(
        quick::get(&db.pool, Uuid::new_v4())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn claiming_with_the_cookie_attaches_the_site_and_queues_the_first_crawl() {
    let db = TestDb::new().await;
    let crawl = crawl_id(&start(&db.pool, "example.com", b"secret").await);
    let ana = account(&db.pool, "ana@example.com").await;

    let outcome = quick::claim(
        &db.pool,
        ana,
        crawl,
        Some(b"secret"),
        Some(1),
        1,
        Some("weekly"),
    )
    .await
    .unwrap();
    let ClaimOutcome::Attached(site) = outcome else {
        panic!("expected Attached, got {outcome:?}");
    };
    assert_eq!(site.account_id, Some(ana));
    assert_eq!(site.schedule.as_deref(), Some("weekly"));

    let claim: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT claim_token_hash FROM sites WHERE id = $1")
            .bind(site.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(claim, None, "the claim token is spent");
    let (priority, trigger): (i16, String) = sqlx::query_as(
        "SELECT priority, trigger::text FROM crawls WHERE site_id = $1 AND trigger = 'first'",
    )
    .bind(site.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!((priority, trigger.as_str()), (1, "first"));
}

#[tokio::test]
async fn claiming_without_the_cookie_creates_a_fresh_site_and_leaves_the_audit_alone() {
    let db = TestDb::new().await;
    let crawl = crawl_id(&start(&db.pool, "example.com", b"secret").await);
    let ana = account(&db.pool, "ana@example.com").await;

    for cookie in [None, Some(&b"wrong"[..])] {
        let _ = sqlx::query("DELETE FROM sites WHERE account_id = $1")
            .bind(ana)
            .execute(&db.pool)
            .await;
        let outcome = quick::claim(&db.pool, ana, crawl, cookie, Some(1), 1, Some("weekly"))
            .await
            .unwrap();
        let ClaimOutcome::Created(site) = outcome else {
            panic!("expected Created, got {outcome:?}");
        };
        assert_eq!(site.domain, "example.com");
        assert_eq!(site.account_id, Some(ana));
    }
    let original: Option<Uuid> = sqlx::query_scalar(
        "SELECT s.account_id FROM sites s JOIN crawls c ON c.site_id = s.id WHERE c.id = $1",
    )
    .bind(crawl)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(original, None, "the visitor's own audit stays unclaimed");
}

#[tokio::test]
async fn claiming_respects_existing_sites_and_the_plan_limit() {
    let db = TestDb::new().await;
    let crawl = crawl_id(&start(&db.pool, "example.com", b"secret").await);

    // Already has this domain: nothing new is queued.
    let ana = account(&db.pool, "ana@example.com").await;
    sqlx::query("INSERT INTO sites (account_id, domain, start_url) VALUES ($1, 'example.com', 'https://example.com/')")
        .bind(ana)
        .execute(&db.pool)
        .await
        .unwrap();
    let before = crawl_count(&db.pool).await;
    let outcome = quick::claim(
        &db.pool,
        ana,
        crawl,
        Some(b"secret"),
        Some(1),
        1,
        Some("weekly"),
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome, ClaimOutcome::Existing(ref s) if s.domain == "example.com"),
        "{outcome:?}"
    );
    assert_eq!(crawl_count(&db.pool).await, before);

    // Already at the plan's site limit with a different site.
    let bo = account(&db.pool, "bo@example.com").await;
    sqlx::query("INSERT INTO sites (account_id, domain, start_url) VALUES ($1, 'bo.com', 'https://bo.com/')")
        .bind(bo)
        .execute(&db.pool)
        .await
        .unwrap();
    let outcome = quick::claim(
        &db.pool,
        bo,
        crawl,
        Some(b"secret"),
        Some(1),
        1,
        Some("weekly"),
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome, ClaimOutcome::LimitReached(Some(ref s)) if s.domain == "bo.com"),
        "{outcome:?}"
    );

    // Not a quick crawl at all.
    let cy = account(&db.pool, "cy@example.com").await;
    let outcome = quick::claim(&db.pool, cy, Uuid::new_v4(), None, Some(1), 1, None)
        .await
        .unwrap();
    assert!(matches!(outcome, ClaimOutcome::NotFound));
}

#[tokio::test]
async fn a_failed_quick_crawl_is_not_retried_but_other_crawls_still_are() {
    let db = TestDb::new().await;
    let queue = CrawlQueue::new(db.pool.clone());

    let quick_id = crawl_id(&start(&db.pool, "example.com", b"a").await);
    let claimed = queue.claim("w1").await.unwrap().expect("claimed");
    assert_eq!(claimed.id, quick_id);
    queue
        .finish_failed(quick_id, "site unreachable: dns", "w1")
        .await
        .unwrap();
    let (status, reason): (String, Option<String>) =
        sqlx::query_as("SELECT status::text, failure_reason FROM crawls WHERE id = $1")
            .bind(quick_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        status, "failed",
        "a visitor can't wait 15 minutes for a retry"
    );
    assert_eq!(reason.as_deref(), Some("site unreachable: dns"));

    // The retry rule for everything else is unchanged.
    let owner = account(&db.pool, "owner@example.com").await;
    let site: Uuid = sqlx::query_scalar(
        "INSERT INTO sites (account_id, domain, start_url) VALUES ($1, 'mine.com', 'https://mine.com/') RETURNING id",
    )
    .bind(owner)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let manual = queue
        .enqueue(
            site,
            "mine.com",
            codoseo_store::crawl_queue::CrawlTrigger::Manual,
            2,
            None,
            None,
        )
        .await
        .unwrap();
    queue.claim("w1").await.unwrap().expect("claimed");
    queue.finish_failed(manual, "boom", "w1").await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status::text FROM crawls WHERE id = $1")
        .bind(manual)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        status, "queued",
        "a first failure of a normal crawl still requeues"
    );
}
