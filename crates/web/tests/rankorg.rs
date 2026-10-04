//! T6.3: RankOrg links on the audit preview and the explorer: the link carries the domain, the
//! top 10 pages by inlinks and UTM tags, and every click is counted on the way through.

mod support;

use axum::http::StatusCode;
use codoseo_core::output::StopReason;
use support::{TestApp, TestResponse, cloud_config_with, page};
use url::Url;
use uuid::Uuid;

const RANKORG: &str = "https://rankorg.example/start";

async fn app() -> TestApp {
    TestApp::with_config(cloud_config_with(&[("RANKORG_URL", RANKORG)])).await
}

fn crawl_id(res: &TestResponse) -> Uuid {
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    Uuid::parse_str(res.location().unwrap().strip_prefix("/audit/").unwrap()).unwrap()
}

/// Twelve pages whose inlinks are 12, 11, ... 1, so the order is known.
async fn audit_with_twelve_pages(app: &TestApp) -> Uuid {
    let crawl = crawl_id(&app.post("/audit", "url=example.com", None).await);
    let pages = (0..12)
        .map(|i| page("example.com", &format!("/p{i}")))
        .collect();
    app.finalize_crawl(crawl, pages, Vec::new(), StopReason::Completed)
        .await;
    for i in 0..12 {
        sqlx::query("UPDATE pages SET inlinks = $2 WHERE crawl_id = $1 AND url = $3")
            .bind(crawl)
            .bind(12 - i)
            .bind(format!("https://example.com/p{i}"))
            .execute(app.pool())
            .await
            .unwrap();
    }
    crawl
}

fn params(location: &str) -> Vec<(String, String)> {
    Url::parse(location)
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[tokio::test]
async fn the_audit_link_carries_the_domain_the_top_ten_pages_and_utm_tags() {
    let app = app().await;
    let crawl = audit_with_twelve_pages(&app).await;

    let res = app
        .get(&format!("/go/rankorg?src=audit&audit={crawl}"), None)
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let location = res.location().unwrap();
    assert!(location.starts_with(RANKORG), "{location}");
    let q = params(location);
    let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    assert_eq!(get("domain"), Some("example.com"));
    assert_eq!(get("utm_source"), Some("codoseo"));
    assert_eq!(get("utm_medium"), Some("audit_preview"));
    assert_eq!(get("utm_campaign"), Some("codoseo_audit"));
    let pages: Vec<&str> = q
        .iter()
        .filter(|(k, _)| k == "page")
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(pages.len(), 10, "{pages:?}");
    assert_eq!(pages[0], "https://example.com/p0", "most inlinks first");
    assert_eq!(pages[9], "https://example.com/p9");
    assert!(!pages.contains(&"https://example.com/p10"));

    let (kind, payload): (String, serde_json::Value) =
        sqlx::query_as("SELECT kind, payload FROM events WHERE kind = 'rankorg_click'")
            .fetch_one(app.pool())
            .await
            .unwrap();
    assert_eq!(kind, "rankorg_click");
    assert_eq!(payload["src"], "audit");
}

#[tokio::test]
async fn the_audit_preview_links_through_the_counter_only_once_it_is_done() {
    let app = app().await;
    let crawl = crawl_id(&app.post("/audit", "url=example.com", None).await);
    let waiting = app.get(&format!("/audit/{crawl}"), None).await.body;
    assert!(!waiting.contains("/go/rankorg"), "nothing to look at yet");

    app.finalize_crawl(
        crawl,
        vec![page("example.com", "/")],
        Vec::new(),
        StopReason::Completed,
    )
    .await;
    let done = app.get(&format!("/audit/{crawl}"), None).await.body;
    assert!(
        done.contains(&format!("/go/rankorg?src=audit&#38;audit={crawl}")),
        "{done}"
    );
    assert!(
        !done.contains("rankorg.example"),
        "the real address only appears after the redirect"
    );
}

#[tokio::test]
async fn the_explorer_link_needs_the_sites_owner() {
    let app = app().await;
    let (ana, ana_cookie) = app.login("ana@example.com").await;
    let site = app.site(&ana, "example.com").await;
    let crawl = app
        .finished_crawl(
            &site,
            (0..3)
                .map(|i| page("example.com", &format!("/p{i}")))
                .collect(),
            Vec::new(),
        )
        .await;
    assert!(!crawl.is_nil());

    let explorer = app
        .get(&format!("/s/{}/explorer", site.id), Some(&ana_cookie))
        .await
        .body;
    assert!(
        explorer.contains(&format!("/go/rankorg?src=explorer&#38;site={}", site.id)),
        "{explorer}"
    );

    let res = app
        .get(
            &format!("/go/rankorg?src=explorer&site={}", site.id),
            Some(&ana_cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let q = params(res.location().unwrap());
    assert!(q.contains(&("utm_medium".into(), "explorer".into())));
    assert!(q.contains(&("domain".into(), "example.com".into())));
    assert_eq!(q.iter().filter(|(k, _)| k == "page").count(), 3);
    let (account, site_id): (Option<Uuid>, Option<Uuid>) =
        sqlx::query_as("SELECT account_id, site_id FROM events WHERE kind = 'rankorg_click'")
            .fetch_one(app.pool())
            .await
            .unwrap();
    assert_eq!((account, site_id), (Some(ana.id), Some(site.id)));

    // Signed out: login. Someone else's site: 404.
    let res = app
        .get(&format!("/go/rankorg?src=explorer&site={}", site.id), None)
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(res.location().unwrap().starts_with("/login"));
    let (_, bo_cookie) = app.login("bo@example.com").await;
    let res = app
        .get(
            &format!("/go/rankorg?src=explorer&site={}", site.id),
            Some(&bo_cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn bad_or_unknown_sources_are_a_404_and_self_hosted_has_no_rankorg_links() {
    let app = app().await;
    for path in [
        "/go/rankorg",
        "/go/rankorg?src=nope",
        "/go/rankorg?src=audit",
        "/go/rankorg?src=audit&audit=not-a-uuid",
        &format!("/go/rankorg?src=audit&audit={}", Uuid::new_v4()),
    ] {
        assert_eq!(
            app.get(path, None).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert_eq!(
        app.get("/go/rankorg?src=audit", None).await.status,
        StatusCode::NOT_FOUND
    );

    let selfhost = TestApp::new().await;
    let (owner, cookie) = selfhost.login("owner@example.com").await;
    let site = selfhost.site(&owner, "example.com").await;
    selfhost
        .finished_crawl(&site, vec![page("example.com", "/")], Vec::new())
        .await;
    let explorer = selfhost
        .get(&format!("/s/{}/explorer", site.id), Some(&cookie))
        .await
        .body;
    assert!(
        !explorer.contains("/go/rankorg"),
        "no marketing links in self-hosted"
    );
    let res = selfhost
        .get(
            &format!("/go/rankorg?src=explorer&site={}", site.id),
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}
