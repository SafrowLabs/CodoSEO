//! The site graph's frame: every tab answers for the site's owner (empty before the first
//! crawl, the tab bar after it), `/graph` opens the bubble map and another account gets a 404.

mod support;

use axum::http::StatusCode;
use support::{TestApp, page};

const DOMAIN: &str = "example.com";
const TABS: [&str; 8] = [
    "tree", "folders", "map", "sitemap", "links", "changes", "llms", "reach",
];

#[tokio::test]
async fn graph_opens_the_bubble_map() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    let res = app
        .get(&format!("/s/{}/graph", site.id), Some(&cookie))
        .await;
    assert!(res.status.is_redirection());
    assert_eq!(
        res.location(),
        Some(format!("/s/{}/graph/map", site.id).as_str())
    );
}

#[tokio::test]
async fn depth_lands_on_its_part_of_the_sitemap_tab() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    let res = app
        .get(&format!("/s/{}/graph/depth", site.id), Some(&cookie))
        .await;
    assert!(res.status.is_redirection());
    assert_eq!(
        res.location(),
        Some(format!("/s/{}/graph/sitemap#depth", site.id).as_str())
    );
}

#[tokio::test]
async fn every_tab_shows_the_empty_state_before_the_first_crawl() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    for tab in TABS {
        let res = app
            .get(&format!("/s/{}/graph/{tab}", site.id), Some(&cookie))
            .await;
        assert_eq!(res.status, StatusCode::OK, "{tab}");
        assert!(res.body.contains("No finished crawl yet"), "{tab}");
        assert!(
            res.body.contains(&format!(
                r#"href="/s/{}/graph/{tab}" aria-current="page""#,
                site.id
            )),
            "{tab}"
        );
    }
}

#[tokio::test]
async fn every_tab_renders_after_a_crawl() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    app.finished_crawl(
        &site,
        vec![page(DOMAIN, "/"), page(DOMAIN, "/about")],
        vec![],
    )
    .await;
    for tab in TABS {
        let res = app
            .get(&format!("/s/{}/graph/{tab}", site.id), Some(&cookie))
            .await;
        assert_eq!(res.status, StatusCode::OK, "{tab}");
        assert!(!res.body.contains("No finished crawl yet"), "{tab}");
    }
}

#[tokio::test]
async fn another_accounts_site_is_a_404() {
    let app = TestApp::new().await;
    let (account, _) = app.login("a@x.com").await;
    let (_, other) = app.login("b@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    let res = app
        .get(&format!("/s/{}/graph", site.id), Some(&other))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    for tab in TABS {
        let res = app
            .get(&format!("/s/{}/graph/{tab}", site.id), Some(&other))
            .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{tab}");
    }
}
