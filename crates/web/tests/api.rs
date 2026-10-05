//! M8 T2: the REST API at `/api/v1`: Bearer-only authentication, every endpoint over fixture
//! crawls written through the real `finalize`, the account boundary, the daily quota and Run
//! crawl's rules.

mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use codoseo_core::change::{Change, ChangeKind};
use codoseo_core::check::{CheckId, Severity};
use codoseo_core::output::StopReason;
use codoseo_core::page::{Indexability, PageRecord};
use codoseo_core::plan::Plan;
use codoseo_mcp::cloud::types::{
    ChangesPage, CrawlQueued, IssueUrlsPage, PageInfo, SiteHealth, SiteInfo, Usage,
};
use codoseo_store::accounts::Account;
use codoseo_store::api_keys::{self, CreateKeyOutcome};
use codoseo_store::sites::Site;
use codoseo_web::agent::keys;
use serde_json::Value;
use support::{TestApp, TestResponse, cloud_config, page};
use url::Url;
use uuid::Uuid;

const DOMAIN: &str = "example.com";

async fn cloud() -> TestApp {
    TestApp::with_config(cloud_config()).await
}

/// A live key for the account; returns the plaintext and the key's id.
async fn make_key(app: &TestApp, account: &Account) -> (String, Uuid) {
    let key = keys::generate();
    let id = match api_keys::create(
        app.pool(),
        account.id,
        "test key",
        &key.hash,
        &key.prefix,
        api_keys::MAX_LIVE_KEYS,
    )
    .await
    .unwrap()
    {
        CreateKeyOutcome::Created(k) => k.id,
        CreateKeyOutcome::LimitReached => panic!("at the cap"),
    };
    (key.plaintext, id)
}

