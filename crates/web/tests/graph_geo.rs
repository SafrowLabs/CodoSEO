//! The site graph's GEO tabs: llms.txt as a tree with its checks, the top pages it leaves out
//! and a suggested edit (or a starter file when there is none), and AI reach from robots.txt and
//! the fetch checks through each crawler to the answer engines.

mod support;

use axum::http::StatusCode;
use codoseo_core::output::{LinkStatus, LlmsTxtFetch};
use support::{TestApp, page, store_probes};

const DOMAIN: &str = "example.com";
const OPEN: &str = "User-agent: *\nAllow: /\n";
/// GPTBot (training) and PerplexityBot (search) kept out, everyone else let in.
const ROBOTS: &str = "User-agent: GPTBot\nDisallow: /\n\nUser-agent: PerplexityBot\nDisallow: /\n\nUser-agent: *\nAllow: /\n";
const FILE: &str = "# Example\n\n> A site about examples.\n\n## Docs\n\n- [Start](/docs/start): how to begin\n- [Gone](/docs/gone)\n\n## Optional\n\n- [Blog](/blog)\n";

fn llms(status: u16, body: &str, links: &[(&str, u16)]) -> LlmsTxtFetch {
    LlmsTxtFetch {
        status,
        content_type: Some("text/plain".to_owned()),
        bytes: body.len() as u64,
        body: body.to_owned(),
        final_url: Some(format!("https://{DOMAIN}/llms.txt")),
        links: links
            .iter()
            .map(|(path, status)| LinkStatus {
                url: format!("https://{DOMAIN}{path}"),
                status: Some(*status),
                from_crawl: false,
            })
            .collect(),
        ..Default::default()
    }
}

fn pages() -> Vec<codoseo_core::page::PageRecord> {
    vec![
        page(DOMAIN, "/"),
        page(DOMAIN, "/docs/start"),
        page(DOMAIN, "/blog"),
        page(DOMAIN, "/pricing"),
    ]
}

