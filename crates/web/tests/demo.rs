//! `/demo`: the public, read-only audit of a sample site, on the cloud only. It needs no login,
//! offers no action, reads its data from a fixture and touches no table.

mod support;

use axum::http::StatusCode;
use support::{TestApp, cloud_config};

async fn rows(app: &TestApp) -> i64 {
    sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM accounts) + (SELECT count(*) FROM sites) \
              + (SELECT count(*) FROM crawls) + (SELECT count(*) FROM ai_incidents)",
    )
    .fetch_one(app.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn the_demo_is_public_read_only_and_labelled_as_sample_data() {
    let app = TestApp::with_config(cloud_config()).await;
    let before = rows(&app).await;
    let res = app.get("/demo", None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let body = &res.body;
    assert!(body.contains("Sample data"), "{body}");
    assert!(body.contains("www.example.com"));
    // The audit screen and its AI & GEO part, from the fixture.
    for part in [
        "Site audit",
        "PerplexityBot is blocked by robots.txt",
        "Fix these, in order",
        "Score trend",
        "AI bots",
        "AI access issues and their fixes",
        "User-agent: PerplexityBot",
    ] {
        assert!(body.contains(part), "missing {part:?}");
    }
    // Nothing to do on it: no crawl, export or links into a site's screens.
    for action in [
        "hx-post",
        "Run crawl",
        "export.csv",
        "href=\"/s/",
        "href=\"/demo/",
    ] {
        assert!(!body.contains(action), "{action:?} on the demo");
    }
    // No GEO score in the fixture: those cards read "—", and nothing to mark intended.
    assert!(body.contains("<b>—</b>"));
    assert!(!body.contains("Mark intended"));
    assert!(body.contains("id=\"fix-1\""));
    // A marketing page: indexable.
    assert!(!body.contains("noindex"));
    assert_eq!(rows(&app).await, before);
}

#[tokio::test]
async fn a_self_hosted_install_has_no_demo() {
    let app = TestApp::new().await;
    assert_eq!(app.get("/demo", None).await.status, StatusCode::NOT_FOUND);
}
