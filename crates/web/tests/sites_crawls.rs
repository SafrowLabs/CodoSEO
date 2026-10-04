//! T5.3: sites (the per-plan site limit, the first crawl), crawl history, Run crawl (the
//! per-site manual allowance, priority lanes, one crawl at a time) and the crawler status poll.

mod support;

use axum::http::StatusCode;
use codoseo_core::plan::Plan;
use codoseo_store::accounts::Account;
use codoseo_store::sites::Site;
use support::{TestApp, page};
use uuid::Uuid;

/// Adds a site through the form, as a user would.
async fn add_site(app: &TestApp, cookie: &str, domain: &str) -> support::TestResponse {
    app.post("/sites", &format!("url={domain}"), Some(cookie))
        .await
}

async fn site_id(app: &TestApp, account: &Account, domain: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM sites WHERE account_id = $1 AND domain = $2")
        .bind(account.id)
        .bind(domain)
        .fetch_one(app.pool())
        .await
        .expect("site exists")
}

/// A crawl created `age` ago (a Postgres interval such as `6 days 23 hours`). Old crawls are
/// `done`, so the "already queued" rule doesn't interfere with allowance tests.
async fn crawl_at(app: &TestApp, site: &Site, trigger: &str, status: &str, age: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, created_at, queued_at, \
                             started_at, finished_at) \
         VALUES ($1, $2, $3::crawl_trigger, 2, $4::crawl_status, now() - $5::interval, \
                 now() - $5::interval, now() - $5::interval, \
                 CASE WHEN $4 = 'done' THEN now() - $5::interval + interval '90 seconds' END) \
         RETURNING id",
    )
    .bind(site.id)
    .bind(&site.domain)
    .bind(trigger)
    .bind(status)
    .bind(age)
    .fetch_one(app.pool())
    .await
    .expect("insert crawl")
}

async fn set_created(app: &TestApp, crawl: Uuid, age: &str) {
    sqlx::query("UPDATE crawls SET created_at = now() - $2::interval WHERE id = $1")
        .bind(crawl)
        .bind(age)
        .execute(app.pool())
        .await
        .expect("move crawl");
}

async fn finish_all(app: &TestApp, site: &Site) {
    sqlx::query(
        "UPDATE crawls SET status = 'done', started_at = coalesce(started_at, now()), \
                finished_at = now() WHERE site_id = $1 AND status IN ('queued', 'running')",
    )
    .bind(site.id)
    .execute(app.pool())
    .await
    .expect("finish crawls");
}

async fn queued_crawls(app: &TestApp, site: &Site) -> Vec<(String, i16)> {
    sqlx::query_as(
        "SELECT trigger::text, priority FROM crawls WHERE site_id = $1 AND status = 'queued' \
         ORDER BY created_at",
    )
    .bind(site.id)
    .fetch_all(app.pool())
    .await
    .expect("queued crawls")
}

fn trigger_json(res: &support::TestResponse) -> serde_json::Value {
    let raw = res.header("hx-trigger").expect("HX-Trigger header");
    serde_json::from_str(raw).expect("HX-Trigger is JSON")
}

async fn run_crawl(app: &TestApp, site: &Site, cookie: &str) -> support::TestResponse {
    app.post_hx(&format!("/s/{}/crawls", site.id), "", Some(cookie))
        .await
}

// ── Sites ─────────────────────────────────────────────

#[tokio::test]
async fn free_plan_allows_one_site() {
    let app = TestApp::new().await;
    let (_, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Free))
        .await;
    let res = add_site(&app, &cookie, "example.com").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    let res = add_site(&app, &cookie, "second.com").await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(
        res.body.contains("Your plan includes 1 site."),
        "{}",
        res.body
    );
}

#[tokio::test]
async fn pro_plan_allows_five_sites() {
    let app = TestApp::new().await;
    let (_, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Pro))
        .await;
    for i in 1..=5 {
        let res = add_site(&app, &cookie, &format!("site{i}.com")).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "site {i}: {}", res.body);
    }
    let res = add_site(&app, &cookie, "site6.com").await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(
        res.body.contains("Your plan includes 5 sites."),
        "{}",
        res.body
    );
}

