//! T5.2: magic links, GitHub and Google OAuth, sessions, the `Origin` check, site ownership, the
//! self-hosted owner and closed signups, and Review focus 5 (email variants are one account).

mod support;

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::routing::{get, post};
use codoseo_testkit::TestServer;
use codoseo_web::auth::CurrentUser;
use codoseo_web::config::{GithubConfig, GoogleConfig};
use codoseo_web::{Config, Mode};
use support::TestApp;
use uuid::Uuid;

/// Requests a magic link for `email` and returns the token from the captured email.
async fn request_link(app: &TestApp, email: &str, next: &str) -> String {
    let form = format!(
        "email={}&next={}",
        codoseo_web::auth::urlencode(email),
        codoseo_web::auth::urlencode(next)
    );
    let res = app.post("/login", &form, None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("Check your email"));
    let mail = app.mail.lock().unwrap().last().cloned().expect("an email");
    assert_eq!(mail.to, email.trim());
    let marker = "/auth/magic/";
    let start = mail.text.find(marker).expect("link in email") + marker.len();
    mail.text[start..]
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}

/// Clicks the link: POSTs the token and returns the session cookie and redirect target.
async fn consume(app: &TestApp, token: &str) -> (StatusCode, Option<String>, Option<String>) {
    let res = app.post(&format!("/auth/magic/{token}"), "", None).await;
    (
        res.status,
        res.cookie("codoseo_session"),
        res.location().map(str::to_owned),
    )
}

