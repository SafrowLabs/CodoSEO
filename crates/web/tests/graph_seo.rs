//! The site graph's server-drawn SEO views (sitemap, depth, broken & redirects, changes) over
//! crawls written through the real checks and `finalize`, with the links, redirects and sitemap
//! files each view reads, so every number on the page is one the worker would produce.

mod support;

use axum::http::StatusCode;
use codoseo_core::crawl::SitemapSummary;
use codoseo_core::output::{CrawlOutput, Edge, LinkGraph, SiteSignals, StopReason};
use codoseo_core::page::{Indexability, PageRecord};
use codoseo_core::sitemap::{LastmodStats, SitemapFile, SitemapKind};
use codoseo_store::sites::Site;
use support::{TestApp, page};
use url::Url;
use uuid::Uuid;

const DOMAIN: &str = "example.com";

fn url(path: &str) -> Url {
    Url::parse(&format!("https://{DOMAIN}{path}")).unwrap()
}

fn at(path: &str, depth: Option<u16>) -> PageRecord {
    PageRecord {
        depth,
        ..page(DOMAIN, path)
    }
}

fn broken(path: &str, depth: Option<u16>) -> PageRecord {
    PageRecord {
        status: 404,
        indexability: Indexability::ClientError,
        ..at(path, depth)
    }
}

fn redirect(path: &str, depth: Option<u16>, hops: &[(u16, &str)], to: &str) -> PageRecord {
    PageRecord {
        status: hops[0].0,
        indexability: Indexability::Redirected,
        redirect_chain: hops.iter().map(|(s, p)| (*s, url(p))).collect(),
        redirect_target: Some(url(to)),
        ..at(path, depth)
    }
}

/// Writes a finished crawl as the worker would, with these links (`from`, `to`, anchor) and
/// sitemap files instead of the harness's links from the first page to every other.
async fn crawl(
    app: &TestApp,
    site: &Site,
    pages: Vec<PageRecord>,
    links: &[(u32, u32, &str)],
    sitemap_files: Vec<SitemapFile>,
) -> Uuid {
    let crawl_id: Uuid = sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, worker_id, started_at, \
         heartbeat_at) VALUES ($1, $2, 'manual', 2, 'running', 'test-worker', \
         now() - interval '95 seconds', now()) RETURNING id",
    )
    .bind(site.id)
    .bind(&site.domain)
    .fetch_one(app.pool())
    .await
    .expect("insert crawl");
    let listed = pages.iter().filter(|p| p.in_sitemap).count() as u32;
    let mut anchors: Vec<String> = Vec::new();
    let edges = links
        .iter()
        .map(|&(from, to, text)| {
            let anchor = match anchors.iter().position(|a| a == text) {
                Some(i) => i as u32,
                None => {
                    anchors.push(text.to_owned());
                    anchors.len() as u32 - 1
                }
            };
            Edge {
                from,
                to,
                anchor,
                nofollow: false,
            }
        })
        .collect();
    let mut out = CrawlOutput {
        origin: url("/"),
        pages,
        links: LinkGraph { edges, anchors },
        robots: None,
        sitemap: SitemapSummary {
            url_count: listed,
            complete: true,
            ..SitemapSummary::default()
        },
        sitemap_files,
        stop: StopReason::Completed,
        duration_ms: 95_000,
        signals: SiteSignals::default(),
    };
    let report = codoseo_checks::run_checks(&mut out);
    codoseo_store::finalize::finalize(
        app.pool(),
        crawl_id,
        site.id,
        "test-worker",
        &out,
        &report,
        &[],
        None,
    )
    .await
    .expect("finalize");
    crawl_id
}

fn sitemap_file(
    path: &str,
    kind: SitemapKind,
    parent: Option<u32>,
    urls: &[&str],
    lastmod: LastmodStats,
) -> SitemapFile {
    SitemapFile {
        url: url(path),
        kind: Some(kind),
        parent,
        failed: false,
        listed: urls.len() as u32,
        lastmod,
        urls: urls.iter().map(|p| url(p).to_string()).collect(),
    }
}