fn request(method: Method, path: &str, key: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(key) = key {
        b = b.header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    b.body(Body::empty()).unwrap()
}

async fn get(app: &TestApp, path: &str, key: &str) -> TestResponse {
    app.send(request(Method::GET, path, Some(key))).await
}

async fn post(app: &TestApp, path: &str, key: &str) -> TestResponse {
    app.send(request(Method::POST, path, Some(key))).await
}

fn json(res: &TestResponse) -> Value {
    serde_json::from_str(&res.body).unwrap_or_else(|e| panic!("json body ({e}): {}", res.body))
}

fn parsed<T: serde::de::DeserializeOwned>(res: &TestResponse) -> T {
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    serde_json::from_str(&res.body).unwrap_or_else(|e| panic!("wire type ({e}): {}", res.body))
}

fn error_code(res: &TestResponse) -> String {
    json(res)["error"]["code"].as_str().unwrap().to_owned()
}

/// Healthy home and about pages, a 404, and three pages with no title.
fn fixture_pages() -> Vec<PageRecord> {
    let mut gone = PageRecord {
        status: 404,
        indexability: Indexability::ClientError,
        ..page(DOMAIN, "/gone")
    };
    gone.key_hash = gone.compute_key_hash();
    let mut pages = vec![page(DOMAIN, "/"), page(DOMAIN, "/about"), gone];
    for n in 1..=3 {
        let mut untitled = page(DOMAIN, &format!("/untitled-{n}"));
        untitled.fields.title = None;
        untitled.fields.title_count = 0;
        untitled.key_hash = untitled.compute_key_hash();
        pages.push(untitled);
    }
    pages
}

fn change(kind: ChangeKind, severity: Severity, path: Option<&str>) -> Change {
    Change {
        kind,
        severity,
        url: path.map(|p| Url::parse(&format!("https://{DOMAIN}{p}")).unwrap()),
        before: "before".to_owned(),
        after: "after".to_owned(),
    }
}

fn fixture_changes() -> Vec<Change> {
    vec![
        change(ChangeKind::RobotsTxtChanged, Severity::Critical, None),
        change(ChangeKind::StatusChanged, Severity::Warning, Some("/gone")),
        change(ChangeKind::NewUrl, Severity::Notice, Some("/about")),
        change(ChangeKind::TitleChanged, Severity::Notice, Some("/")),
    ]
}

struct Fixture {
    app: TestApp,
    account: Account,
    site: Site,
    key: String,
}

/// A cloud app with an account on `plan`, a key, and a site with one finished crawl.
async fn setup(plan: Plan) -> Fixture {
    let app = cloud().await;
    let (account, _) = app.login_with_plan("owner@example.com", Some(plan)).await;
    let (key, _) = make_key(&app, &account).await;
    let site = app.site(&account, DOMAIN).await;
    app.finished_crawl(&site, fixture_pages(), fixture_changes())
        .await;
    Fixture {
        app,
        account,
        site,
        key,
    }
}

// ---- authentication ----

#[tokio::test]
async fn a_missing_malformed_or_revoked_key_is_a_401_with_www_authenticate() {
    let app = cloud().await;
    let (account, _) = app.login("owner@example.com").await;
    let (key, key_id) = make_key(&app, &account).await;
    assert!(
        api_keys::revoke(app.pool(), account.id, key_id)
            .await
            .unwrap()
    );
    let unknown = keys::generate().plaintext;

    let mut attempts = vec![
        request(Method::GET, "/api/v1/sites", None),
        request(Method::GET, "/api/v1/sites", Some("garbage")),
        request(Method::GET, "/api/v1/sites", Some("cdo_short")),
        request(Method::GET, "/api/v1/sites", Some(&key)),
        request(Method::GET, "/api/v1/sites", Some(&unknown)),
        request(Method::POST, "/api/v1/sites/x/crawls", None),
        request(Method::GET, "/api/v1/usage", None),
    ];
    // Another scheme, and a header that isn't text.
    let mut basic = request(Method::GET, "/api/v1/sites", None);
    basic
        .headers_mut()
        .insert(header::AUTHORIZATION, "Basic dXNlcjpwYXNz".parse().unwrap());
    attempts.push(basic);
    let mut odd = request(Method::GET, "/api/v1/sites", None);
    odd.headers_mut()
        .insert(header::AUTHORIZATION, "Bearer \u{e9}".parse().unwrap());
    attempts.push(odd);

    for req in attempts {
        let label = format!(
            "{} {:?}",
            req.uri(),
            req.headers().get(header::AUTHORIZATION)
        );
        let res = app.send(req).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{label}");
        assert_eq!(res.header("www-authenticate"), Some("Bearer"), "{label}");
        assert_eq!(error_code(&res), "unauthorized", "{label}");
        assert_eq!(res.header("content-type"), Some("application/json"));
        assert!(!res.body.contains("<html"), "{label}");
    }
}

#[tokio::test]
async fn a_session_cookie_alone_never_authenticates() {
    let app = cloud().await;
    let (account, cookie) = app.login("owner@example.com").await;
    let site = app.site(&account, DOMAIN).await;

    let res = app.get("/api/v1/sites", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&res), "unauthorized");
    let res = app
        .get(&format!("/api/v1/sites/{}", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    // A same-origin POST with the cookie queues nothing.
    let res = app
        .post(
            &format!("/api/v1/sites/{}/crawls", site.id),
            "",
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    // Nor does a cross-site one: it is exempt from the Origin check and still has no key.
    let mut req = request(
        Method::POST,
        &format!("/api/v1/sites/{}/crawls", site.id),
        None,
    );
    req.headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    req.headers_mut()
        .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
    let res = app.send(req).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM crawls WHERE site_id = $1")
        .bind(site.id)
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(queued, 0);
}

#[tokio::test]
async fn a_post_from_a_foreign_origin_with_a_key_works() {
    let f = setup(Plan::Agency).await;
    let mut req = request(
        Method::POST,
        &format!("/api/v1/sites/{}/crawls", f.site.id),
        Some(&f.key),
    );
    req.headers_mut()
        .insert(header::ORIGIN, "https://agent.example".parse().unwrap());
    let res = f.app.send(req).await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.body);
}

#[tokio::test]
async fn only_the_api_prefix_is_exempt_from_the_origin_check() {
    let app = cloud().await;
    for path in ["/api/v1", "/api/v1x/sites", "/api/v2/sites", "/api"] {
        let mut req = request(Method::POST, path, None);
        req.headers_mut()
            .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        let res = app.send(req).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{path}");
    }
}

#[tokio::test]
async fn an_unknown_endpoint_is_a_json_404() {
    let f = setup(Plan::Free).await;
    let res = get(&f.app, "/api/v1/nope", &f.key).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&res), "not_found");
}