#[tokio::test]
async fn self_hosted_has_no_site_limit() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    assert_eq!(account.plan, Plan::SelfHosted);
    for i in 1..=6 {
        let res = add_site(&app, &cookie, &format!("site{i}.com")).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "site {i}: {}", res.body);
    }
}

#[tokio::test]
async fn adding_a_site_queues_its_first_crawl() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let res = add_site(&app, &cookie, "https://Example.com/blog").await;
    let id = site_id(&app, &account, "example.com").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), Some(format!("/s/{id}/audit").as_str()));

    let crawls: Vec<(String, i16, String)> = sqlx::query_as(
        "SELECT trigger::text, priority, status::text FROM crawls WHERE site_id = $1",
    )
    .bind(id)
    .fetch_all(app.pool())
    .await
    .unwrap();
    assert_eq!(crawls, vec![("first".to_owned(), 1, "queued".to_owned())]);
}

#[tokio::test]
async fn simultaneous_adds_stay_within_the_limit() {
    let app = TestApp::new().await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Free))
        .await;
    let (a, b, c) = tokio::join!(
        add_site(&app, &cookie, "a.com"),
        add_site(&app, &cookie, "b.com"),
        add_site(&app, &cookie, "c.com"),
    );
    let mut statuses = [a.status, b.status, c.status];
    statuses.sort();
    assert_eq!(
        statuses,
        [
            StatusCode::SEE_OTHER,
            StatusCode::FORBIDDEN,
            StatusCode::FORBIDDEN
        ],
        "{} | {} | {}",
        a.body,
        b.body,
        c.body
    );
    let sites: i64 = sqlx::query_scalar("SELECT count(*) FROM sites WHERE account_id = $1")
        .bind(account.id)
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(sites, 1);
    let crawls: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM crawls c JOIN sites s ON s.id = c.site_id WHERE s.account_id = $1",
    )
    .bind(account.id)
    .fetch_one(app.pool())
    .await
    .unwrap();
    assert_eq!(crawls, 1, "one first crawl, for the one site");
}

#[tokio::test]
async fn the_same_domain_cannot_be_added_twice() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let (a, b) = tokio::join!(
        add_site(&app, &cookie, "example.com"),
        add_site(&app, &cookie, "https://EXAMPLE.com/blog"),
    );
    let mut statuses = [a.status, b.status];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::SEE_OTHER, StatusCode::BAD_REQUEST]);
    let refused = if a.status == StatusCode::BAD_REQUEST {
        &a
    } else {
        &b
    };
    assert!(
        refused
            .body
            .contains("example.com is already one of your sites."),
        "{}",
        refused.body
    );
    let sites: i64 = sqlx::query_scalar("SELECT count(*) FROM sites WHERE account_id = $1")
        .bind(account.id)
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(sites, 1);
}

// ── Run crawl: the manual allowance ───────────────────

#[tokio::test]
async fn free_allows_one_manual_crawl_a_week() {
    let app = TestApp::new().await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Free))
        .await;
    let site = app.site(&account, "example.com").await;
    let old = crawl_at(&app, &site, "manual", "done", "6 days 23 hours").await;

    let res = run_crawl(&app, &site, &cookie).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert!(
        res.body.contains(
            "Your Free plan includes 1 manual crawl a week. The next one is available in 1 hour."
        ),
        "{}",
        res.body
    );
    assert!(
        res.body.contains("data-error-message"),
        "inline error for htmx"
    );
    assert!(queued_crawls(&app, &site).await.is_empty());

    set_created(&app, old, "7 days 1 minute").await;
    let res = run_crawl(&app, &site, &cookie).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    assert_eq!(
        queued_crawls(&app, &site).await,
        vec![("manual".to_owned(), 4)]
    );
}

#[tokio::test]
async fn free_limit_message_counts_days() {
    let app = TestApp::new().await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Free))
        .await;
    let site = app.site(&account, "example.com").await;
    crawl_at(&app, &site, "manual", "done", "4 days").await;
    let res = run_crawl(&app, &site, &cookie).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(res.body.contains("available in 3 days."), "{}", res.body);
}

