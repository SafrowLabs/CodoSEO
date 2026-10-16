//! The app sidebar (audit v6): one sidebar on every page. THIS SITE (Site audit first) and the
//! crawler card follow the user's current site even on screens without one in view; WORKSPACE
//! is the same everywhere; only the highlight moves.

mod support;

use axum::http::StatusCode;
use support::{TestApp, cloud_config};

/// The `<aside>` of a page.
fn sidebar(body: &str) -> &str {
    let start = body.find("<aside class=\"sidebar\"").expect("a sidebar");
    let end = start + body[start..].find("</aside>").expect("the sidebar closes");
    &body[start..end]
}

/// Where `needle` sits in `hay`, failing the test if it is missing.
fn at(hay: &str, needle: &str) -> usize {
    hay.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in the sidebar"))
}

#[tokio::test]
async fn site_audit_leads_and_the_reports_group_is_gone() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let site = app.site(&account, "ana.example").await;

    let res = app
        .get(&format!("/s/{}/explorer", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let nav = sidebar(&res.body);
    assert!(nav.contains("THIS SITE"));
    assert!(!nav.contains("REPORTS"));
    assert!(!nav.contains("PROJECT"));
    for gone in [
        "Response codes",
        "Redirects",
        "Indexability",
        "Page titles",
        "Image alt text",
    ] {
        assert!(!nav.contains(gone), "{gone} is still in the sidebar");
    }
    assert!(!nav.contains("Schedule"));

    let base = format!("/s/{}", site.id);
    let order: Vec<usize> = [
        "audit",
        "explorer",
        "ai-access",
        "changes",
        "crawls",
        "policy",
    ]
    .iter()
    .map(|p| at(nav, &format!("href=\"{base}/{p}\"")))
    .collect();
    assert!(order.is_sorted(), "THIS SITE is out of order: {order:?}");
    assert!(at(nav, "THIS SITE") < at(nav, "WORKSPACE"));
    assert!(nav.contains(&format!(
        "class=\"nav-item is-active\" href=\"{base}/explorer\""
    )));
}

#[tokio::test]
async fn a_settings_page_keeps_the_current_site_in_the_sidebar() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let site = app.site(&account, "ana.example").await;

    let res = app.get("/account", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK);
    let nav = sidebar(&res.body);
    assert!(nav.contains("THIS SITE"));
    assert!(!nav.contains("ACCOUNT"));
    assert!(nav.contains("id=\"crawler\""));
    assert!(nav.contains("ana.example"));
    assert!(nav.contains(&format!("href=\"/s/{}/audit\"", site.id)));
    assert!(nav.contains("class=\"nav-item is-active\" href=\"/account\""));
    // The keyboard shortcuts follow the sidebar's site.
    assert!(res.body.contains(&format!("data-site=\"/s/{}\"", site.id)));
    // The header stays out of it: no breadcrumb site or Run crawl without a site in view.
    assert!(!res.body.contains("id=\"run-crawl\""));
}

#[tokio::test]
async fn the_current_site_is_the_one_last_in_view() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("ana@example.com").await;
    let first = app.site(&account, "first.example").await;
    let second = app.site(&account, "second.example").await;
    let nav_for = |site: uuid::Uuid| format!("href=\"/s/{site}/crawls\"");

    // With nothing remembered, it is the first site, where `/` goes.
    let res = app.get("/settings/alerts", Some(&cookie)).await;
    assert!(sidebar(&res.body).contains(&nav_for(first.id)));

    let res = app
        .get(&format!("/s/{}/audit", second.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let remembered = res.cookie("codoseo_site").expect("the site is remembered");
    assert_eq!(remembered, format!("codoseo_site={}", second.id));

    let both = format!("{cookie}; {remembered}");
    let res = app.get("/settings/alerts", Some(&both)).await;
    let nav = sidebar(&res.body);
    assert!(nav.contains(&nav_for(second.id)));
    assert!(!nav.contains(&nav_for(first.id)));

    // Another account's site in the cookie is ignored.
    let (bo, bo_cookie) = app.login("bo@example.com").await;
    let own = app.site(&bo, "bo.example").await;
    let res = app
        .get("/sites", Some(&format!("{bo_cookie}; {remembered}")))
        .await;
    let nav = sidebar(&res.body);
    assert!(nav.contains(&nav_for(own.id)));
    assert!(!res.body.contains("second.example"));
}

#[tokio::test]
async fn with_no_site_there_is_no_site_group_or_crawler() {
    let app = TestApp::new().await;
    let (_, cookie) = app.login("ana@example.com").await;

    for path in ["/account", "/sites", "/settings/api-keys"] {
        let res = app.get(path, Some(&cookie)).await;
        assert_eq!(res.status, StatusCode::OK, "{path}");
        let nav = sidebar(&res.body);
        assert!(!nav.contains("THIS SITE"), "{path}");
        assert!(!nav.contains("id=\"crawler\""), "{path}");
        assert!(nav.contains("WORKSPACE"), "{path}");
        assert!(nav.contains("All sites"), "{path}");
    }
}

#[tokio::test]
async fn all_sites_is_highlighted_on_the_site_list() {
    let app = TestApp::with_config(cloud_config()).await;
    let (account, cookie) = app.login("ana@example.com").await;
    app.site(&account, "ana.example").await;

    let res = app.get("/sites", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK);
    let nav = sidebar(&res.body);
    assert!(nav.contains("class=\"nav-item is-active\" href=\"/sites\""));
    assert!(nav.contains("THIS SITE"));
    assert!(nav.contains("id=\"crawler\""));
    // WORKSPACE, in order, the same as on every page; a free account sees the upgrade nudge.
    let order: Vec<usize> = [
        "href=\"/sites\"",
        "href=\"/settings/alerts\"",
        "href=\"/settings/api-keys\"",
        "href=\"/billing\"",
        "href=\"/account\"",
    ]
    .iter()
    .map(|h| at(nav, h))
    .collect();
    assert!(order.is_sorted(), "WORKSPACE is out of order: {order:?}");
    assert!(nav.contains("upgrade"));
    assert!(!nav.contains("Schedule"));
}