// ---- the endpoints ----

#[tokio::test]
async fn list_sites_lists_only_this_accounts_sites_with_their_latest_score() {
    let f = setup(Plan::Pro).await;
    let (other, _) = f.app.login("other@example.com").await;
    f.app.site(&other, "other.example").await;
    let fresh = f.app.site(&f.account, "fresh.example").await;

    let list: Vec<SiteInfo> = parsed(&get(&f.app, "/api/v1/sites", &f.key).await);
    assert_eq!(list.len(), 2);
    let main = list.iter().find(|s| s.id == f.site.id).unwrap();
    assert_eq!(main.domain, DOMAIN);
    assert_eq!(main.start_url, "https://example.com/");
    assert!(main.monitoring_active);
    assert_eq!(main.schedule.as_deref(), Some("weekly"));
    assert!(main.health_score.is_some());
    assert!(main.last_crawled_at.is_some());
    let fresh_info = list.iter().find(|s| s.id == fresh.id).unwrap();
    assert_eq!(fresh_info.health_score, None);
    assert_eq!(fresh_info.last_crawled_at, None);
    assert!(list.iter().all(|s| s.domain != "other.example"));
}

#[tokio::test]
async fn site_health_has_the_score_failing_checks_with_three_examples_and_the_audit_link() {
    let f = setup(Plan::Pro).await;
    let health: SiteHealth =
        parsed(&get(&f.app, &format!("/api/v1/sites/{}", f.site.id), &f.key).await);
    assert_eq!(health.id, f.site.id);
    assert_eq!(health.domain, DOMAIN);
    assert_eq!(health.schedule.as_deref(), Some("weekly"));
    assert_eq!(
        health.audit_url,
        format!("https://codoseo.com/s/{}/audit", f.site.id)
    );
    assert!(health.active_crawl.is_none());
    let latest = health.latest_crawl.expect("a finished crawl");
    assert_eq!(latest.number, 1);
    assert_eq!(latest.pages_crawled, 6);
    assert_eq!(latest.stop_reason, "completed");
    assert!(latest.health_score.is_some());
    assert!(latest.checks_total.unwrap() > 0);
    assert!(latest.checks_passed.unwrap() < latest.checks_total.unwrap());
    assert!(latest.finished_at.is_some());

    let titles = latest
        .failing_checks
        .iter()
        .find(|c| c.check == CheckId::TitleMissing)
        .expect("title_missing fails");
    assert_eq!(titles.count, 3);
    assert_eq!(titles.severity, Severity::Warning);
    assert_eq!(titles.example_urls.len(), 3);
    assert!(
        titles
            .example_urls
            .iter()
            .all(|u| u.path().starts_with("/untitled-"))
    );
    let http4xx = latest
        .failing_checks
        .iter()
        .find(|c| c.check == CheckId::Http4xx)
        .expect("http_4xx fails");
    assert_eq!(http4xx.count, 1);
    assert_eq!(http4xx.example_urls[0].path(), "/gone");
    // Most severe first.
    let severities: Vec<_> = latest.failing_checks.iter().map(|c| c.severity).collect();
    let mut sorted = severities.clone();
    sorted.sort();
    assert_eq!(severities, sorted);
    assert!(latest.failing_checks.len() <= 15);
}

#[tokio::test]
async fn the_next_crawl_shows_only_while_monitoring_is_on() {
    let f = setup(Plan::Pro).await;
    sqlx::query("UPDATE sites SET next_crawl_at = now() + interval '2 days' WHERE id = $1")
        .bind(f.site.id)
        .execute(f.app.pool())
        .await
        .unwrap();
    let path = format!("/api/v1/sites/{}", f.site.id);
    let health: SiteHealth = parsed(&get(&f.app, &path, &f.key).await);
    let next = health.next_crawl_at.expect("a slot is set");
    assert!(next > time::OffsetDateTime::now_utc() + time::Duration::days(1));

    sqlx::query("UPDATE sites SET monitoring_active = false WHERE id = $1")
        .bind(f.site.id)
        .execute(f.app.pool())
        .await
        .unwrap();
    let health: SiteHealth = parsed(&get(&f.app, &path, &f.key).await);
    assert!(!health.monitoring_active);
    assert_eq!(health.next_crawl_at, None);
}

