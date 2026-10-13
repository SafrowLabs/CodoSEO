//! The deploy webhook: `POST /api/v1/sites/{site}/deploy` with a key, and the per-site secret URL
//! `POST /api/v1/hooks/deploy/{secret}` for platforms that can't send headers. It queues a crawl
//! under the plan's manual crawl rules, answers repeated calls with the crawl already covering
//! them, and the secret URL reaches one site only.

mod support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use codoseo_core::plan::Plan;
use codoseo_store::accounts::Account;
use codoseo_store::api_keys::{self, CreateKeyOutcome};
use codoseo_store::sites::Site;
use codoseo_web::agent::keys;
use serde_json::Value;
use support::{TestApp, TestResponse, cloud_config};

async fn make_key(app: &TestApp, account: &Account) -> String {
    let key = keys::generate();
    match api_keys::create(
        app.pool(),
        account.id,
        "deploy",
        &key.hash,
        &key.prefix,
        api_keys::MAX_LIVE_KEYS,
    )
    .await
    .unwrap()
    {
        CreateKeyOutcome::Created(_) => key.plaintext,
        CreateKeyOutcome::LimitReached => panic!("at the cap"),
    }
}

fn request(method: Method, path: &str, key: Option<&str>, body: &str) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(key) = key {
        b = b.header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    if !body.is_empty() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    b.body(Body::from(body.to_owned())).unwrap()
}

fn json(res: &TestResponse) -> Value {
    serde_json::from_str(&res.body).unwrap_or_else(|e| panic!("json body ({e}): {}", res.body))
}

struct Fixture {
    app: TestApp,
    account: Account,
    site: Site,
    key: String,
}

async fn setup(plan: Plan) -> Fixture {
    let app = TestApp::with_config(cloud_config()).await;
    let (account, _) = app.login_with_plan("owner@example.com", Some(plan)).await;
    let key = make_key(&app, &account).await;
    let site = app.site(&account, "example.com").await;
    Fixture {
        app,
        account,
        site,
        key,
    }
}

impl Fixture {
    async fn deploy(&self) -> TestResponse {
        let path = format!("/api/v1/sites/{}/deploy", self.site.id);
        self.app
            .send(request(Method::POST, &path, Some(&self.key), ""))
            .await
    }

    async fn hook(&self, method: Method) -> TestResponse {
        let path = format!("/api/v1/sites/{}/deploy-hook", self.site.id);
        self.app
            .send(request(method, &path, Some(&self.key), ""))
            .await
    }

    async fn crawls(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM crawls WHERE site_id = $1")
            .bind(self.site.id)
            .fetch_one(self.app.pool())
            .await
            .unwrap()
    }

    /// Every crawl of the site finishes, `minutes_ago` minutes after it was queued that long ago.
    async fn finish_all(&self, minutes_ago: i32) {
        sqlx::query(
            "UPDATE crawls SET status = 'done', \
               created_at = now() - make_interval(mins => $2), \
               queued_at = now() - make_interval(mins => $2), \
               started_at = now() - make_interval(mins => $2), finished_at = now() \
             WHERE site_id = $1",
        )
        .bind(self.site.id)
        .bind(minutes_ago)
        .execute(self.app.pool())
        .await
        .unwrap();
    }

    async fn calls_today(&self) -> i64 {
        api_keys::usage_today(self.app.pool(), self.account.id)
            .await
            .unwrap()
    }
}

/// The path part of a secret URL.
fn path_of(url: &str) -> String {
    let parsed = url::Url::parse(url).expect("a URL");
    parsed.path().to_owned()
}

#[tokio::test]
async fn a_deploy_queues_one_crawl_and_repeats_get_that_crawl_back() {
    let f = setup(Plan::Pro).await;
    let res = f.deploy().await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.body);
    let first = json(&res);
    assert_eq!(first["status"], "queued");
    assert_eq!(first["deduplicated"], false);
    assert_eq!(first["site_id"], f.site.id.to_string());
    assert_eq!(first["number"], 1);

    // A retry while it is queued: the same crawl, nothing new.
    let again = json(&f.deploy().await);
    assert_eq!(again["crawl_id"], first["crawl_id"]);
    assert_eq!(again["deduplicated"], true);
    assert_eq!(again["status"], "queued");
    assert_eq!(f.crawls().await, 1);

    // Finished a minute ago: still the same crawl, within the window.
    f.finish_all(1).await;
    let done = json(&f.deploy().await);
    assert_eq!(done["crawl_id"], first["crawl_id"]);
    assert_eq!(done["status"], "done");
    assert_eq!(done["deduplicated"], true);

    // Past the window, the next deploy crawls again.
    f.finish_all(10).await;
    let next = json(&f.deploy().await);
    assert_eq!(next["deduplicated"], false);
    assert_ne!(next["crawl_id"], first["crawl_id"]);
    assert_eq!(next["number"], 2);
    assert_eq!(f.crawls().await, 2);
}