async fn body(app: &TestApp, site: uuid::Uuid, tab: &str, cookie: &str) -> String {
    let res = app
        .get(&format!("/s/{site}/graph/{tab}"), Some(cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{tab}");
    res.body
}

#[tokio::test]
async fn llms_tab_draws_the_file_with_its_checks_and_the_pages_it_leaves_out() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    let file = llms(
        200,
        FILE,
        &[("/docs/start", 200), ("/docs/gone", 404), ("/blog", 200)],
    );
    app.finished_crawl_with_llms(&site, pages(), (200, OPEN), file, false)
        .await;
    let report = codoseo_store::geo::latest_llms_txt(app.pool(), site.id)
        .await
        .unwrap()
        .expect("llms.txt report");
    let passed = report.checks.iter().filter(|c| c.ok).count();
    let b = body(&app, site.id, "llms", &cookie).await;

    assert!(b.contains("The map you hand to AI agents"));
    // The root card, both sections (Optional dashed) and every link with its status.
    assert!(b.contains(">Example</text>"));
    assert!(b.contains("&#62; A site about examples."));
    assert!(b.contains(">Docs</text>"));
    assert!(b.contains(r#"class="gl-section is-optional""#));
    assert!(b.contains(r#"class="gl-link is-ok""#));
    assert!(b.contains(r#"class="gl-link is-broken""#));
    assert!(b.contains(">404</text>"));
    assert!(b.contains(">/docs/gone</text>"));
    assert!(b.contains("3 links in 2 sections · 1 with problems"));
    // The top page the file leaves out (the home page never counts).
    assert!(b.contains("Not listed"));
    assert!(b.contains(">/pricing</text>"));
    assert!(!b.contains(r#">/</text>"#));
    // The checks, counted from the report, and the edit that drops the broken link.
    assert!(b.contains(&format!("{passed} of {} pass", report.checks.len())));
    assert!(b.contains("Starts with an H1"));
    assert!(b.contains("- [Gone](/docs/gone)"));
    assert!(b.contains("+ [/pricing | A page title that is long enough](/pricing)"));
    assert!(b.contains("Copy suggested edit"));
    assert!(b.contains(r#"href="https://example.com/llms.txt""#));
}

#[tokio::test]
async fn llms_tab_offers_a_starter_file_when_there_is_none() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    app.finished_crawl_with_llms(&site, pages(), (200, OPEN), llms(404, "", &[]), false)
        .await;
    let b = body(&app, site.id, "llms", &cookie).await;
    assert!(b.contains("No llms.txt yet"));
    assert!(b.contains("/llms.txt answered HTTP 404."));
    assert!(b.contains("Suggested starter file"));
    assert!(b.contains("# / | A page title that is long enough"));
    assert!(b.contains("- [/docs/start | A page title that is long enough](/docs/start)"));
    assert!(b.contains("Copy starter file"));
    assert!(!b.contains("gl-svg"));
}

#[tokio::test]
async fn llms_tab_asks_for_a_new_crawl_when_the_report_has_no_tree() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    app.finished_crawl_with_llms(&site, pages(), (200, OPEN), llms(200, FILE, &[]), false)
        .await;
    sqlx::query("UPDATE ai_reports SET report = report #- '{llms_txt,tree}' WHERE site_id = $1")
        .bind(site.id)
        .execute(app.pool())
        .await
        .unwrap();
    let b = body(&app, site.id, "llms", &cookie).await;
    assert!(b.contains("Run a new crawl to see the file as a tree."));
    assert!(!b.contains("gl-svg"));

    // A crawl that never fetched the file says so.
    let other = app.site(&account, "other.example").await;
    app.finished_crawl(&other, vec![page("other.example", "/")], vec![])
        .await;
    let b = body(&app, other.id, "llms", &cookie).await;
    assert!(b.contains("llms.txt not checked yet"));
}

#[tokio::test]
async fn reach_tab_draws_bots_and_engines_from_robots_and_the_fetch_checks() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    app.finished_crawl_with_robots(
        &site,
        vec![page(DOMAIN, "/"), page(DOMAIN, "/about")],
        (200, ROBOTS),
        vec![],
    )
    .await;
    store_probes(&app, &site).await;
    let b = body(&app, site.id, "reach", &cookie).await;

    assert!(b.contains("Which AI answers can read your site"));
    // The site card: the rules robots.txt has and the CDN the fetch checks met.
    assert!(b.contains("robots.txt · 3 rules"));
    assert!(b.contains("CDN · bot challenge on"));
    assert!(b.contains("Cloudflare challenges some AI bots."));
    // Every registry bot of the three groups, the blocked ones with why.
    for token in [
        "OAI-SearchBot",
        "PerplexityBot",
        "ChatGPT-User",
        "GPTBot",
        "CCBot",
    ] {
        assert!(b.contains(&format!(">{token}</text>")), "{token}");
    }
    assert!(b.contains(">CDN challenge</text>"));
    assert!(b.contains(">robots.txt</text>"));
    assert!(
        b.contains(r#"class="gr-bot is-off""#),
        "GPTBot is opted out"
    );
    // ChatGPT and Perplexity keep only their user fetchers; the other eight can read the site.
    assert!(b.contains("8 <span>of 10 can read you</span>"));
    assert!(b.contains("ChatGPT search is blocked"));
    assert!(b.contains("Perplexity search is blocked"));
    assert!(b.contains("user fetch only"));
    assert!(b.contains("robots.txt:4  User-agent: PerplexityBot"));
    assert!(b.contains("robots.txt:5  Disallow: /"));
    assert!(b.contains("published IP ranges"));
    // Training: GPTBot out, the rest in.
    assert!(b.contains("1 training crawler opted out"));
    assert!(b.contains("A choice, not a problem."));
    assert!(b.contains("7 of 8 in"));
    assert!(b.contains(&format!(r#"href="/s/{}/ai-access/robots""#, site.id)));
}

#[tokio::test]
async fn reach_tab_without_a_report_says_so() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    app.finished_crawl(&site, vec![page(DOMAIN, "/")], vec![])
        .await;
    let b = body(&app, site.id, "reach", &cookie).await;
    assert!(b.contains("No AI access report yet"));
    assert!(!b.contains("gr-svg"));
}

#[tokio::test]
async fn another_accounts_geo_tabs_are_a_404() {
    let app = TestApp::new().await;
    let (account, _) = app.login("a@x.com").await;
    let (_, other) = app.login("b@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    app.finished_crawl_with_llms(&site, pages(), (200, ROBOTS), llms(200, FILE, &[]), false)
        .await;
    for tab in ["llms", "reach"] {
        let res = app
            .get(&format!("/s/{}/graph/{tab}", site.id), Some(&other))
            .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{tab}");
    }
}