#[tokio::test]
async fn failing_checks_list_three_examples_however_many_pages_fail() {
    let app = cloud().await;
    let (account, _) = app.login("owner@example.com").await;
    let (key, _) = make_key(&app, &account).await;
    let site = app.site(&account, DOMAIN).await;
    let mut pages = vec![page(DOMAIN, "/")];
    for n in 1..=5 {
        let mut untitled = page(DOMAIN, &format!("/untitled-{n}"));
        untitled.fields.title = None;
        untitled.fields.title_count = 0;
        untitled.key_hash = untitled.compute_key_hash();
        pages.push(untitled);
    }
    app.finished_crawl(&site, pages, vec![]).await;
    let health: SiteHealth = parsed(&get(&app, &format!("/api/v1/sites/{}", site.id), &key).await);
    let titles = health
        .latest_crawl
        .unwrap()
        .failing_checks
        .into_iter()
        .find(|c| c.check == CheckId::TitleMissing)
        .unwrap();
    assert_eq!(titles.count, 5);
    assert_eq!(titles.example_urls.len(), 3);
}

#[tokio::test]
async fn site_health_shows_the_crawl_in_flight_and_a_site_without_a_crawl() {
    let f = setup(Plan::Agency).await;
    let fresh = f.app.site(&f.account, "fresh.example").await;
    let res = get(&f.app, &format!("/api/v1/sites/{}", fresh.id), &f.key).await;
    let health: SiteHealth = parsed(&res);
    assert!(health.latest_crawl.is_none());
    assert!(health.active_crawl.is_none());

    let queued = post(
        &f.app,
        &format!("/api/v1/sites/{}/crawls", f.site.id),
        &f.key,
    )
    .await;
    assert_eq!(queued.status, StatusCode::ACCEPTED);
    let health: SiteHealth =
        parsed(&get(&f.app, &format!("/api/v1/sites/{}", f.site.id), &f.key).await);
    let active = health.active_crawl.expect("a crawl is queued");
    assert_eq!(active.status, "queued");
    assert_eq!(active.number, 2);
    assert!(
        health.latest_crawl.is_some(),
        "the finished one still shows"
    );
}

#[tokio::test]
async fn issue_urls_pages_through_the_failing_pages() {
    let f = setup(Plan::Pro).await;
    let base = format!("/api/v1/sites/{}/issues/title_missing", f.site.id);

    let all: IssueUrlsPage = parsed(&get(&f.app, &base, &f.key).await);
    assert_eq!(all.check, CheckId::TitleMissing);
    assert_eq!(all.title, "Title is missing");
    assert_eq!(all.crawl_number, Some(1));
    assert_eq!(all.total, 3);
    assert_eq!((all.limit, all.offset), (50, 0));
    assert_eq!(all.urls.len(), 3);
    assert_eq!(all.next_offset, None);
    assert!(all.urls.iter().all(|u| u.status == 200));
    assert!(all.urls.iter().all(|u| u.title.is_none()));

    let first: IssueUrlsPage = parsed(&get(&f.app, &format!("{base}?limit=2"), &f.key).await);
    assert_eq!(first.urls.len(), 2);
    assert_eq!(first.next_offset, Some(2));
    let next: IssueUrlsPage = parsed(
        &get(
            &f.app,
            &format!("{base}?limit=2&offset={}", first.next_offset.unwrap()),
            &f.key,
        )
        .await,
    );
    assert_eq!(next.urls.len(), 1);
    assert_eq!(next.next_offset, None);
    let mut seen: Vec<_> = first
        .urls
        .iter()
        .chain(&next.urls)
        .map(|u| u.url.clone())
        .collect();
    seen.dedup();
    assert_eq!(seen.len(), 3, "pages don't overlap");

    // The limit is capped, and a check nothing fails is an empty page.
    let capped: IssueUrlsPage = parsed(&get(&f.app, &format!("{base}?limit=100000"), &f.key).await);
    assert_eq!(capped.limit, 200);
    let clean: IssueUrlsPage = parsed(
        &get(
            &f.app,
            &format!("/api/v1/sites/{}/issues/not_https", f.site.id),
            &f.key,
        )
        .await,
    );
    assert_eq!((clean.total, clean.urls.len()), (0, 0));
}