#[tokio::test]
async fn a_deploy_counts_as_a_manual_crawl() {
    // Free: one manual crawl a week.
    let f = setup(Plan::Free).await;
    assert_eq!(f.deploy().await.status, StatusCode::ACCEPTED);
    f.finish_all(10).await;
    let res = f.deploy().await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert_eq!(json(&res)["error"]["code"], "plan_limit");
    assert_eq!(f.crawls().await, 1);
}

#[tokio::test]
async fn deploy_needs_a_key_for_one_of_your_sites() {
    let f = setup(Plan::Pro).await;
    let path = format!("/api/v1/sites/{}/deploy", f.site.id);
    let res = f.app.send(request(Method::POST, &path, None, "")).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);

    let (other, _) = f
        .app
        .login_with_plan("other@example.com", Some(Plan::Pro))
        .await;
    let other_key = make_key(&f.app, &other).await;
    let res = f
        .app
        .send(request(Method::POST, &path, Some(&other_key), ""))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let res = f
        .app
        .send(request(
            Method::POST,
            &format!("/api/v1/sites/{}/deploy-hook", f.site.id),
            Some(&other_key),
            "",
        ))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(f.crawls().await, 0);
}

#[tokio::test]
async fn the_secret_url_queues_a_crawl_of_its_site_until_it_is_replaced_or_removed() {
    let f = setup(Plan::Pro).await;
    let status = json(&f.hook(Method::GET).await);
    assert_eq!(status["active"], false);

    let res = f.hook(Method::POST).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    let url = json(&res)["url"].as_str().unwrap().to_owned();
    let path = path_of(&url);
    assert!(path.starts_with("/api/v1/hooks/deploy/cdh_"), "{url}");
    // Only the hash is kept.
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT token_hash FROM deploy_hooks WHERE site_id = $1")
            .bind(f.site.id)
            .fetch_one(f.app.pool())
            .await
            .unwrap();
    assert_eq!(stored.len(), 32);
    assert!(!String::from_utf8_lossy(&stored).contains("cdh_"));

    // No key, and the platform's own JSON payload, as Vercel and Netlify send it.
    let calls = f.calls_today().await;
    let res = f
        .app
        .send(request(
            Method::POST,
            &path,
            None,
            r#"{"type":"deployment.succeeded","payload":{"deployment":{"url":"x.vercel.app"}}}"#,
        ))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.body);
    let body = json(&res);
    assert_eq!(body["site_id"], f.site.id.to_string());
    assert_eq!(body["status"], "queued");
    assert_eq!(f.calls_today().await, calls + 1);
    let status = json(&f.hook(Method::GET).await);
    assert_eq!(status["active"], true);
    assert!(status["last_used_at"].is_string());

    // A new secret replaces the old one.
    let newer = json(&f.hook(Method::POST).await)["url"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(newer, url);
    let res = f.app.send(request(Method::POST, &path, None, "")).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(json(&res)["error"]["code"], "not_found");
    let res = f
        .app
        .send(request(Method::POST, &path_of(&newer), None, ""))
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED);
    assert_eq!(json(&res)["deduplicated"], true);

    // Removed: it stops working.
    assert_eq!(f.hook(Method::DELETE).await.status, StatusCode::OK);
    let res = f
        .app
        .send(request(Method::POST, &path_of(&newer), None, ""))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(json(&f.hook(Method::GET).await)["active"], false);
}

#[tokio::test]
async fn an_unknown_or_malformed_secret_is_a_404_and_costs_nothing() {
    let f = setup(Plan::Pro).await;
    let calls = f.calls_today().await;
    let unknown = format!("cdh_{}", "a".repeat(43));
    for secret in [unknown.as_str(), "cdh_short", "anything", "cdo_x"] {
        let res = f
            .app
            .send(request(
                Method::POST,
                &format!("/api/v1/hooks/deploy/{secret}"),
                None,
                "",
            ))
            .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{secret}");
    }
    // GET is the API's JSON 405.
    let res = f
        .app
        .send(request(
            Method::GET,
            &format!("/api/v1/hooks/deploy/{unknown}"),
            None,
            "",
        ))
        .await;
    assert_eq!(res.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(f.calls_today().await, calls);
    assert_eq!(f.crawls().await, 0);
}
