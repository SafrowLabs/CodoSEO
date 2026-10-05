//! T5.2: magic links, GitHub OAuth, sessions, the `Origin` check, site ownership, the
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
use codoseo_web::config::GithubConfig;
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
    assert_eq!(location.as_deref(), Some("/"));
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
/// `//evil.com`. Any such `next` falls back to `/`.
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
        assert_eq!(res.location(), Some("/"), "{next}");
    }
    let token = request_link(&app, "bo@example.com", "/\t/evil.example").await;
    let (status, _, location) = consume(&app, &token).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location.as_deref(), Some("/"));
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