#[tokio::test]
async fn an_unknown_check_or_a_bad_query_is_a_400() {
    let f = setup(Plan::Pro).await;
    let res = get(
        &f.app,
        &format!("/api/v1/sites/{}/issues/no_such_check", f.site.id),
        &f.key,
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&res), "bad_request");
    assert!(res.body.contains("no_such_check"));

    for path in [
        format!("/api/v1/sites/{}/issues/title_missing?limit=abc", f.site.id),
        format!("/api/v1/sites/{}/issues/title_missing?offset=-1", f.site.id),
        format!("/api/v1/sites/{}/changes?severity=severe", f.site.id),
        format!("/api/v1/sites/{}/page", f.site.id),
        format!("/api/v1/sites/{}/page?url=", f.site.id),
        format!("/api/v1/sites/{}/page?url=mailto:a@b.c", f.site.id),
    ] {
        let res = get(&f.app, &path, &f.key).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{path}: {}", res.body);
        assert_eq!(error_code(&res), "bad_request", "{path}");
    }
}

#[tokio::test]
async fn a_site_without_a_crawl_gives_empty_pages() {
    let f = setup(Plan::Pro).await;
    let fresh = f.app.site(&f.account, "fresh.example").await;
    let issues: IssueUrlsPage = parsed(
        &get(
            &f.app,
            &format!("/api/v1/sites/{}/issues/title_missing", fresh.id),
            &f.key,
        )
        .await,
    );
    assert_eq!(issues.crawl_number, None);
    assert!(issues.urls.is_empty());
    let changes: ChangesPage = parsed(
        &get(
            &f.app,
            &format!("/api/v1/sites/{}/changes", fresh.id),
            &f.key,
        )
        .await,
    );
    assert_eq!(changes.crawl_number, None);
    assert!(changes.changes.is_empty());
    let res = get(
        &f.app,
        &format!("/api/v1/sites/{}/page?url=https://fresh.example/", fresh.id),
        &f.key,
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn page_finds_a_url_however_it_is_written() {
    let f = setup(Plan::Pro).await;
    let base = format!("/api/v1/sites/{}/page", f.site.id);
    let by_absolute: PageInfo = parsed(
        &get(
            &f.app,
            &format!("{base}?url=https%3A%2F%2Fexample.com%2Funtitled-2"),
            &f.key,
        )
        .await,
    );
    assert_eq!(by_absolute.url, "https://example.com/untitled-2");
    assert_eq!(by_absolute.status, 200);
    assert_eq!(by_absolute.crawl_number, 1);
    assert_eq!(by_absolute.indexability, Indexability::Indexable);
    assert_eq!(by_absolute.title, None);
    assert_eq!(by_absolute.h1, vec!["Heading for /untitled-2".to_owned()]);
    assert_eq!(by_absolute.word_count, Some(640));
    assert!(by_absolute.inlinks >= 1);
    let failing: Vec<_> = by_absolute.issues.iter().map(|i| i.check).collect();
    assert!(failing.contains(&CheckId::TitleMissing), "{failing:?}");
    assert!(by_absolute.issues.iter().all(|i| !i.title.is_empty()));

    // A path, and a fragment or a trailing-dot host, normalise to the same page.
    for written in [
        "%2Funtitled-2",
        "https%3A%2F%2Fexample.com%2Funtitled-2%23top",
    ] {
        let again: PageInfo = parsed(&get(&f.app, &format!("{base}?url={written}"), &f.key).await);
        assert_eq!(again.url, by_absolute.url, "{written}");
    }

    let gone: PageInfo = parsed(&get(&f.app, &format!("{base}?url=%2Fgone"), &f.key).await);
    assert_eq!(gone.status, 404);
    let failing: Vec<_> = gone.issues.iter().map(|i| i.check).collect();
    assert!(failing.contains(&CheckId::Http4xx));

    let missing = get(&f.app, &format!("{base}?url=%2Fnever-crawled"), &f.key).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&missing), "not_found");
}

