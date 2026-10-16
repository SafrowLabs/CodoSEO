//! The site policy in real crawls: the default policy changes nothing, a declared policy sets
//! its checks and keeps its report in `crawls.summary.policy`, a must-work page answering 500
//! records an instant `must_work_failed` change on every crawl it keeps failing, and the plan
//! caps the must-work list.

#[allow(dead_code)]
mod support;

use codoseo::worker::{DEFAULT_MEMORY_BUDGET, worker_loop_once};
use codoseo_core::change::ChangeKind;
use codoseo_core::check::CheckId;
use codoseo_core::crawl::AddressPolicy;
use codoseo_policy::{IndexRule, IndexStance, PathPattern, SitePolicy};
use codoseo_store::crawl_queue::{CrawlQueue, CrawlTrigger};
use codoseo_store::jobs::JobQueue;
use codoseo_store::policy::{Actor, SaveError};
use codoseo_testkit::{Page, SiteBuilder};
use support::TestDb;

/// A site owned by an account on `plan`: (site, account).
async fn owned_site(db: &TestDb, start_url: &str, plan: &str) -> (uuid::Uuid, uuid::Uuid) {
    let account: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO accounts (email, email_canonical, plan) VALUES ($1, $1, $2::plan) RETURNING id",
    )
    .bind(format!("{}@example.com", uuid::Uuid::new_v4().simple()))
    .bind(plan)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let site: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO sites (account_id, domain, start_url) VALUES ($1, 'example.test', $2) \
         RETURNING id",
    )
    .bind(account)
    .bind(start_url)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    (site, account)
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
    let status: String = sqlx::query_scalar("SELECT status::text FROM crawls WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(status, "done");
    id
}

/// (path, issue bits) of each page of the crawl.
async fn page_bits(db: &TestDb, crawl: uuid::Uuid) -> Vec<(String, u64)> {
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT url, issues FROM pages WHERE crawl_id = $1 ORDER BY url")
            .bind(crawl)
            .fetch_all(&db.pool)
            .await
            .unwrap();
    rows.into_iter()
        .map(|(u, b)| {
            (
                url::Url::parse(&u).unwrap().path().to_owned(),
                codoseo_store::hash::from_db(b),
            )
        })
        .collect()
}

fn has(bits: u64, id: CheckId) -> bool {
    bits & (1 << id.bit()) != 0
}