#[tokio::test]
async fn only_manual_crawls_count_against_the_allowance() {
    let app = TestApp::new().await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Free))
        .await;
    let site = app.site(&account, "example.com").await;
    crawl_at(&app, &site, "first", "done", "1 hour").await;
    crawl_at(&app, &site, "schedule", "done", "30 minutes").await;
    let res = run_crawl(&app, &site, &cookie).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
}

#[tokio::test]
async fn the_allowance_is_per_site() {
    let app = TestApp::new().await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Pro))
        .await;
    let a = app.site(&account, "a.com").await;
    let b = app.site(&account, "b.com").await;
    crawl_at(&app, &a, "manual", "done", "1 hour").await;
    assert_eq!(
        run_crawl(&app, &a, &cookie).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        run_crawl(&app, &b, &cookie).await.status,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn pro_allows_one_manual_crawl_a_day() {
    let app = TestApp::new().await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Pro))
        .await;
    let site = app.site(&account, "example.com").await;
    let old = crawl_at(&app, &site, "manual", "done", "23 hours").await;

    let res = run_crawl(&app, &site, &cookie).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert!(
        res.body.contains(
            "Your Pro plan includes 1 manual crawl a day. The next one is available in 1 hour."
        ),
        "{}",
        res.body
    );

    set_created(&app, old, "25 hours").await;
    let res = run_crawl(&app, &site, &cookie).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    assert_eq!(
        queued_crawls(&app, &site).await,
        vec![("manual".to_owned(), 2)]
    );
}

#[tokio::test]
async fn agency_manual_crawls_are_unlimited() {
    let app = TestApp::new().await;
    let (account, cookie) = app
        .login_with_plan("ana@example.com", Some(Plan::Agency))
        .await;
    let site = app.site(&account, "example.com").await;
    for _ in 0..3 {
        crawl_at(&app, &site, "manual", "done", "10 minutes").await;
    }
    for _ in 0..3 {
        let res = run_crawl(&app, &site, &cookie).await;
        assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
        finish_all(&app, &site).await;
    }
}

// ── Run crawl: one at a time, priority, responses ─────

#[tokio::test]
async fn a_second_run_while_one_is_queued_or_running_is_refused() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let site = app.site(&account, "example.com").await;

    let res = run_crawl(&app, &site, &cookie).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    let res = run_crawl(&app, &site, &cookie).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert!(
        res.body
            .contains("A crawl is already queued for this site."),
        "{}",
        res.body
    );

    sqlx::query("UPDATE crawls SET status = 'running', started_at = now() WHERE site_id = $1")
        .bind(site.id)
        .execute(app.pool())
        .await
        .unwrap();
    let res = run_crawl(&app, &site, &cookie).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert!(
        res.body
            .contains("A crawl is already running for this site."),
        "{}",
        res.body
    );
    assert_eq!(queued_crawls(&app, &site).await.len(), 0);
}

#[tokio::test]
async fn simultaneous_clicks_queue_one_crawl() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let site = app.site(&account, "example.com").await;
    let (a, b, c) = tokio::join!(
        run_crawl(&app, &site, &cookie),
        run_crawl(&app, &site, &cookie),
        run_crawl(&app, &site, &cookie),
    );
    let mut statuses = [a.status, b.status, c.status];
    statuses.sort();
    assert_eq!(
        statuses,
        [
            StatusCode::NO_CONTENT,
            StatusCode::CONFLICT,
            StatusCode::CONFLICT
        ],
        "{} | {} | {}",
        a.body,
        b.body,
        c.body
    );
    assert_eq!(queued_crawls(&app, &site).await.len(), 1);
}

#[tokio::test]
async fn manual_priority_lanes() {
    let app = TestApp::new().await;
    for (i, (plan, lane)) in [
        (Plan::Free, 4),
        (Plan::Pro, 2),
        (Plan::Agency, 2),
        (Plan::SelfHosted, 2),
    ]
    .into_iter()
    .enumerate()
    {
        let (account, cookie) = app
            .login_with_plan(&format!("user{i}@example.com"), Some(plan))
            .await;
        let site = app.site(&account, &format!("site{i}.com")).await;
        let res = run_crawl(&app, &site, &cookie).await;
        assert_eq!(res.status, StatusCode::NO_CONTENT, "{plan:?}: {}", res.body);
        assert_eq!(
            queued_crawls(&app, &site).await,
            vec![("manual".to_owned(), lane)],
            "{plan:?}"
        );
    }
}