#[tokio::test]
async fn changes_are_most_severe_first_and_can_be_filtered() {
    let f = setup(Plan::Pro).await;
    let base = format!("/api/v1/sites/{}/changes", f.site.id);
    let all: ChangesPage = parsed(&get(&f.app, &base, &f.key).await);
    assert_eq!(all.crawl_number, Some(1));
    assert!(all.crawl_finished_at.is_some());
    assert_eq!(all.total, 4);
    assert_eq!(all.changes.len(), 4);
    assert_eq!(all.changes[0].severity, Severity::Critical);
    assert_eq!(all.changes[0].kind, ChangeKind::RobotsTxtChanged);
    assert_eq!(all.changes[0].url, None);
    assert_eq!(all.changes[1].severity, Severity::Warning);
    assert_eq!(
        all.changes[1].url.as_deref(),
        Some("https://example.com/gone")
    );
    assert_eq!(all.changes[1].before, "before");

    let notices: ChangesPage =
        parsed(&get(&f.app, &format!("{base}?severity=notice"), &f.key).await);
    assert_eq!(notices.total, 2);
    assert!(
        notices
            .changes
            .iter()
            .all(|c| c.severity == Severity::Notice)
    );

    let one: ChangesPage = parsed(&get(&f.app, &format!("{base}?limit=1"), &f.key).await);
    assert_eq!(one.changes.len(), 1);
    assert_eq!(one.total, 4, "the total counts past the limit");
}

// ---- Review focus 1: another account's data ----

#[tokio::test]
async fn another_accounts_site_is_indistinguishable_from_an_unknown_one() {
    let f = setup(Plan::Pro).await;
    let (other, _) = f.app.login("other@example.com").await;
    let theirs = f.app.site(&other, "other.example").await;
    f.app
        .finished_crawl(&theirs, vec![page("other.example", "/")], vec![])
        .await;
    let unknown = Uuid::new_v4();

    let paths = |site: &str| {
        vec![
            (Method::GET, format!("/api/v1/sites/{site}")),
            (
                Method::GET,
                format!("/api/v1/sites/{site}/issues/title_missing"),
            ),
            (
                Method::GET,
                format!("/api/v1/sites/{site}/page?url=https%3A%2F%2Fother.example%2F"),
            ),
            (Method::GET, format!("/api/v1/sites/{site}/changes")),
            (Method::POST, format!("/api/v1/sites/{site}/crawls")),
        ]
    };
    let foreign = paths(&theirs.id.to_string());
    let missing = paths(&unknown.to_string());
    let garbled = paths("not-a-uuid");
    for ((a, b), c) in foreign.into_iter().zip(missing).zip(garbled) {
        let theirs = f.app.send(request(a.0.clone(), &a.1, Some(&f.key))).await;
        let nothing = f.app.send(request(b.0.clone(), &b.1, Some(&f.key))).await;
        let nonsense = f.app.send(request(c.0.clone(), &c.1, Some(&f.key))).await;
        assert_eq!(theirs.status, StatusCode::NOT_FOUND, "{}", a.1);
        assert_eq!(theirs.status, nothing.status, "{}", a.1);
        assert_eq!(theirs.body, nothing.body, "{}", a.1);
        assert_eq!(nothing.body, nonsense.body, "{}", c.1);
        assert_eq!(error_code(&theirs), "not_found");
    }
    // Nothing was queued for the other account's site.
    let queued: i64 =
        sqlx::query_scalar("SELECT count(*) FROM crawls WHERE site_id = $1 AND status = 'queued'")
            .bind(theirs.id)
            .fetch_one(f.app.pool())
            .await
            .unwrap();
    assert_eq!(queued, 0);
}

// ---- quota ----