/// A docs site: a chain of pages down to 5 clicks, a 404 four pages still link to with a page
/// of a similar name beside it, a 301 then 302 chain, a redirect to a 404, two orphan landing
/// pages, a noindex page and a page left out of the sitemap.
async fn docs_site(app: &TestApp, site: &Site) -> Uuid {
    let pages = vec![
        at("/", Some(0)),                            // 0
        at("/docs/", Some(1)),                       // 1
        at("/docs/api/", Some(2)),                   // 2
        at("/docs/api/auth/", Some(3)),              // 3
        at("/docs/api/auth/tokens", Some(4)),        // 4
        at("/docs/api/auth/tokens/rotate", Some(5)), // 5
        broken("/docs/api/rate-limits", Some(3)),    // 6
        at("/docs/api/limits", Some(3)),             // 7
        redirect(
            "/old-pricing",
            Some(1),
            &[(301, "/old-pricing"), (302, "/plans")],
            "/pricing",
        ), // 8
        at("/pricing", Some(2)),                     // 9
        PageRecord {
            in_sitemap: false,
            ..redirect("/legacy", Some(1), &[(301, "/legacy")], "/missing")
        }, // 10
        PageRecord {
            in_sitemap: false,
            ..broken("/missing", Some(2))
        }, // 11
        at("/lp/a", None),                           // 12
        at("/lp/b", None),                           // 13
        PageRecord {
            indexability: Indexability::Noindex,
            ..at("/hidden", Some(1))
        }, // 14
        PageRecord {
            in_sitemap: false,
            ..at("/about", Some(1))
        }, // 15
    ];
    let links = [
        (0, 1, "Docs"),
        (1, 2, "API"),
        (2, 3, "Auth"),
        (3, 4, "Tokens"),
        (4, 5, "Rotate"),
        (2, 6, "rate limits"),
        (3, 6, "limits"),
        (1, 6, "Rate limits"),
        (2, 7, "Limits"),
        (0, 8, "Pricing"),
        (1, 8, "Plans"),
        (0, 10, "Legacy"),
        (0, 14, "Hidden"),
        (0, 15, "About"),
        (5, 6, "see limits"),
    ];
    let varied = LastmodStats {
        with: 4,
        distinct: 4,
        min: Some("2026-01-02".to_owned()),
        max: Some("2026-09-01".to_owned()),
        future: 0,
    };
    let same = LastmodStats {
        with: 3,
        distinct: 1,
        min: Some("2026-03-03".to_owned()),
        max: Some("2026-03-03".to_owned()),
        future: 0,
    };
    let files = vec![
        sitemap_file(
            "/sitemap_index.xml",
            SitemapKind::Index,
            None,
            &[],
            LastmodStats::default(),
        ),
        sitemap_file(
            "/docs.xml",
            SitemapKind::UrlSet,
            Some(0),
            &["/", "/docs/", "/docs/api/", "/docs/api/rate-limits"],
            varied,
        ),
        sitemap_file(
            "/landing.xml",
            SitemapKind::UrlSet,
            Some(0),
            &["/lp/a", "/lp/b", "/old-pricing"],
            same,
        ),
    ];
    crawl(app, site, pages, &links, files).await
}

/// The page from the site graph's frame on, without the app's shell around it.
fn graph_part(body: &str) -> &str {
    body.split_once("graph-page").map_or(body, |(_, rest)| rest)
}

