//! T5.1: health routes, the 503 page while Postgres is down, asset caching, the base layout,
//! and friendly error pages (full page, or inline for htmx).

mod support;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use codoseo_web::auth::mailer::Mailer;
use codoseo_web::{AppState, Config};
use sqlx::postgres::PgPoolOptions;
use support::TestApp;
use tower::ServiceExt;

#[tokio::test]
async fn health_routes() {
    let app = TestApp::new().await;
    let res = app.get("/healthz", None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body, "ok");
    let res = app.get("/readyz", None).await;
    assert_eq!(res.status, StatusCode::OK);
}

/// A pool pointing at a port nothing listens on, failing fast.
fn dead_state() -> AppState {
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(300))
        .connect_lazy("postgres://codoseo@127.0.0.1:1/codoseo")
        .expect("lazy pool");
    AppState::new(pool, Config::for_tests(), Mailer::Log)
}

async fn send(state: AppState, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let res = codoseo_web::app(state).oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn database_down_gives_503() {
    let (status, _, _) = send(
        dead_state(),
        Request::get("/readyz").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    // A page that needs the database (the session lookup) gets the friendly 503 page.
    let (status, headers, body) = send(
        dead_state(),
        Request::get("/sites")
            .header(header::COOKIE, "codoseo_session=whatever")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(headers.get(header::RETRY_AFTER).unwrap(), "10");
    assert!(body.contains("reach its database"), "{body}");
    assert!(body.contains("http-equiv=\"refresh\""));

    // The same failure inside an htmx partial renders inline.
    let (status, _, body) = send(
        dead_state(),
        Request::get("/sites")
            .header(header::COOKIE, "codoseo_session=whatever")
            .header("hx-request", "true")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("inline-error"), "{body}");
    assert!(!body.contains("<html"));
}

#[tokio::test]
async fn hashed_assets_are_cached_for_a_year() {
    let app = TestApp::new().await;
    let css = codoseo_web::assets::url("app.css");
    let res = app.get(css, None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    assert!(res.header("content-type").unwrap().starts_with("text/css"));
    assert!(res.body.contains("--accent"));

    let gz = app
        .send(
            Request::get(css)
                .header(header::ACCEPT_ENCODING, "br, gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(gz.header("content-encoding"), Some("gzip"));

    let font = app
        .get(codoseo_web::assets::url("Geist-Variable.woff2"), None)
        .await;
    assert_eq!(font.status, StatusCode::OK);
    assert_eq!(font.header("content-type"), Some("font/woff2"));

    let stale = app.get("/assets/app.00000000.css", None).await;
    assert_eq!(stale.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn base_layout_renders_the_shell() {
    let app = TestApp::new().await;
    let (_, cookie) = app.login("ana@example.com").await;
    let res = app.get("/sites", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let b = &res.body;
    for needle in [
        r#"id="sidebar""#,
        r#"id="nav""#,
        r#"id="crumbs""#,
        r#"id="main""#,
        "⌘K",
        r#"id="palette""#,
        "codoseo-theme",
        codoseo_web::assets::url("app.css"),
        codoseo_web::assets::url("htmx.min.js"),
        "ana@example.com",
        "self-hosted · owner",
        "Add your first site",
    ] {
        assert!(b.contains(needle), "missing {needle:?}");
    }
    assert_eq!(res.header("cache-control"), Some("private, no-cache"));
    assert_eq!(res.header("x-frame-options"), Some("DENY"));
}

#[tokio::test]
async fn shell_shows_run_crawl_for_a_site() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let site = app.site(&account, "example.com").await;
    let res = app.get("/sites", Some(&cookie)).await;
    // The site appears in the switcher and the site list.
    assert!(res.body.contains("example.com"));
    assert!(res.body.contains(&format!("/s/{}/audit", site.id)));
}

#[tokio::test]
async fn unknown_pages_get_a_friendly_404() {
    let app = TestApp::new().await;
    let res = app.get("/nope", None).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert!(res.body.contains("Page not found"));
    assert!(res.body.contains("<html"));

    let res = app.get_hx("/nope", None).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert!(res.body.contains("inline-error"));
    assert!(res.body.contains("data-error-message"));
}

#[tokio::test]
async fn signed_out_visitors_are_sent_to_login() {
    let app = TestApp::new().await;
    let res = app.get("/sites?x=1", None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), Some("/login?next=%2Fsites%3Fx%3D1"));

    // htmx requests get HX-Redirect instead of a redirect htmx would follow inline.
    let res = app.get_hx("/sites", None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.header("hx-redirect").unwrap().starts_with("/login?next="));
}