async fn set_calls_today(app: &TestApp, account: &Account, calls: i32) {
    sqlx::query(
        "INSERT INTO api_usage (account_id, day, calls) \
         VALUES ($1, (now() AT TIME ZONE 'utc')::date, $2) \
         ON CONFLICT (account_id, day) DO UPDATE SET calls = $2",
    )
    .bind(account.id)
    .bind(calls)
    .execute(app.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn every_keyed_response_carries_the_rate_limit_headers() {
    let f = setup(Plan::Free).await;
    let res = get(&f.app, "/api/v1/sites", &f.key).await;
    assert_eq!(res.header("x-ratelimit-limit"), Some("100"));
    assert_eq!(res.header("x-ratelimit-remaining"), Some("99"));
    // Errors are charged and carry them too.
    let res = get(&f.app, &format!("/api/v1/sites/{}", Uuid::new_v4()), &f.key).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.header("x-ratelimit-limit"), Some("100"));
    assert_eq!(res.header("x-ratelimit-remaining"), Some("98"));
    assert_eq!(res.header("cache-control"), Some("no-store"));
}

#[tokio::test]
async fn the_call_after_the_limit_is_a_429_with_retry_after_and_costs_nothing() {
    let f = setup(Plan::Free).await;
    set_calls_today(&f.app, &f.account, 99).await;
    let last = get(&f.app, "/api/v1/sites", &f.key).await;
    assert_eq!(last.status, StatusCode::OK);
    assert_eq!(last.header("x-ratelimit-remaining"), Some("0"));

    for _ in 0..3 {
        let res = get(&f.app, "/api/v1/sites", &f.key).await;
        assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(error_code(&res), "quota_exceeded");
        assert_eq!(res.header("x-ratelimit-limit"), Some("100"));
        assert_eq!(res.header("x-ratelimit-remaining"), Some("0"));
        let retry: u64 = res.header("retry-after").unwrap().parse().unwrap();
        assert!((1..=86_400).contains(&retry), "{retry}");
        assert_eq!(res.header("www-authenticate"), None);
    }
    // A refused POST queues nothing.
    let res = post(
        &f.app,
        &format!("/api/v1/sites/{}/crawls", f.site.id),
        &f.key,
    )
    .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    let queued: i64 =
        sqlx::query_scalar("SELECT count(*) FROM crawls WHERE site_id = $1 AND status = 'queued'")
            .bind(f.site.id)
            .fetch_one(f.app.pool())
            .await
            .unwrap();
    assert_eq!(queued, 0);

    // Usage is free: it answers over quota and the refusals didn't count.
    let usage: Usage = parsed(&get(&f.app, "/api/v1/usage", &f.key).await);
    assert_eq!(usage.calls_today, 100);
    assert_eq!(usage.limit, Some(100));
    assert_eq!(usage.remaining, Some(0));
    let again: Usage = parsed(&get(&f.app, "/api/v1/usage", &f.key).await);
    assert_eq!(again.calls_today, 100);
}

#[tokio::test]
async fn usage_reports_the_count_the_limit_and_the_next_reset() {
    let f = setup(Plan::Pro).await;
    for _ in 0..3 {
        get(&f.app, "/api/v1/sites", &f.key).await;
    }
    let res = get(&f.app, "/api/v1/usage", &f.key).await;
    let usage: Usage = parsed(&res);
    assert_eq!(usage.calls_today, 3);
    assert_eq!(usage.limit, Some(2_000));
    assert_eq!(usage.remaining, Some(1_997));
    let now = time::OffsetDateTime::now_utc();
    assert!(usage.resets_at > now);
    assert!(usage.resets_at - now <= time::Duration::days(1));
    assert_eq!(
        (
            usage.resets_at.hour(),
            usage.resets_at.minute(),
            usage.resets_at.second()
        ),
        (0, 0, 0)
    );
    // Not counted: still 3 after the call above.
    let again: Usage = parsed(&get(&f.app, "/api/v1/usage", &f.key).await);
    assert_eq!(again.calls_today, 3);
}

#[tokio::test]
async fn concurrent_calls_with_m_left_succeed_exactly_m_times() {
    let f = setup(Plan::Free).await;
    set_calls_today(&f.app, &f.account, 95).await;
    let calls = (0..20).map(|_| get(&f.app, "/api/v1/sites", &f.key));
    let results = futures_util::future::join_all(calls).await;
    let ok = results
        .iter()
        .filter(|r| r.status == StatusCode::OK)
        .count();
    let refused = results
        .iter()
        .filter(|r| r.status == StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert_eq!((ok, refused), (5, 15));
}

#[tokio::test]
async fn self_hosted_has_no_limit_but_still_counts() {
    let app = TestApp::new().await;
    let (account, _) = app.login("owner@example.com").await;
    let (key, _) = make_key(&app, &account).await;
    set_calls_today(&app, &account, 1_000_000).await;
    let res = get(&app, "/api/v1/sites", &key).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.header("x-ratelimit-limit"), None);
    assert_eq!(res.header("x-ratelimit-remaining"), None);
    let usage: Usage = parsed(&get(&app, "/api/v1/usage", &key).await);
    assert_eq!(usage.calls_today, 1_000_001);
    assert_eq!(usage.limit, None);
    assert_eq!(usage.remaining, None);
}

// ---- Run crawl ----

#[tokio::test]
async fn run_crawl_queues_one_then_conflicts_while_it_is_queued() {
    let f = setup(Plan::Agency).await;
    let path = format!("/api/v1/sites/{}/crawls", f.site.id);
    let res = post(&f.app, &path, &f.key).await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.body);
    let queued: CrawlQueued = serde_json::from_str(&res.body).unwrap();
    assert_eq!(queued.site_id, f.site.id);
    assert_eq!(queued.number, 2);
    assert_eq!(queued.status, "queued");
    // Same lane as the button: priority 2 on a paid plan, manual trigger. (Agency, because the
    // fixture crawl is itself a manual one and Pro's daily allowance is a single crawl.)
    let (priority, trigger): (i16, String) =
        sqlx::query_as("SELECT priority, trigger::text FROM crawls WHERE id = $1")
            .bind(queued.crawl_id)
            .fetch_one(f.app.pool())
            .await
            .unwrap();
    assert_eq!((priority, trigger.as_str()), (2, "manual"));

    let res = post(&f.app, &path, &f.key).await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(error_code(&res), "crawl_in_progress");
    assert!(res.body.contains("already queued"));
}