async fn account_count(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts")
        .fetch_one(app.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn magic_link_signs_in_and_returns_to_next() {
    let app = TestApp::new().await;
    let token = request_link(&app, "ana@example.com", "/sites?tab=1").await;

    // The emailed link opens a confirm page without using the token...
    let page = app.get(&format!("/auth/magic/{token}"), None).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Signing you in"));

    // ...and its POST signs in.
    let (status, cookie, location) = consume(&app, &token).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location.as_deref(), Some("/sites?tab=1"));
    let cookie = cookie.expect("session cookie");
    let res = app.get("/sites", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("ana@example.com"));

    // Signed in, /login goes straight on.
    let res = app.get("/login?next=/account", Some(&cookie)).await;
    assert_eq!(res.location(), Some("/account"));
}

#[tokio::test]
async fn reused_and_expired_tokens_are_rejected() {
    let app = TestApp::new().await;
    let token = request_link(&app, "ana@example.com", "/").await;
    assert_eq!(consume(&app, &token).await.0, StatusCode::SEE_OTHER);
    let (status, cookie, _) = consume(&app, &token).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(cookie.is_none());

    let token = request_link(&app, "ana@example.com", "/").await;
    sqlx::query(
        "UPDATE login_tokens SET expires_at = now() - interval '1 second' WHERE used_at IS NULL",
    )
    .execute(app.pool())
    .await
    .unwrap();
    let res = app.post(&format!("/auth/magic/{token}"), "", None).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.body.contains("expired or was already used"));

    // A token nobody issued is just as dead.
    assert_eq!(
        consume(&app, "not-a-real-token").await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn tokens_are_stored_hashed() {
    let app = TestApp::new().await;
    let token = request_link(&app, "ana@example.com", "/").await;
    let stored: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM login_tokens")
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_ne!(stored, token.as_bytes());
    assert_eq!(stored, codoseo_web::auth::session::hash(&token));
}

#[tokio::test]
async fn invalid_email_shows_an_inline_error() {
    let app = TestApp::new().await;
    let res = app.post_hx("/login", "email=not-an-email", None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.body.contains("doesn&#39;t look like an email")
            || res.body.contains("look like an email")
    );
    assert!(!res.body.contains("<html"), "htmx gets just the card");
    assert!(app.mail.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cross_origin_posts_are_rejected() {
    let app = TestApp::new().await;
    let (_, cookie) = app.login("ana@example.com").await;

    let evil = axum::http::Request::post("/logout")
        .header("origin", "https://evil.example")
        .header("cookie", &cookie)
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.send(evil).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    // The session survived.
    assert_eq!(
        app.get("/sites", Some(&cookie)).await.status,
        StatusCode::OK
    );

    // No Origin and a foreign Referer: rejected. A same-site Referer is fine.
    let referer = |r: &str| {
        axum::http::Request::post("/login")
            .header("origin", "null")
            .header("referer", r)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(axum::body::Body::from("email=a%40example.com"))
            .unwrap()
    };
    assert_eq!(
        app.send(referer("https://evil.example/x")).await.status,
        StatusCode::FORBIDDEN
    );

    let no_origin = axum::http::Request::post("/login")
        .header("referer", format!("{}/login", app.origin()))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(axum::body::Body::from("email=a%40example.com"))
        .unwrap();
    // `send` adds our Origin only when none is set; strip it by building a request it won't touch.
    let res = codoseo_web::app(app.state.clone())
        .oneshot(no_origin)
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

use tower::ServiceExt;

#[tokio::test]
async fn another_accounts_site_is_not_found() {
    let app = TestApp::new().await;
    let (ana, _) = app.login("ana@example.com").await;
    let (bo, _) = app.login("bo@example.com").await;
    let site = app.site(&ana, "ana.example").await;

    let as_user = |account| CurrentUser {
        account,
        session_id: Uuid::nil(),
        last_site: None,
    };
    let found = codoseo_web::auth::load_site(&app.state, &as_user(ana.clone()), site.id).await;
    assert_eq!(found.unwrap().id, site.id);
    let other = codoseo_web::auth::load_site(&app.state, &as_user(bo), site.id).await;
    assert!(matches!(other, Err(codoseo_web::error::AppError::NotFound)));
}

/// Review focus 5: one person using email variants gets one account.
#[tokio::test]
async fn email_variants_resolve_to_one_account() {
    let app = TestApp::new().await;
    for email in ["Ana@Gmail.com", "a.na+seo@gmail.com", "ANA@googlemail.com"] {
        let token = request_link(&app, email, "/").await;
        let (status, cookie, _) = consume(&app, &token).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert!(cookie.is_some());
    }
    assert_eq!(account_count(&app).await, 1);
    let (email, canonical): (String, String) =
        sqlx::query_as("SELECT email, email_canonical FROM accounts")
            .fetch_one(app.pool())
            .await
            .unwrap();
    // The first address is kept for sending; the key is canonical.
    assert_eq!(email, "Ana@Gmail.com");
    assert_eq!(canonical, "ana@gmail.com");
}

#[tokio::test]
async fn first_selfhosted_signup_becomes_owner_and_signups_can_close() {
    let app = TestApp::new().await;
    let token = request_link(&app, "owner@example.com", "/").await;
    let (_, owner_cookie, _) = consume(&app, &token).await;
    let owner_cookie = owner_cookie.unwrap();
    let token = request_link(&app, "member@example.com", "/").await;
    consume(&app, &token).await;

    let owners: Vec<(String, bool, String)> =
        sqlx::query_as("SELECT email, is_owner, plan::text FROM accounts ORDER BY created_at")
            .fetch_all(app.pool())
            .await
            .unwrap();
    assert_eq!(
        owners,
        vec![
            ("owner@example.com".into(), true, "self_hosted".into()),
            ("member@example.com".into(), false, "self_hosted".into()),
        ]
    );

    // Only the owner may close signups.
    let (_, member_cookie) = app.login("member@example.com").await;
    let res = app
        .post("/account/signups", "open=false", Some(&member_cookie))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = app
        .post("/account/signups", "open=false", Some(&owner_cookie))
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);

    // A new address is refused; an existing account still gets in.
    let token = request_link(&app, "stranger@example.com", "/").await;
    let res = app.post(&format!("/auth/magic/{token}"), "", None).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(res.body.contains("Signups are closed"));
    let token = request_link(&app, "member@example.com", "/").await;
    assert_eq!(consume(&app, &token).await.0, StatusCode::SEE_OTHER);
    assert_eq!(account_count(&app).await, 2);
}

#[tokio::test]
async fn cloud_accounts_start_free_and_never_own_the_instance() {
    let config = Config::from_lookup(|k| match k {
        "CODOSEO_MODE" => Some("cloud".into()),
        "BASE_URL" => Some("https://codoseo.com".into()),
        "SECRET_KEY" => Some("k".into()),
        "SMTP_URL" => Some("smtp://127.0.0.1:2525".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(config.mode, Mode::Cloud);
    let app = TestApp::with_config(config).await;
    let token = request_link(&app, "ana@example.com", "/").await;
    let res = app.post(&format!("/auth/magic/{token}"), "", None).await;
    let set_cookie = res.header("set-cookie").unwrap();
    assert!(set_cookie.contains("Secure"), "{set_cookie}");
    assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Lax"));
    let (plan, owner): (String, bool) = sqlx::query_as("SELECT plan::text, is_owner FROM accounts")
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!((plan.as_str(), owner), ("free", false));
}

#[tokio::test]
async fn logout_revokes_the_session() {
    let app = TestApp::new().await;
    let (_, cookie) = app.login("ana@example.com").await;
    assert_eq!(
        app.get("/sites", Some(&cookie)).await.status,
        StatusCode::OK
    );
    let res = app.post("/logout", "", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(res.header("set-cookie").unwrap().contains("Max-Age=0"));
    let res = app.get("/sites", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(res.location().unwrap().starts_with("/login"));
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn expired_sessions_are_rejected() {
    let app = TestApp::new().await;
    let (_, cookie) = app.login("ana@example.com").await;
    sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 second'")
        .execute(app.pool())
        .await
        .unwrap();
    assert_eq!(
        app.get("/sites", Some(&cookie)).await.status,
        StatusCode::SEE_OTHER
    );
}

#[tokio::test]
async fn next_cannot_leave_the_site() {
    let app = TestApp::new().await;
    let token = request_link(&app, "ana@example.com", "//evil.example/steal").await;
    let (_, _, location) = consume(&app, &token).await;
    assert_eq!(location.as_deref(), Some("/app"));
}

/// A stand-in for GitHub's OAuth and API endpoints.
async fn fake_github(seen_codes: Arc<Mutex<Vec<String>>>) -> TestServer {
    #[derive(serde::Deserialize)]
    struct TokenForm {
        code: String,
    }
    let codes = seen_codes.clone();
    let router = Router::new()
        .route(
            "/login/oauth/access_token",
            post(move |axum::Form(f): axum::Form<TokenForm>| {
                let codes = codes.clone();
                async move {
                    codes.lock().unwrap().push(f.code.clone());
                    if f.code == "good" {
                        Json(serde_json::json!({ "access_token": "gho_test" }))
                    } else {
                        Json(serde_json::json!({ "error_description": "bad code" }))
                    }
                }
            }),
        )
        .route(
            "/user",
            get(|| async { Json(serde_json::json!({ "id": 4242, "login": "ana" })) }),
        )
        .route(
            "/user/emails",
            get(
                |_: Query<std::collections::HashMap<String, String>>| async {
                    Json(serde_json::json!([
                        { "email": "old@example.com", "primary": false, "verified": true },
                        { "email": "Ana@Gmail.com", "primary": true, "verified": true }
                    ]))
                },
            ),
        );
    TestServer::start(router).await
}

fn github_config(gh: &TestServer) -> Config {
    let mut c = Config::for_tests();
    c.github = Some(GithubConfig {
        client_id: "cid".into(),
        client_secret: "secret".into(),
        authorize_url: gh.url("/login/oauth/authorize"),
        token_url: gh.url("/login/oauth/access_token"),
        api_url: gh.url("/"),
    });
    c
}

#[tokio::test]
async fn github_sign_in_links_to_the_email_account() {
    let codes = Arc::new(Mutex::new(Vec::new()));
    let gh = fake_github(codes.clone()).await;
    let app = TestApp::with_config(github_config(&gh)).await;

    // An account already exists for a variant of the GitHub primary email.
    let token = request_link(&app, "a.na@gmail.com", "/").await;
    consume(&app, &token).await;

    let start = app.get("/auth/github?next=/account", None).await;
    assert_eq!(start.status, StatusCode::SEE_OTHER);
    let authorize = url::Url::parse(start.location().unwrap()).unwrap();
    let state: String = authorize
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .unwrap();
    let oauth_cookie = start.cookie("codoseo_oauth").unwrap();

    // A forged state is refused before GitHub is called.
    let res = app
        .get(
            "/auth/github/callback?code=good&state=forged",
            Some(&oauth_cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(codes.lock().unwrap().is_empty());

    let res = app
        .get(
            &format!("/auth/github/callback?code=good&state={state}"),
            Some(&oauth_cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), Some("/account"));
    assert!(res.cookie("codoseo_session").is_some());

    assert_eq!(account_count(&app).await, 1);
    let github_id: Option<String> = sqlx::query_scalar("SELECT github_id FROM accounts")
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(github_id.as_deref(), Some("4242"));
}

#[tokio::test]
async fn github_routes_are_hidden_when_not_configured() {
    let app = TestApp::new().await;
    assert_eq!(
        app.get("/auth/github", None).await.status,
        StatusCode::NOT_FOUND
    );
    let page = app.get("/login", None).await;
    assert!(!page.body.contains("Continue with GitHub"));
}

/// Browsers strip tabs and newlines from a redirect address, so `/\t/evil.com` would become
/// `//evil.com`. Any such `next` falls back to `/app`.
#[tokio::test]
async fn next_with_control_characters_cannot_leave_the_site() {
    let app = TestApp::new().await;
    let (_, cookie) = app.login("ana@example.com").await;
    for next in [
        "/%09/evil.example",
        "/%0A/evil.example",
        "/%0D%0A/evil.example",
    ] {
        let res = app.get(&format!("/login?next={next}"), Some(&cookie)).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{next}");
        assert_eq!(res.location(), Some("/app"), "{next}");
    }
    let token = request_link(&app, "bo@example.com", "/\t/evil.example").await;
    let (status, _, location) = consume(&app, &token).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location.as_deref(), Some("/app"));
}

/// An account written before `email_canonical` existed is found by its address and keyed,
/// instead of a sign-in trying to create a duplicate.
#[tokio::test]
async fn legacy_accounts_without_a_canonical_key_still_sign_in() {
    let app = TestApp::new().await;
    let legacy: Uuid =
        sqlx::query_scalar("INSERT INTO accounts (email) VALUES ('A.Na@Gmail.com') RETURNING id")
            .fetch_one(app.pool())
            .await
            .unwrap();
    for email in ["a.na@gmail.com", "ana+seo@gmail.com"] {
        let token = request_link(&app, email, "/").await;
        assert_eq!(
            consume(&app, &token).await.0,
            StatusCode::SEE_OTHER,
            "{email}"
        );
    }
    assert_eq!(account_count(&app).await, 1);
    let canonical: Option<String> =
        sqlx::query_scalar("SELECT email_canonical FROM accounts WHERE id = $1")
            .bind(legacy)
            .fetch_one(app.pool())
            .await
            .unwrap();
    assert_eq!(canonical.as_deref(), Some("ana@gmail.com"));
}

/// A stand-in for Google's token and userinfo endpoints. The code picks the user: `good` is a
/// verified Gmail address, `unverified` an unverified one.
async fn fake_google(seen: Arc<Mutex<Vec<(String, String)>>>) -> TestServer {
    #[derive(serde::Deserialize)]
    struct TokenForm {
        code: String,
        grant_type: String,
    }
    let router = Router::new()
        .route(
            "/token",
            post(move |axum::Form(f): axum::Form<TokenForm>| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push((f.code.clone(), f.grant_type));
                    match f.code.as_str() {
                        "good" | "unverified" => {
                            Json(serde_json::json!({ "access_token": format!("ya29.{}", f.code) }))
                        }
                        _ => Json(serde_json::json!({ "error_description": "bad code" })),
                    }
                }
            }),
        )
        .route(
            "/userinfo",
            get(|headers: axum::http::HeaderMap| async move {
                let auth = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default();
                if auth == "Bearer ya29.good" {
                    Json(serde_json::json!({
                        "sub": "109876543210", "email": "Ana.Lima@gmail.com", "email_verified": true
                    }))
                } else {
                    Json(serde_json::json!({
                        "sub": "555", "email": "eve@example.com", "email_verified": false
                    }))
                }
            }),
        );
    TestServer::start(router).await
}

fn google_config(g: &TestServer) -> Config {
    let mut c = Config::for_tests();
    c.google = Some(GoogleConfig {
        client_id: "gid".into(),
        client_secret: "secret".into(),
        authorize_url: g.url("/auth"),
        token_url: g.url("/token"),
        userinfo_url: g.url("/userinfo"),
    });
    c
}

/// Starts the Google flow and returns the `state` it sent and the state cookie.
async fn start_google(app: &TestApp, next: &str) -> (String, String) {
    let start = app.get(&format!("/auth/google?next={next}"), None).await;
    assert_eq!(start.status, StatusCode::SEE_OTHER);
    let authorize = url::Url::parse(start.location().unwrap()).unwrap();
    let param = |name: &str| {
        authorize
            .query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    assert_eq!(param("client_id").as_deref(), Some("gid"));
    assert_eq!(param("scope").as_deref(), Some("openid email"));
    assert_eq!(param("response_type").as_deref(), Some("code"));
    assert!(
        param("redirect_uri")
            .unwrap()
            .ends_with("/auth/google/callback")
    );
    (
        param("state").unwrap(),
        start.cookie("codoseo_oauth").unwrap(),
    )
}

#[tokio::test]
async fn google_sign_in_links_to_the_email_account() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let g = fake_google(seen.clone()).await;
    let app = TestApp::with_config(google_config(&g)).await;

    let page = app.get("/login", None).await;
    assert!(page.body.contains("Continue with Google"));
    assert!(!page.body.contains("Continue with GitHub"));

    // An account already exists for a variant of the Google address.
    let token = request_link(&app, "analima+seo@gmail.com", "/").await;
    consume(&app, &token).await;

    let (state, oauth_cookie) = start_google(&app, "/account").await;

    // A forged state is refused before Google is called.
    let res = app
        .get(
            "/auth/google/callback?code=good&state=forged",
            Some(&oauth_cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(seen.lock().unwrap().is_empty());

    let res = app
        .get(
            &format!("/auth/google/callback?code=good&state={state}"),
            Some(&oauth_cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), Some("/account"));
    assert!(res.cookie("codoseo_session").is_some());
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[("good".to_owned(), "authorization_code".to_owned())]
    );

    assert_eq!(account_count(&app).await, 1);
    let google_id: Option<String> = sqlx::query_scalar("SELECT google_id FROM accounts")
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(google_id.as_deref(), Some("109876543210"));

    // Once linked, the Google id finds the account even if its email has since changed.
    sqlx::query(
        "UPDATE accounts SET email = 'new@example.com', email_canonical = 'new@example.com'",
    )
    .execute(app.pool())
    .await
    .unwrap();
    let (state, oauth_cookie) = start_google(&app, "/app").await;
    let res = app
        .get(
            &format!("/auth/google/callback?code=good&state={state}"),
            Some(&oauth_cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(account_count(&app).await, 1);
}

#[tokio::test]
async fn google_sign_in_creates_an_account() {
    let g = fake_google(Arc::new(Mutex::new(Vec::new()))).await;
    let app = TestApp::with_config(google_config(&g)).await;
    let (state, oauth_cookie) = start_google(&app, "/app").await;
    let res = app
        .get(
            &format!("/auth/google/callback?code=good&state={state}"),
            Some(&oauth_cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), Some("/app"));
    let (email, google_id): (String, Option<String>) =
        sqlx::query_as("SELECT email, google_id FROM accounts")
            .fetch_one(app.pool())
            .await
            .unwrap();
    assert_eq!(email, "Ana.Lima@gmail.com");
    assert_eq!(google_id.as_deref(), Some("109876543210"));
}

#[tokio::test]
async fn google_sign_in_refuses_an_unverified_email() {
    let g = fake_google(Arc::new(Mutex::new(Vec::new()))).await;
    let app = TestApp::with_config(google_config(&g)).await;
    let (state, oauth_cookie) = start_google(&app, "/app").await;
    let res = app
        .get(
            &format!("/auth/google/callback?code=unverified&state={state}"),
            Some(&oauth_cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.body.contains("no verified email"), "{}", res.body);
    assert_eq!(account_count(&app).await, 0);
}

#[tokio::test]
async fn google_routes_are_hidden_when_not_configured() {
    let app = TestApp::new().await;
    for path in ["/auth/google", "/auth/google/callback?code=x&state=y"] {
        assert_eq!(app.get(path, None).await.status, StatusCode::NOT_FOUND);
    }
    let page = app.get("/login", None).await;
    assert!(!page.body.contains("Continue with Google"));
    assert!(!page.body.contains("class=\"or\""));
}

/// Makes one kind of write fail with a database error until [`allow_writes`]. Each test owns a
/// fresh database, so the trigger cannot leak into another test.
async fn fail_writes(app: &TestApp, trigger: &str) {
    sqlx::query(
        "CREATE FUNCTION codoseo_test_fail() RETURNS trigger LANGUAGE plpgsql \
         AS 'BEGIN RAISE EXCEPTION ''injected failure''; END'",
    )
    .execute(app.pool())
    .await
    .unwrap();
    sqlx::query(&format!(
        "CREATE TRIGGER codoseo_test_fail {trigger} FOR EACH ROW EXECUTE FUNCTION codoseo_test_fail()"
    ))
    .execute(app.pool())
    .await
    .unwrap();
}

async fn allow_writes(app: &TestApp) {
    sqlx::query("DROP FUNCTION codoseo_test_fail() CASCADE")
        .execute(app.pool())
        .await
        .unwrap();
}

async fn session_count(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(app.pool())
        .await
        .unwrap()
}

/// A database error partway through signing in leaves no session behind and the link usable,
/// whichever step failed.
#[tokio::test]
async fn a_failed_sign_in_keeps_the_link_and_leaves_no_session() {
    for trigger in [
        "BEFORE INSERT ON accounts",
        "BEFORE INSERT ON sessions",
        "BEFORE UPDATE OF last_login_at ON accounts",
        "BEFORE UPDATE OF last_email_click_at ON accounts",
    ] {
        let app = TestApp::new().await;
        let token = request_link(&app, "retry@example.com", "/account").await;
        fail_writes(&app, trigger).await;
        let (status, cookie, _) = consume(&app, &token).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{trigger}");
        assert!(cookie.is_none(), "{trigger}");
        assert_eq!(session_count(&app).await, 0, "{trigger}");

        allow_writes(&app).await;
        let (status, cookie, location) = consume(&app, &token).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{trigger}");
        assert!(cookie.is_some(), "{trigger}");
        assert_eq!(location.as_deref(), Some("/account"), "{trigger}");
        assert_eq!(session_count(&app).await, 1, "{trigger}");
        // Restored once is still single use.
        assert_eq!(
            consume(&app, &token).await.0,
            StatusCode::BAD_REQUEST,
            "{trigger}"
        );
    }
}

/// A refusal is not retried: the link stays spent.
#[tokio::test]
async fn a_refused_sign_in_still_spends_the_link() {
    let app = TestApp::new().await;
    let token = request_link(&app, "owner@example.com", "/").await;
    let (_, owner_cookie, _) = consume(&app, &token).await;
    let res = app
        .post(
            "/account/signups",
            "open=false",
            Some(&owner_cookie.unwrap()),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);

    let token = request_link(&app, "stranger@example.com", "/").await;
    assert_eq!(consume(&app, &token).await.0, StatusCode::FORBIDDEN);
    let res = app
        .post(
            "/account/signups",
            "open=true",
            Some(&app.login("owner@example.com").await.1),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(consume(&app, &token).await.0, StatusCode::BAD_REQUEST);
}

/// Cancelling on GitHub's side clears the OAuth state cookie.
#[tokio::test]
async fn github_error_callback_clears_the_state_cookie() {
    let gh = fake_github(Arc::new(Mutex::new(Vec::new()))).await;
    let app = TestApp::with_config(github_config(&gh)).await;
    let start = app.get("/auth/github?next=/account", None).await;
    let oauth_cookie = start.cookie("codoseo_oauth").unwrap();

    let res = app
        .get(
            "/auth/github/callback?error=access_denied&state=x",
            Some(&oauth_cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), Some("/login"));
    assert_eq!(
        res.cookie("codoseo_oauth").as_deref(),
        Some("codoseo_oauth=")
    );
    assert!(res.cookie("codoseo_session").is_none());
}

/// Cancelling on Google's side clears the OAuth state cookie too.
#[tokio::test]
async fn google_error_callback_clears_the_state_cookie() {
    let g = fake_google(Arc::new(Mutex::new(Vec::new()))).await;
    let app = TestApp::with_config(google_config(&g)).await;
    let (_, oauth_cookie) = start_google(&app, "/account").await;

    let res = app
        .get(
            "/auth/google/callback?error=access_denied&state=x",
            Some(&oauth_cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), Some("/login"));
    assert_eq!(
        res.cookie("codoseo_oauth").as_deref(),
        Some("codoseo_oauth=")
    );
    assert!(res.cookie("codoseo_session").is_none());
}

/// "Continue with Google" on the free audit's unlock card attaches that audit, as the emailed
/// link does: the claim cookie moves the audit itself into the new account.
#[tokio::test]
async fn google_from_the_unlock_card_attaches_the_audit() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let g = fake_google(seen.clone()).await;
    let mut config = support::cloud_config();
    config.google = google_config(&g).google;
    let app = TestApp::with_config(config).await;

    let started = app.post("/audit", "url=example.com", None).await;
    assert_eq!(started.status, StatusCode::SEE_OTHER, "{}", started.body);
    let crawl =
        Uuid::parse_str(started.location().unwrap().strip_prefix("/audit/").unwrap()).unwrap();
    let claim = started.cookie("codoseo_audit").expect("claim cookie");
    app.finalize_crawl(
        crawl,
        vec![support::page("example.com", "/")],
        Vec::new(),
        codoseo_core::output::StopReason::Completed,
    )
    .await;

    let report = app.get(&format!("/audit/{crawl}"), Some(&claim)).await;
    assert!(
        report
            .body
            .contains(&format!(r#"href="/auth/google?audit={crawl}""#)),
        "{}",
        report.body
    );
    assert!(
        report.body.contains("q-quota"),
        "the meter sits in the head"
    );

    let start = app
        .get(&format!("/auth/google?audit={crawl}"), Some(&claim))
        .await;
    assert_eq!(start.status, StatusCode::SEE_OTHER);
    let authorize = url::Url::parse(start.location().unwrap()).unwrap();
    let state = authorize
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .unwrap();
    let oauth_cookie = start.cookie("codoseo_oauth").unwrap();

    let res = app
        .get(
            &format!("/auth/google/callback?code=good&state={state}"),
            Some(&format!("{oauth_cookie}; {claim}")),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(res.cookie("codoseo_session").is_some());
    let (site, owner, claim_hash): (Uuid, Option<Uuid>, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT s.id, s.account_id, s.claim_token_hash FROM crawls c JOIN sites s ON s.id = c.site_id WHERE c.id = $1",
    )
    .bind(crawl)
    .fetch_one(app.pool())
    .await
    .unwrap();
    let account: Uuid = sqlx::query_scalar("SELECT id FROM accounts")
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!((owner, claim_hash), (Some(account), None));
    assert_eq!(res.location(), Some(format!("/s/{site}/audit").as_str()));
}
