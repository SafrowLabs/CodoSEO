//! The public "pages crawled" counter: every finished crawl adds to it, free audits included; the
//! cloud landing shows it and `/stats/pages-crawled` serves it live; the website count is stored
//! but shown only on `/admin`.

mod support;

use axum::http::StatusCode;
use codoseo_core::output::StopReason;
use support::{TestApp, cloud_config, cloud_config_with, page};
use uuid::Uuid;

/// A no-signup audit of `domain`, submitted from the landing page and finished with `pages` pages.
async fn free_audit(app: &TestApp, domain: &str, pages: usize) {
    let res = app.post("/audit", &format!("url={domain}"), None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let id = res.location().unwrap().strip_prefix("/audit/").unwrap();
    let crawl = Uuid::parse_str(id).unwrap();
    let pages = (0..pages)
        .map(|i| page(domain, &format!("/p{i}")))
        .collect();
    app.finalize_crawl(crawl, pages, Vec::new(), StopReason::Completed)
        .await;
}

#[tokio::test]
async fn finished_crawls_and_free_audits_count_on_the_cloud_landing() {
    let app =
        TestApp::with_config(cloud_config_with(&[("ADMIN_EMAILS", "boss@example.com")])).await;

    // A free audit, and a monitored crawl of the same domain.
    free_audit(&app, "example.com", 3).await;
    let (account, cookie) = app.login("boss@example.com").await;
    let site = app.site(&account, "example.com").await;
    let pages = (0..1200)
        .map(|i| page("example.com", &format!("/m{i}")))
        .collect();
    app.finished_crawl(&site, pages, Vec::new()).await;
    let totals = codoseo_store::stats::totals(app.pool()).await.unwrap();
    assert_eq!(
        (totals.pages_crawled, totals.crawls, totals.websites),
        (1203, 2, 1)
    );

    // The landing shows the page total, formatted, and where to poll it.
    let res = app.get("/", None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.body.contains(
            r#"<b id="pages-crawled" class="mono" data-poll="/stats/pages-crawled">1,203</b> pages crawled"#
        ),
        "{}",
        res.body
    );
    let res = app.get("/stats/pages-crawled", None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.header("cache-control"), Some("public, max-age=10"));
    let json: serde_json::Value = serde_json::from_str(&res.body).unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "pages_crawled": 1203, "label": "1,203" })
    );

    // The website count is stored but not public: only the admin page shows it.
    let landing = app.get("/", None).await.body;
    assert!(!landing.to_lowercase().contains("websites"), "{landing}");
    let admin = app.get("/admin", Some(&cookie)).await.body;
    assert!(
        admin.contains(r#"<b class="mono" data-total="websites">1</b>"#),
        "{admin}"
    );
    assert!(admin.contains(r#"<b class="mono" data-total="pages crawled">1,203</b>"#));
}

#[tokio::test]
async fn a_free_audit_on_its_own_counts() {
    let app = TestApp::with_config(cloud_config()).await;
    free_audit(&app, "solo.example", 5).await;
    let res = app.get("/stats/pages-crawled", None).await;
    let json: serde_json::Value = serde_json::from_str(&res.body).unwrap();
    assert_eq!(json["pages_crawled"], 5);

    // Within the cache's few seconds, landing traffic doesn't go back to the database.
    free_audit(&app, "other.example", 2).await;
    let stored = codoseo_store::stats::totals(app.pool()).await.unwrap();
    assert_eq!(stored.pages_crawled, 7);
    let res = app.get("/stats/pages-crawled", None).await;
    let json: serde_json::Value = serde_json::from_str(&res.body).unwrap();
    assert_eq!(json["pages_crawled"], 5, "served from the cache");
}

#[tokio::test]
async fn self_hosted_has_no_public_counter() {
    let app = TestApp::new().await;
    let (account, _) = app.login("owner@example.com").await;
    let site = app.site(&account, "example.com").await;
    app.finished_crawl(&site, vec![page("example.com", "/")], Vec::new())
        .await;

    assert_eq!(
        app.get("/stats/pages-crawled", None).await.status,
        StatusCode::NOT_FOUND
    );
    // Signed out, `/` is the sign-in page there, never the landing.
    let res = app.get("/", None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let login = app.get("/login", None).await.body;
    assert!(!login.contains("pages-crawled"), "{login}");
}