#[tokio::test]
async fn a_free_accounts_second_manual_crawl_in_a_week_is_refused_as_a_plan_limit() {
    let app = cloud().await;
    let (account, _) = app
        .login_with_plan("free@example.com", Some(Plan::Free))
        .await;
    let (key, _) = make_key(&app, &account).await;
    let site = app.site(&account, DOMAIN).await;
    let path = format!("/api/v1/sites/{}/crawls", site.id);

    let res = post(&app, &path, &key).await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.body);
    let queued: CrawlQueued = serde_json::from_str(&res.body).unwrap();
    let priority: i16 = sqlx::query_scalar("SELECT priority FROM crawls WHERE id = $1")
        .bind(queued.crawl_id)
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(priority, 4, "Free's lane");

    // Once it has finished, the week's allowance is still spent.
    app.finalize_crawl(
        queued.crawl_id,
        fixture_pages(),
        vec![],
        StopReason::Completed,
    )
    .await;
    let res = post(&app, &path, &key).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert_eq!(error_code(&res), "plan_limit");
    assert!(
        res.body
            .contains("Free plan includes 1 manual crawl a week")
    );
}

#[tokio::test]
async fn self_hosted_manual_crawls_are_unlimited() {
    let app = TestApp::new().await;
    let (account, _) = app.login("owner@example.com").await;
    let (key, _) = make_key(&app, &account).await;
    let site = app.site(&account, DOMAIN).await;
    let path = format!("/api/v1/sites/{}/crawls", site.id);
    for _ in 0..3 {
        let res = post(&app, &path, &key).await;
        assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.body);
        let queued: CrawlQueued = serde_json::from_str(&res.body).unwrap();
        app.finalize_crawl(
            queued.crawl_id,
            fixture_pages(),
            vec![],
            StopReason::Completed,
        )
        .await;
    }
}