#[tokio::test]
async fn run_crawl_toasts_and_announces_the_queued_crawl() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let site = app.site(&account, "example.com").await;
    app.finished_crawl(&site, vec![page("example.com", "/")], vec![])
        .await;

    let res = run_crawl(&app, &site, &cookie).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    let t = trigger_json(&res);
    assert_eq!(t["toast"]["kind"], "ok");
    assert_eq!(t["toast"]["message"], "Crawl #2 queued");
    assert_eq!(t["crawlQueued"], true);
}

#[tokio::test]
async fn run_crawl_without_htmx_redirects_to_history() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let site = app.site(&account, "example.com").await;
    let res = app
        .post(&format!("/s/{}/crawls", site.id), "", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(
        res.location(),
        Some(format!("/s/{}/crawls", site.id).as_str())
    );
    assert_eq!(queued_crawls(&app, &site).await.len(), 1);
}

// ── History ───────────────────────────────────────────

#[tokio::test]
async fn history_lists_crawls_newest_first() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let site = app.site(&account, "example.com").await;
    let pages = || vec![page("example.com", "/"), page("example.com", "/about")];
    app.finished_crawl(&site, pages(), vec![]).await;
    app.finished_crawl(&site, pages(), vec![]).await;
    let failed = crawl_at(&app, &site, "schedule", "failed", "0 seconds").await;
    sqlx::query(
        "UPDATE crawls SET failure_reason = 'Site unreachable: DNS lookup failed' WHERE id = $1",
    )
    .bind(failed)
    .execute(app.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, started_at, progress) \
         VALUES ($1, $2, 'manual', 2, 'running', now(), \
                 '{\"pages_done\": 312, \"queued\": 40, \"failures\": 0, \"depth\": 2, \"elapsed_ms\": 42000}')",
    )
    .bind(site.id)
    .bind(&site.domain)
    .execute(app.pool())
    .await
    .unwrap();

    let res = app
        .get(&format!("/s/{}/crawls", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let b = &res.body;
    assert!(b.contains("<html"), "a full page");
    assert!(b.contains("<h1>Crawls</h1>"));
    assert!(b.contains("4 crawls"));
    let pos = |n: u32| {
        let needle = format!(">#{n}</span>");
        b.find(&needle)
            .unwrap_or_else(|| panic!("missing {needle:?}"))
    };
    assert!(pos(4) < pos(3));
    assert!(pos(3) < pos(2));
    assert!(pos(2) < pos(1));
    // Running: a shimmering label and the pages done so far; the list polls while it runs.
    assert!(b.contains("shimmer"));
    assert!(b.contains("312"));
    assert!(b.contains("every 3s"));
    // Failed, with its reason; scheduled trigger label.
    assert!(b.contains("Site unreachable: DNS lookup failed"));
    assert!(b.contains("Scheduled"));
    // Done rows link to the audit and show health, pages and duration.
    assert!(b.contains(&format!("href=\"/s/{}/audit\"", site.id)));
    assert!(b.contains("1m 35s"));
    // The list refreshes itself when a crawl is queued or finishes.
    assert!(b.contains("crawlQueued from:body"));
    assert!(b.contains("crawlFinished from:body"));

    // The same URL from htmx returns only the list fragment.
    let res = app
        .get_hx(&format!("/s/{}/crawls", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(!res.body.contains("<html"));
    assert!(res.body.contains("id=\"crawl-list\""));
    assert!(res.body.contains("#4"));
}

#[tokio::test]
async fn history_without_crawls_shows_the_empty_state() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let site = app.site(&account, "example.com").await;
    let res = app
        .get(&format!("/s/{}/crawls", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("No crawls yet"), "{}", res.body);
    assert!(!res.body.contains("every 3s"));
}

#[tokio::test]
async fn another_accounts_site_is_not_found() {
    let app = TestApp::new().await;
    let (owner, _) = app.login("owner@example.com").await;
    let site = app.site(&owner, "example.com").await;
    let (_, cookie) = app.login("other@example.com").await;

    let base = format!("/s/{}", site.id);
    let res = app.get(&format!("{base}/crawls"), Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let res = app.get_hx(&format!("{base}/crawls"), Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let res = app
        .post_hx(&format!("{base}/crawls"), "", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let res = app
        .get_hx(&format!("{base}/status?was=running"), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    // Nothing was queued for someone else's site, and a bad ID is a 404 too.
    assert!(queued_crawls(&app, &site).await.is_empty());
    let res = app.get("/s/not-a-uuid/crawls", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

// ── Status poll ───────────────────────────────────────

#[tokio::test]
async fn status_poll_shows_the_active_crawl() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let site = app.site(&account, "example.com").await;
    run_crawl(&app, &site, &cookie).await;

    let res = app
        .get_hx(&format!("/s/{}/status?was=queued", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("id=\"crawler\""), "{}", res.body);
    assert!(res.body.contains("QUEUED #1"));
    assert!(res.body.contains("every 2s"), "keeps polling");
    assert!(res.header("hx-trigger").is_none());
}

#[tokio::test]
async fn status_poll_announces_a_finished_crawl() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let site = app.site(&account, "example.com").await;
    app.finished_crawl(&site, vec![page("example.com", "/")], vec![])
        .await;
    let health: i16 = sqlx::query_scalar("SELECT health_score FROM crawls WHERE site_id = $1")
        .bind(site.id)
        .fetch_one(app.pool())
        .await
        .unwrap();

    let res = app
        .get_hx(&format!("/s/{}/status?was=running", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("CRAWLER IDLE"), "{}", res.body);
    assert!(!res.body.contains("every 2s"), "stops polling");
    let t = trigger_json(&res);
    assert!(t.get("crawlFinished").is_some(), "{t}");
    assert_eq!(t["toast"]["kind"], "ok");
    assert_eq!(
        t["toast"]["message"],
        format!("Crawl #1 finished · health {health}/100")
    );

    // Polling an idle card (or a card that was already idle) announces nothing.
    let res = app
        .get_hx(&format!("/s/{}/status?was=idle", site.id), Some(&cookie))
        .await;
    assert!(res.header("hx-trigger").is_none());
    let res = app
        .get_hx(&format!("/s/{}/status", site.id), Some(&cookie))
        .await;
    assert!(res.header("hx-trigger").is_none());
}

#[tokio::test]
async fn status_poll_announces_a_failed_crawl() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let site = app.site(&account, "example.com").await;
    app.finished_crawl(&site, vec![page("example.com", "/")], vec![])
        .await;
    let failed = crawl_at(&app, &site, "manual", "failed", "0 seconds").await;
    sqlx::query(
        "UPDATE crawls SET failure_reason = 'robots.txt blocks the whole site' WHERE id = $1",
    )
    .bind(failed)
    .execute(app.pool())
    .await
    .unwrap();

    let res = app
        .get_hx(&format!("/s/{}/status?was=running", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let t = trigger_json(&res);
    assert!(t.get("crawlFinished").is_some(), "{t}");
    assert_eq!(t["toast"]["kind"], "error");
    assert_eq!(
        t["toast"]["message"],
        "Crawl #2 failed: robots.txt blocks the whole site"
    );
}

/// The add-site form is boosted: success is a normal redirect into the new site, and a bad
/// address or a reached limit comes back as the form, retargeted onto itself.
#[tokio::test]
async fn add_site_over_htmx_redirects_or_retargets_the_form() {
    let app = TestApp::new().await;
    let (_, cookie) = app
        .login_with_plan("free@example.com", Some(Plan::Free))
        .await;

    let res = app
        .post_hx("/sites", "url=not%20a%20url", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.header("hx-retarget"), Some("#add-site"));
    assert_eq!(res.header("hx-reswap"), Some("outerHTML"));
    assert!(res.body.contains("id=\"add-site\""));
    assert!(res.body.contains("error-text"));

    let res = app
        .post_hx("/sites", "url=example.com", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(res.location().unwrap().ends_with("/audit"));

    // Free allows one site: the second shows the limit inside the form.
    let res = app
        .post_hx("/sites", "url=second.example", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.header("hx-retarget"), Some("#add-site"));
    assert!(
        res.body.contains("Your plan includes 1 site"),
        "{}",
        res.body
    );
}