async fn summary(db: &TestDb, crawl: uuid::Uuid) -> (serde_json::Value, i16, i16) {
    sqlx::query_as("SELECT summary, checks_passed, checks_total FROM crawls WHERE id = $1")
        .bind(crawl)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

async fn must_work_changes(
    db: &TestDb,
    crawl: uuid::Uuid,
) -> Vec<(String, String, String, String)> {
    sqlx::query_as(
        "SELECT severity::text, url, before_value, after_value FROM changes \
         WHERE crawl_id = $1 AND kind = 'must_work_failed' ORDER BY id",
    )
    .bind(crawl)
    .fetch_all(&db.pool)
    .await
    .unwrap()
}

fn shop() -> SiteBuilder {
    SiteBuilder::new()
        .html("/", "Home", &["/pricing", "/private/a"])
        .html("/pricing", "Pricing", &[])
        .html("/private/a", "Private", &[])
        // Linked from nowhere: only the must-work list leads there.
        .page("/checkout/cart", Page::status(500, "down"))
}

#[tokio::test]
async fn the_default_policy_changes_nothing() {
    let db = TestDb::new().await;
    let server = shop().start().await;
    let (site, _) = owned_site(&db, server.url("/").as_str(), "pro").await;
    let crawl = run_crawl(&db, site, CrawlTrigger::Manual).await;
    let pages = page_bits(&db, crawl).await;
    assert_eq!(
        pages.len(),
        3,
        "the unlinked page is not fetched: {pages:?}"
    );
    for (path, bits) in &pages {
        assert!(
            CheckId::POLICY.iter().all(|c| !has(*bits, *c)),
            "{path} has a policy bit"
        );
    }
    let (summary, passed, total) = summary(&db, crawl).await;
    assert_eq!(total, 44);
    assert!(passed <= 44);
    let report = &summary["policy"];
    assert_eq!(report["seo_gaps"], 0);
    for id in ["address", "index", "params", "must_work"] {
        let rule = report["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap();
        assert_eq!(rule["status"], "not_checked", "{id}");
    }
    assert!(must_work_changes(&db, crawl).await.is_empty());
}

#[tokio::test]
async fn a_declared_policy_sets_its_checks_and_a_failing_must_work_page_alerts_instantly() {
    let db = TestDb::new().await;
    let server = shop().start().await;
    let (site, account) = owned_site(&db, server.url("/").as_str(), "pro").await;
    codoseo_store::channels::ensure_default_email(
        &db.pool,
        &codoseo_notify::ChannelKey::derive("k"),
        account,
    )
    .await
    .unwrap();
    codoseo_store::alert_rules::create_defaults_for_site(&db.pool, account, site)
        .await
        .unwrap();
    let mut policy = SitePolicy::default();
    policy.seo.index = vec![IndexRule {
        pattern: PathPattern("/private/*".to_owned()),
        stance: IndexStance::KeepOut,
    }];
    policy.seo.must_work = vec!["/pricing".to_owned(), "/checkout/cart".to_owned()];
    codoseo_store::policy::save(&db.pool, site, &policy, &Actor::web(account))
        .await
        .unwrap();

    let first = run_crawl(&db, site, CrawlTrigger::Manual).await;
    let pages = page_bits(&db, first).await;
    let bits = |path: &str| {
        pages
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, b)| *b)
            .unwrap()
    };
    // The must-work page was fetched though nothing links to it, and failed.
    assert!(has(bits("/checkout/cart"), CheckId::MustWorkFailed));
    assert!(has(bits("/private/a"), CheckId::PolicyIndexMismatch));
    assert!(!has(bits("/pricing"), CheckId::MustWorkFailed));
    assert!(!has(bits("/"), CheckId::PolicyIndexMismatch));

    let (summary, passed, total) = summary(&db, first).await;
    // Index mismatch, keep-out partial and must-work failed are declared.
    assert_eq!(total, 47);
    let counts = summary["counts"].as_array().unwrap();
    assert!(
        counts
            .iter()
            .any(|c| c[0] == "must_work_failed" && c[1] == 1),
        "{counts:?}"
    );
    assert!(passed < total);
    let report = &summary["policy"];
    assert_eq!(report["seo_gaps"], 2, "{report}");
    let must = report["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "must_work")
        .unwrap();
    assert_eq!(must["status"], "gaps");
    assert_eq!(
        must["parts"].as_array().unwrap().len(),
        3,
        "home, two listed"
    );

    let changes = must_work_changes(&db, first).await;
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].0, "critical");
    assert!(changes[0].1.ends_with("/checkout/cart"));
    assert_eq!((changes[0].2.as_str(), changes[0].3.as_str()), ("", "500"));
    let planned: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = 'send_alert' AND payload->>'crawl_id' = $1",
    )
    .bind(first.to_string())
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(planned >= 1, "the change queues alert planning");
    assert!(codoseo_store::alert_rules::DEFAULT_INSTANT.contains(&ChangeKind::MustWorkFailed));
    let instant = codoseo_store::alert_rules::instant_channels_for(
        &db.pool,
        site,
        ChangeKind::MustWorkFailed,
    )
    .await
    .unwrap();
    assert_eq!(instant.len(), 1, "instant on the default channel");

    // Still failing on the next crawl: recorded again, with how it fared before.
    let second = run_crawl(&db, site, CrawlTrigger::Manual).await;
    let again = must_work_changes(&db, second).await;
    assert_eq!(again.len(), 1);
    assert_eq!((again[0].2.as_str(), again[0].3.as_str()), ("500", "500"));
}

#[tokio::test]
async fn the_plan_caps_the_must_work_list() {
    let db = TestDb::new().await;
    let (free, account) = owned_site(&db, "https://example.test/", "free").await;
    let mut policy = SitePolicy::default();
    policy.seo.must_work = (0..6).map(|i| format!("/p{i}")).collect();
    let err = codoseo_store::policy::save(&db.pool, free, &policy, &Actor::web(account))
        .await
        .unwrap_err();
    let SaveError::Invalid(errors) = &err else {
        panic!("{err}");
    };
    assert_eq!(errors[0].path, "seo.must_work");
    assert!(
        errors[0]
            .message
            .starts_with("the Free plan allows 5 must-work pages and this policy lists 6"),
        "{}",
        errors[0].message
    );
    assert!(
        codoseo_store::policy::check(&db.pool, free, &policy)
            .await
            .is_err()
    );
    // Five fit; a Pro site takes the six.
    policy.seo.must_work.pop();
    codoseo_store::policy::save(&db.pool, free, &policy, &Actor::web(account))
        .await
        .unwrap();
    let (pro, pro_account) = owned_site(&db, "https://example.test/", "pro").await;
    policy.seo.must_work.push("/p5".to_owned());
    codoseo_store::policy::save(&db.pool, pro, &policy, &Actor::web(pro_account))
        .await
        .unwrap();
}