async fn tab(app: &TestApp, site: &Site, cookie: &str, name: &str) -> String {
    let res = app
        .get(&format!("/s/{}/graph/{name}", site.id), Some(cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{name}");
    res.body
}

#[tokio::test]
async fn the_sitemap_tab_draws_the_tree_the_overlap_and_what_to_fix() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    docs_site(&app, &site).await;
    let body = tab(&app, &site, &cookie, "sitemap").await;

    // The index card and its two sitemaps, one stamping the same lastmod on every URL.
    assert!(body.contains("sitemap_index.xml"), "{body}");
    assert!(body.contains("2 sitemaps · 7 URLs"));
    assert!(body.contains(">docs.xml<") && body.contains(">landing.xml<"));
    assert!(body.contains("lastmod: varied"));
    assert!(body.contains("lastmod: all Mar 3 2026"));
    assert_eq!(body.matches(r#"<path d="M0 44C28"#).count(), 2);
    // landing.xml: 2 orphans and a redirect.
    assert!(body.contains(r#"title="2 orphans""#));
    assert!(body.contains(r#"title="1 redirect""#));

    // 13 crawled pages in the sitemap, 14 reached by links, 11 in both.
    assert!(body.contains("In sitemap · 13"));
    assert!(body.contains("Found by links · 14"));
    assert!(body.contains(">11</text>"));
    assert!(body.contains("3 not indexable"));
    assert!(body.contains("1 indexable"));

    assert!(body.contains("Non-indexable URLs in the sitemap"));
    assert!(body.contains("1 redirect, 1 noindex, 1 error. Remove them."));
    assert!(body.contains("All are under /lp. Link the ones you keep."));
    assert!(body.contains(&format!(
        r#"href="/s/{}/explorer?filter=check:orphan""#,
        site.id
    )));
    assert!(body.contains(&format!(
        r#"href="/s/{}/explorer?filter=check:not_in_sitemap""#,
        site.id
    )));
    assert!(body.contains("landing.xml stamps Mar 3 2026 on all 3 URLs, so engines ignore it."));
    assert!(!graph_part(&body).contains('—'));
}

#[tokio::test]
async fn the_depth_tab_counts_clicks_and_suggests_hubs() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    docs_site(&app, &site).await;
    let body = tab(&app, &site, &cookie, "depth").await;

    // 14 linked pages, 29 clicks between them; 12 within 3 clicks.
    assert!(body.contains(">2.1<"), "{body}");
    assert!(body.contains(">85.7%<"));
    assert!(body.contains(">2 pages<"));
    assert!(body.contains(">5 clicks<"));
    assert!(body.contains(r#"aria-label="1 click: 5 pages""#));
    assert!(body.contains(r#"aria-label="3 clicks: 3 pages""#));
    assert!(body.contains(r#"aria-label="5 or more clicks: 1 page""#));
    assert!(body.contains(r#"aria-label="Not linked: 2 pages""#));

    // /docs holds 7 pages, two of them too deep; the top level 6.
    assert!(body.contains(r#"title="/docs, 3 clicks: 3 pages""#));
    assert!(body.contains(r#"title="/docs, 4 clicks: 1 page""#));
    assert!(body.contains(r#"title="Top level, 1 click: 4 pages""#));
    assert!(body.contains("is-deep"));

    // Both deep pages hang under /docs/api/ in the crawl tree: one link from it puts them at 3.
    assert!(body.contains(">/docs/api/auth/tokens<"));
    assert!(body.contains(">/docs/api/auth/tokens/rotate<"));
    assert_eq!(body.matches(">/docs/api/</span>").count(), 2);
    assert_eq!(body.matches("→ 3 clicks").count(), 2);
    assert!(body.contains(&format!(
        r#"href="/s/{}/explorer?filter=check:deep_page""#,
        site.id
    )));
}

#[tokio::test]
async fn the_links_tab_fans_in_the_most_linked_404_and_lists_chains() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    docs_site(&app, &site).await;
    let body = tab(&app, &site, &cookie, "links").await;

    // 2 broken pages, linked from 4 pages; 3 links to 2 redirects.
    assert!(body.contains("linked from 4 pages"), "{body}");
    assert!(body.contains("across 2 redirects"));
    assert!(body.contains(">2 hops<"));

    // The fan: four sources with their anchors into the 404, and the page beside it to
    // redirect to.
    assert!(body.contains(r#"title="/docs/api/rate-limits">/docs/api/rate-limits</b>"#));
    assert!(body.contains("Linked from 4 pages"));
    for anchor in ["Rate limits", "rate limits", "limits", "see limits"] {
        assert!(
            body.contains(&format!("&#34;{anchor}&#34;"))
                || body.contains(&format!("&quot;{anchor}&quot;")),
            "{anchor}"
        );
    }
    assert!(body.contains("Suggested fix"));
    assert!(body.contains(r#"Redirect it to <span class="mono">/docs/api/limits</span>"#));
    // The other broken page, linked from nowhere.
    assert!(body.contains(">/missing<"));
    assert!(body.contains("linked from 0 pages"));

    // /old-pricing: 301 then 302, landing on a 200, linked from 2 pages.
    assert!(body.contains(">302 temporary<"));
    assert!(body.contains("Temporary redirect"));
    assert!(body.contains(">2 links<"));
    // /legacy lands on the 404.
    assert!(body.contains("Ends broken"));
    assert!(body.contains("glk-chain is-broken"));
}

#[tokio::test]
async fn the_changes_tab_draws_the_folders_that_changed() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    let fan = |n: u32| (1..n).map(|to| (0, to, "Nav")).collect::<Vec<_>>();

    let before = vec![
        at("/", Some(0)),
        at("/blog/", Some(1)),
        at("/blog/a", Some(2)),
        at("/blog/b", Some(3)),
        at("/docs/x", Some(1)),
        at("/old", Some(1)),
        at("/zoho", Some(1)),
        broken("/fixed", Some(1)),
    ];
    crawl(&app, &site, before, &fan(8), Vec::new()).await;

    let one = tab(&app, &site, &cookie, "changes").await;
    assert!(one.contains("Changes appear after your second crawl."));

    let after = vec![
        at("/", Some(0)),
        at("/blog/", Some(1)),
        at("/blog/a", Some(2)),
        at("/blog/b", Some(5)),
        broken("/docs/x", Some(1)),
        PageRecord {
            indexability: Indexability::Noindex,
            ..at("/zoho", Some(1))
        },
        at("/fixed", Some(1)),
        at("/blog/new", Some(2)),
        at("/lp/promo", None),
    ];
    crawl(&app, &site, after, &fan(8), Vec::new()).await;
    let body = tab(&app, &site, &cookie, "changes").await;

    assert!(body.contains("From #1 · "), "{body}");
    assert!(body.contains("To #2 · "));
    for chip in [
        "2 new",
        "1 removed",
        "1 moved deeper",
        "1 broke",
        "1 new orphan",
        "1 now noindex",
    ] {
        assert!(body.contains(&format!(">{chip}<")), "{chip}");
    }
    // Only the folders that changed, each changed page badged.
    assert!(body.contains(">/blog<") && body.contains(">/docs<") && body.contains(">/lp<"));
    assert!(body.contains(">+1 · 1 deeper<"));
    assert!(body.contains(">3 → 5 clicks<"));
    assert!(body.contains(">200 → 404<"));
    assert!(body.contains(">new, not linked<"));
    assert!(body.contains(">removed<"));
    assert!(body.contains(">now noindex<"));
    assert!(body.contains(">6 changes<"));
    assert!(!body.contains(">/blog/a<"));

    // The side panel: the trend, the page that broke and the one that was fixed.
    assert!(body.contains("since #1") || body.contains("same as #1"));
    assert!(body.contains("<polyline points="));
    assert!(body.contains(" started returning 404. 1 page still links to it."));
    assert!(body.contains(&format!(r#"href="/s/{}/graph/links""#, site.id)));
    assert!(body.contains("Broken pages fixed"));
    assert!(!graph_part(&body).contains('—'));
}

#[tokio::test]
async fn crawls_from_before_the_graph_say_to_run_a_new_one() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    let crawl_id = docs_site(&app, &site).await;
    sqlx::query("UPDATE site_files SET sitemap_tree = NULL WHERE crawl_id = $1")
        .bind(crawl_id)
        .execute(app.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM crawl_graphs WHERE crawl_id = $1")
        .bind(crawl_id)
        .execute(app.pool())
        .await
        .unwrap();

    let sitemap = tab(&app, &site, &cookie, "sitemap").await;
    assert!(sitemap.contains("Run a new crawl to see this."));
    // The overlap comes from the pages, which every crawl has.
    assert!(sitemap.contains("In sitemap · 13"));
    let depth = tab(&app, &site, &cookie, "depth").await;
    assert!(depth.contains("Run a new crawl to see this."));
    assert!(depth.contains(">5 clicks<"));
}

#[tokio::test]
async fn another_account_gets_a_404() {
    let app = TestApp::new().await;
    let (account, _) = app.login("a@x.com").await;
    let (_, other) = app.login("b@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    docs_site(&app, &site).await;
    for name in ["sitemap", "depth", "links", "changes"] {
        let res = app
            .get(&format!("/s/{}/graph/{name}", site.id), Some(&other))
            .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{name}");
    }
}
