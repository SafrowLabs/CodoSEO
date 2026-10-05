//! T6.1: no-signup audits in the store: starting one (with the 24 h reuse window), reading one
//! back, claiming one for an account, and the no-retry rule for failed quick crawls.

mod support;

use codoseo_store::crawl_queue::CrawlQueue;
use codoseo_store::quick::{
    self, ClaimOutcome, LimitWindow, Limits, MonitoringCaps, MonitoringSlot, Requester, Source,
    StartOutcome, StartRequest, UnlockCaps, UnlockSlot,
};
use sqlx::PgPool;
use support::TestDb;
use uuid::Uuid;

fn request<'a>(domain: &'a str, start_url: &'a str, claim: &'a [u8]) -> StartRequest<'a> {
    StartRequest {
        domain,
        start_url,
        claim_hash: claim,
        ip_hash: Some(b"ip-hash-1"),
        limits: Limits::NONE,
        source: Source::Web,
        agent_daily_budget: None,
        previous_ip_hash: None,
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
        StartOutcome::Limited { .. } | StartOutcome::AgentBudgetReached { .. } => {
            panic!("limited: {outcome:?}")
        }
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
    set_status(&db.pool, crawl, "done", "1 minute").await;
    let ana = account(&db.pool, "ana@example.com").await;

    let outcome = quick::claim(
        &db.pool,
        ana,
        crawl,
        &[b"secret".to_vec()],
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

    for cookie in [&[][..], &[b"wrong".to_vec()][..]] {
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
        &[b"secret".to_vec()],
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
        &[b"secret".to_vec()],
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
    let outcome = quick::claim(&db.pool, cy, Uuid::new_v4(), &[], Some(1), 1, None)
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

async fn start_from(pool: &PgPool, domain: &str, ip: &[u8], limits: Limits) -> StartOutcome {
    let url = format!("https://{domain}/");
    quick::start(
        pool,
        &StartRequest {
            domain,
            start_url: &url,
            claim_hash: domain.as_bytes(),
            ip_hash: Some(ip),
            limits,
            source: Source::Web,
            agent_daily_budget: None,
            previous_ip_hash: None,
        },
    )
    .await
    .expect("start")
}

async fn age_all(pool: &PgPool, age: &str) {
    sqlx::query(
        "UPDATE crawls SET created_at = now() - $1::interval, queued_at = now() - $1::interval",
    )
    .bind(age)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn an_ip_gets_three_audits_an_hour_and_ten_a_day() {
    let db = TestDb::new().await;
    let limits = Limits::DEFAULT;
    assert_eq!((limits.per_hour, limits.per_day), (3, 10));

    for i in 0..3 {
        let outcome = start_from(&db.pool, &format!("site{i}.com"), b"ip-a", limits).await;
        assert!(
            matches!(outcome, StartOutcome::Started { .. }),
            "{i}: {outcome:?}"
        );
    }
    // The fourth in the hour is turned away, told when the oldest one ages out.
    let outcome = start_from(&db.pool, "site3.com", b"ip-a", limits).await;
    let StartOutcome::Limited {
        window,
        retry_after_secs,
    } = outcome
    else {
        panic!("expected Limited, got {outcome:?}");
    };
    assert_eq!(window, LimitWindow::Hour);
    assert!(
        (3590..=3600).contains(&retry_after_secs),
        "{retry_after_secs}"
    );
    assert_eq!(
        crawl_count(&db.pool).await,
        3,
        "a refused audit starts nothing"
    );

    // Another visitor is unaffected.
    let outcome = start_from(&db.pool, "site3.com", b"ip-b", limits).await;
    assert!(
        matches!(outcome, StartOutcome::Started { .. }),
        "{outcome:?}"
    );

    // Audits that are already running or cached cost nothing and are never refused.
    let outcome = start_from(&db.pool, "site0.com", b"ip-a", limits).await;
    assert!(
        matches!(outcome, StartOutcome::Joined { .. }),
        "{outcome:?}"
    );

    // Ten in a day: age the hour-long window away between rounds.
    let db = TestDb::new().await;
    for round in 0..3 {
        for i in 0..3 {
            let outcome = start_from(&db.pool, &format!("r{round}s{i}.com"), b"ip-a", limits).await;
            assert!(
                matches!(outcome, StartOutcome::Started { .. }),
                "{round}/{i}: {outcome:?}"
            );
        }
        age_all(&db.pool, "2 hours").await;
    }
    let tenth = start_from(&db.pool, "tenth.com", b"ip-a", limits).await;
    assert!(matches!(tenth, StartOutcome::Started { .. }), "{tenth:?}");
    let eleventh = start_from(&db.pool, "eleventh.com", b"ip-a", limits).await;
    let StartOutcome::Limited {
        window,
        retry_after_secs,
    } = eleventh
    else {
        panic!("expected Limited, got {eleventh:?}");
    };
    assert_eq!(window, LimitWindow::Day);
    assert!(
        retry_after_secs > 3600,
        "a day limit isn't lifted within the hour: {retry_after_secs}"
    );
}

#[tokio::test]
async fn a_visitor_with_no_ip_hash_is_not_limited_per_ip() {
    let db = TestDb::new().await;
    for i in 0..5 {
        let url = format!("https://site{i}.com/");
        let outcome = quick::start(
            &db.pool,
            &StartRequest {
                domain: &format!("site{i}.com"),
                start_url: &url,
                claim_hash: format!("site{i}.com").as_bytes(),
                ip_hash: None,
                limits: Limits::DEFAULT,
                source: Source::Web,
                agent_daily_budget: None,
                previous_ip_hash: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(outcome, StartOutcome::Started { .. }));
    }
}

#[tokio::test]
async fn at_most_eight_quick_crawls_run_at_once_and_the_ninth_waits_its_turn() {
    let db = TestDb::new().await;
    let queue = CrawlQueue::new(db.pool.clone());
    let mut ids = Vec::new();
    for i in 0..9 {
        let domain = format!("site{i}.com");
        ids.push(crawl_id(&start(&db.pool, &domain, domain.as_bytes()).await));
        // Distinct queue times, oldest first.
        sqlx::query(
            "UPDATE crawls SET queued_at = now() - ($2 || ' seconds')::interval WHERE id = $1",
        )
        .bind(ids[i])
        .bind((100 - i).to_string())
        .execute(&db.pool)
        .await
        .unwrap();
    }

    // The queue position counts the quick crawls ahead, from 1.
    assert_eq!(
        quick::queue_position(&db.pool, ids[0]).await.unwrap(),
        Some(1)
    );
    assert_eq!(
        quick::queue_position(&db.pool, ids[8]).await.unwrap(),
        Some(9)
    );

    for _ in 0..8 {
        assert!(queue.claim("w").await.unwrap().is_some());
    }
    assert!(
        queue.claim("w").await.unwrap().is_none(),
        "the ninth quick crawl waits while eight are running"
    );
    assert_eq!(
        quick::queue_position(&db.pool, ids[8]).await.unwrap(),
        Some(1)
    );
    assert_eq!(
        quick::queue_position(&db.pool, ids[0]).await.unwrap(),
        None,
        "running isn't queued"
    );

    // An ordinary crawl is not held up by the quick cap.
    let owner = account(&db.pool, "owner@example.com").await;
    let site: Uuid = sqlx::query_scalar(
        "INSERT INTO sites (account_id, domain, start_url) VALUES ($1, 'mine.com', 'https://mine.com/') RETURNING id",
    )
    .bind(owner)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    queue
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
    let claimed = queue
        .claim("w")
        .await
        .unwrap()
        .expect("the manual crawl runs");
    assert_eq!(claimed.domain, "mine.com");

    // One quick crawl finishing frees a slot for the ninth.
    set_status(&db.pool, ids[0], "done", "1 minute").await;
    let claimed = queue.claim("w").await.unwrap().expect("the ninth now runs");
    assert_eq!(claimed.id, ids[8]);
}

fn unlock_payload(crawl: Uuid, canonical: &str) -> serde_json::Value {
    serde_json::json!({ "email": canonical, "canonical": canonical, "audit": crawl })
}

async fn unlock(pool: &PgPool, crawl: Uuid, canonical: &str, n: u32) -> UnlockSlot {
    quick::create_unlock_token(
        pool,
        crawl,
        canonical,
        format!("{crawl}-{canonical}-{n}").as_bytes(),
        unlock_payload(crawl, canonical),
        time::Duration::minutes(15),
        UnlockCaps::DEFAULT,
    )
    .await
    .expect("unlock token")
}

#[tokio::test]
async fn unlock_emails_are_capped_per_audit_and_per_address_each_hour() {
    let db = TestDb::new().await;
    let one = crawl_id(&start(&db.pool, "one.com", b"one").await);
    let two = crawl_id(&start(&db.pool, "two.com", b"two").await);

    // Three different people on one audit, then the audit is full.
    for i in 0..3 {
        assert_eq!(
            unlock(&db.pool, one, &format!("p{i}@x.co"), 0).await,
            UnlockSlot::Created
        );
    }
    assert_eq!(
        unlock(&db.pool, one, "p9@x.co", 0).await,
        UnlockSlot::AuditCapReached
    );
    assert_eq!(
        unlock(&db.pool, two, "p9@x.co", 0).await,
        UnlockSlot::Created,
        "another audit has room"
    );

    // One address can't be sent mail through many audits: p9 has one, give it two more.
    assert_eq!(
        unlock(&db.pool, two, "p9@x.co", 1).await,
        UnlockSlot::Created
    );
    let three = crawl_id(&start(&db.pool, "three.com", b"three").await);
    assert_eq!(
        unlock(&db.pool, three, "p9@x.co", 2).await,
        UnlockSlot::Created
    );
    let four = crawl_id(&start(&db.pool, "four.com", b"four").await);
    assert_eq!(
        unlock(&db.pool, four, "p9@x.co", 3).await,
        UnlockSlot::AddressCapReached,
        "a fourth email to the same address in an hour is refused whichever audit asks"
    );
    // ...and a refused request stores nothing.
    let stored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM login_tokens WHERE payload->>'canonical' = 'p9@x.co'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(stored, 3);

    // The hour rolls over.
    sqlx::query("UPDATE login_tokens SET created_at = now() - interval '2 hours'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        unlock(&db.pool, one, "p9@x.co", 4).await,
        UnlockSlot::Created
    );
}

#[tokio::test]
async fn ten_unlock_requests_at_once_store_exactly_three() {
    let db = TestDb::new().await;
    let crawl = crawl_id(&start(&db.pool, "one.com", b"one").await);
    let results =
        futures_util::future::join_all((0..10).map(|n| unlock(&db.pool, crawl, "victim@x.co", n)))
            .await;
    let created = results
        .iter()
        .filter(|r| **r == UnlockSlot::Created)
        .count();
    assert_eq!(created, 3, "{results:?}");
}

#[tokio::test]
async fn limits_count_the_previous_days_hash_too() {
    let db = TestDb::new().await;
    let limits = Limits {
        per_hour: 2,
        per_day: 10,
    };
    // Two audits started 20 minutes ago under yesterday's salt (just before UTC midnight).
    for i in 0..2 {
        let outcome =
            start_from(&db.pool, &format!("site{i}.com"), b"hash-yesterday", limits).await;
        assert!(matches!(outcome, StartOutcome::Started { .. }));
    }
    age_all(&db.pool, "20 minutes").await;

    // Now it is after midnight: the visitor's hash is new, but yesterday's is counted as well.
    let url = "https://site2.com/";
    let outcome = quick::start(
        &db.pool,
        &StartRequest {
            domain: "site2.com",
            start_url: url,
            claim_hash: b"c2",
            ip_hash: Some(b"hash-today"),
            previous_ip_hash: Some(b"hash-yesterday"),
            limits,
            source: Source::Web,
            agent_daily_budget: None,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(
            outcome,
            StartOutcome::Limited {
                window: LimitWindow::Hour,
                ..
            }
        ),
        "{outcome:?}"
    );
    // Without the previous hash the rollover would have reset the count.
    let outcome = quick::start(
        &db.pool,
        &StartRequest {
            domain: "site2.com",
            start_url: url,
            claim_hash: b"c3",
            ip_hash: Some(b"hash-today"),
            previous_ip_hash: None,
            limits,
            source: Source::Web,
            agent_daily_budget: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(outcome, StartOutcome::Started { .. }));
}

#[tokio::test]
async fn a_stale_quick_crawl_fails_instead_of_looping_but_others_still_requeue() {
    let db = TestDb::new().await;
    let queue = CrawlQueue::new(db.pool.clone());
    let quick_id = crawl_id(&start(&db.pool, "example.com", b"a").await);
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
    queue.claim("w1").await.unwrap().expect("quick");
    queue.claim("w1").await.unwrap().expect("manual");
    sqlx::query(
        "UPDATE crawls SET heartbeat_at = now() - interval '10 minutes' WHERE status = 'running'",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let moved = queue
        .requeue_stale(std::time::Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(moved, 2);
    let (status, reason): (String, Option<String>) =
        sqlx::query_as("SELECT status::text, failure_reason FROM crawls WHERE id = $1")
            .bind(quick_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(status, "failed");
    assert!(reason.unwrap().contains("stopped"));
    let status: String = sqlx::query_scalar("SELECT status::text FROM crawls WHERE id = $1")
        .bind(manual)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(status, "queued");
}

#[tokio::test]
async fn any_of_several_claim_tokens_attaches_the_audit() {
    let db = TestDb::new().await;
    let a = crawl_id(&start(&db.pool, "a.com", b"token-a").await);
    let _b = crawl_id(&start(&db.pool, "b.com", b"token-b").await);
    set_status(&db.pool, a, "done", "1 minute").await;
    let ana = account(&db.pool, "ana@example.com").await;
    // The cookie holds both tokens, in either order.
    let outcome = quick::claim(
        &db.pool,
        ana,
        a,
        &[b"token-b".to_vec(), b"token-a".to_vec()],
        Some(5),
        1,
        Some("weekly"),
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome, ClaimOutcome::Attached(ref s) if s.domain == "a.com"),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn an_audit_still_running_is_not_attached_and_the_first_crawl_is_marked_as_from_an_audit() {
    let db = TestDb::new().await;
    let crawl = crawl_id(&start(&db.pool, "example.com", b"secret").await);
    let ana = account(&db.pool, "ana@example.com").await;

    // Queued: the quick crawl would be governed by the account's larger limits if attached.
    let outcome = quick::claim(
        &db.pool,
        ana,
        crawl,
        &[b"secret".to_vec()],
        Some(5),
        1,
        Some("weekly"),
    )
    .await
    .unwrap();
    let ClaimOutcome::Created(site) = outcome else {
        panic!("expected Created while still queued, got {outcome:?}");
    };
    let source: Option<String> =
        sqlx::query_scalar("SELECT source FROM crawls WHERE site_id = $1 AND trigger = 'first'")
            .bind(site.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(source.as_deref(), Some("audit"));
    let original: Option<Uuid> = sqlx::query_scalar(
        "SELECT s.account_id FROM crawls c JOIN sites s ON s.id = c.site_id WHERE c.id = $1",
    )
    .bind(crawl)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(original, None);

    // Once it has ended, the same token attaches it.
    sqlx::query("DELETE FROM sites WHERE account_id = $1")
        .bind(ana)
        .execute(&db.pool)
        .await
        .unwrap();
    set_status(&db.pool, crawl, "done", "1 minute").await;
    let outcome = quick::claim(
        &db.pool,
        ana,
        crawl,
        &[b"secret".to_vec()],
        Some(5),
        1,
        Some("weekly"),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, ClaimOutcome::Attached(_)), "{outcome:?}");
}

// ---- agents: the daily budget and the start-monitoring token caps (M8 T4) ----

async fn agent_start(pool: &PgPool, domain: &str, ip: Option<&[u8]>, budget: i64) -> StartOutcome {
    let url = format!("https://{domain}/");
    quick::start(
        pool,
        &StartRequest {
            domain,
            start_url: &url,
            claim_hash: domain.as_bytes(),
            ip_hash: ip,
            previous_ip_hash: None,
            limits: Limits::NONE,
            source: Source::Agent,
            agent_daily_budget: Some(budget),
        },
    )
    .await
    .expect("start")
}

#[tokio::test]
async fn an_agent_audit_is_marked_as_one_and_spends_the_budget_only_when_fresh() {
    let db = TestDb::new().await;
    let first = crawl_id(&agent_start(&db.pool, "a.com", None, 2).await);
    let source: Option<String> = sqlx::query_scalar("SELECT source FROM crawls WHERE id = $1")
        .bind(first)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(source.as_deref(), Some("agent"));

    // Joining and reusing cost nothing, even with the budget spent.
    assert!(matches!(
        agent_start(&db.pool, "a.com", None, 1).await,
        StartOutcome::Joined { crawl_id } if crawl_id == first
    ));
    // Older than the hour that the hourly share counts, still inside the 24 h reuse window.
    set_status(&db.pool, first, "done", "2 hours").await;
    assert!(matches!(
        agent_start(&db.pool, "a.com", None, 1).await,
        StartOutcome::Cached { crawl_id } if crawl_id == first
    ));
    assert_eq!(crawl_count(&db.pool).await, 1);

    // A second fresh audit uses the second slot; the third is refused and stores nothing.
    assert!(matches!(
        agent_start(&db.pool, "b.com", None, 2).await,
        StartOutcome::Started { .. }
    ));
    let refused = agent_start(&db.pool, "c.com", None, 2).await;
    let StartOutcome::AgentBudgetReached { retry_after_secs } = refused else {
        panic!("expected the budget to be spent: {refused:?}");
    };
    assert!(retry_after_secs > 0 && retry_after_secs <= 24 * 3600);
    assert_eq!(crawl_count(&db.pool).await, 2);
    let sites: i64 = sqlx::query_scalar("SELECT count(*) FROM sites")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(sites, 2);

    // The window is the last 24 hours.
    age_all(&db.pool, "25 hours").await;
    assert!(matches!(
        agent_start(&db.pool, "c.com", None, 2).await,
        StartOutcome::Started { .. }
    ));
}

#[tokio::test]
async fn website_audits_do_not_spend_the_agent_budget() {
    let db = TestDb::new().await;
    for i in 0..3 {
        start(&db.pool, &format!("web{i}.com"), format!("c{i}").as_bytes()).await;
    }
    assert!(matches!(
        agent_start(&db.pool, "agent.com", None, 1).await,
        StartOutcome::Started { .. }
    ));
    // And the budget only applies to agents: the website passes none.
    assert!(matches!(
        start(&db.pool, "web9.com", b"c9").await,
        StartOutcome::Started { .. }
    ));
}

#[tokio::test]
async fn twenty_agent_audits_at_once_with_five_left_start_exactly_five() {
    let db = TestDb::new().await;
    for i in 0..195 {
        let id = crawl_id(&agent_start(&db.pool, &format!("old{i}.com"), None, 1000).await);
        set_status(&db.pool, id, "done", "1 hour").await;
    }
    let domains: Vec<String> = (0..20).map(|i| format!("new{i}.com")).collect();
    let results = futures_util::future::join_all(
        domains
            .iter()
            .map(|domain| agent_start(&db.pool, domain, None, 200)),
    )
    .await;
    let started = results
        .iter()
        .filter(|r| matches!(r, StartOutcome::Started { .. }))
        .count();
    let refused = results
        .iter()
        .filter(|r| matches!(r, StartOutcome::AgentBudgetReached { .. }))
        .count();
    assert_eq!((started, refused), (5, 15), "{results:?}");
    assert_eq!(crawl_count(&db.pool).await, 200);
}

#[tokio::test]
async fn a_direct_agent_client_is_limited_per_ip_like_the_website_but_not_without_a_hash() {
    let db = TestDb::new().await;
    let limits = Limits {
        per_hour: 1,
        per_day: 10,
    };
    // One audit from the website, then the same address through an agent is over the hour limit.
    start_from(&db.pool, "web.com", b"ip-x", limits).await;
    let url = "https://agent.com/";
    let outcome = quick::start(
        &db.pool,
        &StartRequest {
            domain: "agent.com",
            start_url: url,
            claim_hash: b"c",
            ip_hash: Some(b"ip-x"),
            previous_ip_hash: None,
            limits,
            source: Source::Agent,
            agent_daily_budget: Some(200),
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(
            outcome,
            StartOutcome::Limited {
                window: LimitWindow::Hour,
                ..
            }
        ),
        "{outcome:?}"
    );
    // A shared client has no hash, so nothing per IP applies.
    assert!(matches!(
        agent_start(&db.pool, "agent.com", None, 200).await,
        StartOutcome::Started { .. }
    ));
}

#[tokio::test]
async fn agents_get_an_eighth_of_the_daily_budget_in_any_hour() {
    let db = TestDb::new().await;
    // A budget of 16 a day is 2 an hour.
    for n in 0..2 {
        assert!(matches!(
            agent_start(&db.pool, &format!("h{n}.com"), None, 16).await,
            StartOutcome::Started { .. }
        ));
    }
    let refused = agent_start(&db.pool, "h2.com", None, 16).await;
    let StartOutcome::AgentBudgetReached { retry_after_secs } = refused else {
        panic!("expected the hourly share to be spent: {refused:?}");
    };
    assert!(
        (1..=3600).contains(&retry_after_secs),
        "an hour at most: {retry_after_secs}"
    );
    // Reuse is free, and the next hour has room again.
    assert!(matches!(
        agent_start(&db.pool, "h0.com", None, 16).await,
        StartOutcome::Joined { .. }
    ));
    age_all(&db.pool, "90 minutes").await;
    assert!(matches!(
        agent_start(&db.pool, "h2.com", None, 16).await,
        StartOutcome::Started { .. }
    ));
    // The share is at least 1, however small the budget.
    assert_eq!(quick::hourly_share(0), 1);
    assert_eq!(quick::hourly_share(7), 1);
    assert_eq!(quick::hourly_share(200), 25);
}

#[tokio::test]
async fn www_and_the_bare_domain_are_one_site_for_reuse() {
    let db = TestDb::new().await;
    let first = crawl_id(&start(&db.pool, "www.example.com", b"a").await);
    // Either spelling joins the running audit, and then reuses the finished one.
    let bare = start(&db.pool, "example.com", b"b").await;
    assert!(
        matches!(bare, StartOutcome::Joined { crawl_id } if crawl_id == first),
        "{bare:?}"
    );
    set_status(&db.pool, first, "done", "1 minute").await;
    for domain in ["example.com", "www.example.com"] {
        let out = start(&db.pool, domain, domain.as_bytes()).await;
        assert!(
            matches!(out, StartOutcome::Cached { crawl_id } if crawl_id == first),
            "{domain}: {out:?}"
        );
    }
    assert_eq!(crawl_count(&db.pool).await, 1);
    // A different host that merely contains it is another site, and a new audit keeps the
    // host that was asked for.
    let other = start(&db.pool, "blog.example.com", b"c").await;
    assert!(matches!(other, StartOutcome::Started { .. }));
    let domain: String = sqlx::query_scalar("SELECT domain FROM crawls WHERE id = $1")
        .bind(crawl_id(&other))
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(domain, "blog.example.com");
    // And at the same moment, the two spellings still start one crawl.
    let (a, b) = tokio::join!(
        start(&db.pool, "www.race.com", b"r1"),
        start(&db.pool, "race.com", b"r2")
    );
    assert_eq!(crawl_id(&a), crawl_id(&b), "{a:?} {b:?}");
}

#[tokio::test]
async fn status_is_one_cheap_read_of_a_quick_audit() {
    let db = TestDb::new().await;
    let id = crawl_id(&start(&db.pool, "example.com", b"a").await);
    let queued = quick::status(&db.pool, id).await.unwrap();
    assert_eq!(
        queued,
        Some((codoseo_store::crawls::CrawlStatus::Queued, None))
    );
    sqlx::query(
        "UPDATE crawls SET status = 'running', progress = $2 WHERE id = $1",
    )
    .bind(id)
    .bind(serde_json::json!({"pages_done": 12, "queued": 3, "failures": 0, "depth": 1, "elapsed_ms": 100}))
    .execute(&db.pool)
    .await
    .unwrap();
    let (status, pages) = quick::status(&db.pool, id).await.unwrap().unwrap();
    assert_eq!(status, codoseo_store::crawls::CrawlStatus::Running);
    assert_eq!(pages, Some(12));
    // Not a quick audit, or not there at all.
    assert_eq!(quick::status(&db.pool, Uuid::new_v4()).await.unwrap(), None);
    let site: Uuid = sqlx::query_scalar("SELECT site_id FROM crawls WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let other: Uuid = sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, trigger, priority) VALUES ($1, 'x.com', 'manual', 2) RETURNING id",
    )
    .bind(site)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(quick::status(&db.pool, other).await.unwrap(), None);
}

fn monitoring_payload(canonical: &str) -> serde_json::Value {
    serde_json::json!({ "email": canonical, "canonical": canonical, "domain": "example.com" })
}

async fn monitoring(
    pool: &PgPool,
    canonical: &str,
    ip: Option<&[u8]>,
    n: u32,
    caps: MonitoringCaps,
) -> MonitoringSlot {
    quick::create_monitoring_token(
        pool,
        canonical,
        ip.map(|ip_hash| Requester {
            ip_hash,
            previous_ip_hash: None,
        }),
        format!("{canonical}-{n}").as_bytes(),
        monitoring_payload(canonical),
        time::Duration::hours(24),
        caps,
    )
    .await
    .expect("monitoring token")
}

#[tokio::test]
async fn monitoring_emails_are_capped_per_address_per_client_and_per_day() {
    let db = TestDb::new().await;
    let caps = MonitoringCaps {
        per_day: 7,
        per_hour: 100,
        ..MonitoringCaps::with_daily(7)
    };

    // Three to one address, the fourth is refused, whoever asks.
    for n in 0..3 {
        assert_eq!(
            monitoring(&db.pool, "p@x.co", None, n, caps).await,
            MonitoringSlot::Created
        );
    }
    assert_eq!(
        monitoring(&db.pool, "p@x.co", None, 3, caps).await,
        MonitoringSlot::AddressCapReached
    );
    assert_eq!(
        monitoring(&db.pool, "p@x.co", Some(b"other-ip"), 4, caps).await,
        MonitoringSlot::AddressCapReached
    );

    // One client can't mail many addresses: three an hour.
    for n in 0..3 {
        assert_eq!(
            monitoring(&db.pool, &format!("q{n}@x.co"), Some(b"ip-1"), 0, caps).await,
            MonitoringSlot::Created
        );
    }
    assert_eq!(
        monitoring(&db.pool, "q9@x.co", Some(b"ip-1"), 0, caps).await,
        MonitoringSlot::IpCapReached
    );
    // A caller without a hash (a shared connector) has no per-client cap...
    assert_eq!(
        monitoring(&db.pool, "q9@x.co", None, 0, caps).await,
        MonitoringSlot::Created
    );
    // ...but the daily total (3 + 3 + 1 = 7 so far) stops everyone.
    assert_eq!(
        monitoring(&db.pool, "r@x.co", None, 0, caps).await,
        MonitoringSlot::DailyCapReached
    );
    assert_eq!(
        monitoring(&db.pool, "r@x.co", Some(b"ip-2"), 0, caps).await,
        MonitoringSlot::DailyCapReached
    );

    // Refused requests store nothing; the window rolls over.
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM login_tokens")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(stored, 7);
    sqlx::query("UPDATE login_tokens SET created_at = now() - interval '25 hours'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        monitoring(&db.pool, "p@x.co", Some(b"ip-1"), 9, caps).await,
        MonitoringSlot::Created
    );
}

#[tokio::test]
async fn an_address_gets_five_emails_in_24_hours_not_three_an_hour_all_day() {
    let db = TestDb::new().await;
    let caps = MonitoringCaps {
        per_hour: 100,
        ..MonitoringCaps::with_daily(1000)
    };
    for n in 0..3 {
        assert_eq!(
            monitoring(&db.pool, "victim@x.co", None, n, caps).await,
            MonitoringSlot::Created
        );
    }
    // The hour passes: two more fit, the sixth in the day does not.
    sqlx::query("UPDATE login_tokens SET created_at = now() - interval '2 hours'")
        .execute(&db.pool)
        .await
        .unwrap();
    for n in 3..5 {
        assert_eq!(
            monitoring(&db.pool, "victim@x.co", None, n, caps).await,
            MonitoringSlot::Created
        );
    }
    assert_eq!(
        monitoring(&db.pool, "victim@x.co", None, 5, caps).await,
        MonitoringSlot::AddressDayCapReached
    );
    // Spelling variants count as the same address (the canonical form is what is counted),
    // other addresses are untouched, and the day rolls over.
    assert_eq!(
        monitoring(&db.pool, "other@x.co", None, 6, caps).await,
        MonitoringSlot::Created
    );
    sqlx::query("UPDATE login_tokens SET created_at = now() - interval '25 hours'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        monitoring(&db.pool, "victim@x.co", None, 7, caps).await,
        MonitoringSlot::Created
    );
}

#[tokio::test]
async fn the_daily_email_budget_cannot_be_drained_in_one_hour() {
    let db = TestDb::new().await;
    // 16 a day is 2 an hour.
    let caps = MonitoringCaps::with_daily(16);
    assert_eq!(caps.per_hour, 2);
    for n in 0..2 {
        assert_eq!(
            monitoring(&db.pool, &format!("p{n}@x.co"), None, n, caps).await,
            MonitoringSlot::Created
        );
    }
    assert_eq!(
        monitoring(&db.pool, "p9@x.co", None, 9, caps).await,
        MonitoringSlot::HourlyCapReached
    );
    sqlx::query("UPDATE login_tokens SET created_at = now() - interval '90 minutes'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        monitoring(&db.pool, "p9@x.co", None, 9, caps).await,
        MonitoringSlot::Created
    );
    assert_eq!(MonitoringCaps::with_daily(3).per_hour, 1);
}

#[tokio::test]
async fn a_monitoring_token_keeps_the_client_hash_for_the_counts_and_ten_at_once_store_three() {
    let db = TestDb::new().await;
    let caps = MonitoringCaps::with_daily(100);
    monitoring(&db.pool, "p@x.co", Some(b"ip-1"), 0, caps).await;
    let ip: Option<String> = sqlx::query_scalar(
        "SELECT payload->>'ip' FROM login_tokens WHERE purpose = 'start_monitoring'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(ip.as_deref(), Some("69702d31"));

    let results = futures_util::future::join_all(
        (1..11).map(|n| monitoring(&db.pool, "victim@x.co", None, n, caps)),
    )
    .await;
    let created = results
        .iter()
        .filter(|r| **r == MonitoringSlot::Created)
        .count();
    assert_eq!(created, 3, "{results:?}");
}
