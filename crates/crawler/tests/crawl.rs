//! End-to-end crawls against generated local sites.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use codoseo_core::Url;
use codoseo_core::crawl::{AddressPolicy, CrawlConfig, CrawlLimits, Politeness, USER_AGENT};
use codoseo_core::page::{Indexability, PageRecord};
use codoseo_crawler::crawl::{CrawlError, crawl, inspect_page};
use codoseo_crawler::preflight::BLOCKED_MSG;
use codoseo_crawler::{CrawlOutput, StopReason};
use codoseo_testkit::{Page, SiteBuilder, TestSite, html_page};

fn cfg(start: Url) -> CrawlConfig {
    CrawlConfig {
        start_url: start,
        limits: CrawlLimits {
            request_timeout: Duration::from_secs(2),
            max_duration: Duration::from_secs(30),
            ..CrawlLimits::default()
        },
        politeness: Politeness {
            requests_per_sec: 1000.0,
            ..Politeness::default()
        },
        address_policy: AddressPolicy::AllowPrivate,
        user_agent: USER_AGENT.to_owned(),
    }
}

fn cfg_pages(start: Url, max_pages: u32) -> CrawlConfig {
    let mut c = cfg(start);
    c.limits.max_pages = max_pages;
    c
}

fn cfg_time(start: Url, max_duration: Duration) -> CrawlConfig {
    let mut c = cfg(start);
    c.limits.max_duration = max_duration;
    c
}

fn cfg_timeout(start: Url, request_timeout: Duration) -> CrawlConfig {
    let mut c = cfg(start);
    c.limits.request_timeout = request_timeout;
    c
}

fn refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

fn try_page<'a>(out: &'a CrawlOutput, path: &str) -> Option<&'a PageRecord> {
    out.pages.iter().find(|p| p.url.path() == path)
}

fn page<'a>(out: &'a CrawlOutput, path: &str) -> &'a PageRecord {
    try_page(out, path).unwrap_or_else(|| panic!("no record for {path}"))
}

fn depth_of(out: &CrawlOutput, path: &str) -> Option<u16> {
    page(out, path).depth
}

/// Every edge joins two recorded pages and none is a self-link.
fn assert_edges_are_sound(out: &CrawlOutput) {
    let n = out.pages.len() as u32;
    for e in &out.links.edges {
        assert!(e.from < n && e.to < n, "{e:?} out of range");
        assert_ne!(e.from, e.to, "self-edge {e:?}");
        assert!((e.anchor as usize) < out.links.anchors.len());
    }
}

#[tokio::test]
async fn depth_is_clicks_from_home_and_sitemap_only_pages_have_none() {
    let site = SiteBuilder::new()
        .html("/", "Home", &["/a", "/b"])
        .html("/a", "A", &["/c"])
        .html("/b", "B", &["/a"])
        .html("/c", "C", &["/d"])
        .html("/d", "D", &[])
        .html("/e", "E", &[])
        .sitemap(&["/", "/e"])
        .start()
        .await;
    let out = crawl(cfg(site.url("/")), |_| {}).await.unwrap();
    assert_eq!(out.stop, StopReason::Completed);
    assert_eq!(out.pages.len(), 6);
    assert_eq!(depth_of(&out, "/"), Some(0));
    assert_eq!(depth_of(&out, "/a"), Some(1));
    assert_eq!(depth_of(&out, "/d"), Some(3));
    assert_eq!(depth_of(&out, "/e"), None);
    assert!(page(&out, "/e").in_sitemap && page(&out, "/").in_sitemap);
    assert!(!page(&out, "/a").in_sitemap);
    assert_edges_are_sound(&out);
}

#[tokio::test]
async fn the_level_barrier_keeps_depth_exact_with_parallel_fetches() {
    // /target is two clicks away through the slow page and three through the fast ones.
    // Without the barrier, /mid is fetched before /slow answers and claims /target at 3.
    let mut slow_page = Page::html(&html_page("Slow", &["/target"]));
    slow_page.delay = Some(Duration::from_millis(400));
    let site = SiteBuilder::new()
        .html("/", "Home", &["/slow", "/fast"])
        .page("/slow", slow_page)
        .html("/fast", "Fast", &["/mid"])
        .html("/mid", "Mid", &["/target"])
        .html("/target", "Target", &[])
        .start()
        .await;
    let out = crawl(cfg(site.url("/")), |_| {}).await.unwrap();
    assert_eq!(depth_of(&out, "/target"), Some(2));
    assert_eq!(depth_of(&out, "/mid"), Some(2));
}

#[tokio::test]
async fn link_graph_uses_page_indices_and_nofollow_is_recorded_not_followed() {
    let site = SiteBuilder::new()
        .page(
            "/",
            Page::html(r#"<a href="/x">X</a><a rel="nofollow" href="/nf">NF</a>"#),
        )
        .html("/x", "X", &["/"])
        .html("/nf", "NF", &[])
        .start()
        .await;
    let out = crawl(cfg(site.url("/")), |_| {}).await.unwrap();
    assert!(try_page(&out, "/nf").is_none());
    assert_eq!(site.path_hits("/nf"), 0);
    assert_eq!(page(&out, "/").outlinks_nofollow, 1);
    let e = out
        .links
        .edges
        .iter()
        .find(|e| out.pages[e.from as usize].url.path() == "/")
        .unwrap();
    assert_eq!(out.pages[e.to as usize].url.path(), "/x");
    assert_eq!(out.links.anchor(e), "X");
    assert!(!e.nofollow);
    // /x links back home.
    assert!(out.links.edges.iter().any(|e| {
        out.pages[e.from as usize].url.path() == "/x" && out.pages[e.to as usize].url.path() == "/"
    }));
    assert_edges_are_sound(&out);
}

#[tokio::test]
async fn robots_disallowed_paths_become_records_and_are_never_requested() {
    let site = SiteBuilder::new()
        .robots(200, "User-agent: *\nDisallow: /private")
        .html("/", "Home", &["/private/x", "/a"])
        .html("/a", "A", &[])
        .start()
        .await;
    let out = crawl(cfg(site.url("/")), |_| {}).await.unwrap();
    let p = page(&out, "/private/x");
    assert_eq!(
        (p.status, p.indexability),
        (0, Indexability::BlockedByRobots)
    );
    assert_eq!(p.depth, Some(1));
    assert_eq!(site.path_hits("/private/x"), 0);
    assert!(try_page(&out, "/a").is_some());
    assert_eq!(out.stop, StopReason::Completed);
}

#[tokio::test]
async fn a_redirecting_start_url_settles_the_origin() {
    let site = SiteBuilder::new()
        .page("/start", Page::redirect(301, "/home"))
        .html("/home", "Home", &["/start", "/a"])
        .html("/a", "A", &[])
        .start()
        .await;
    let out = crawl(cfg(site.url("/start")), |_| {}).await.unwrap();
    let start = page(&out, "/start");
    assert_eq!(start.indexability, Indexability::Redirected);
    assert_eq!(start.redirect_target, Some(site.url("/home")));
    assert_eq!(depth_of(&out, "/home"), Some(0));
    assert_eq!(depth_of(&out, "/a"), Some(1));
    assert_eq!(out.origin, site.url("/"));
    // /start was not fetched a second time, and the link back to it is internal.
    assert_eq!(site.path_hits("/start"), 1);
    assert!(out.links.edges.iter().any(|e| {
        out.pages[e.from as usize].url.path() == "/home"
            && out.pages[e.to as usize].url.path() == "/start"
    }));
    assert_eq!(out.stop, StopReason::Completed);
}

/// Pages of the navigation site: every page links to the three nav pages, and each
/// content page links to the next one.
fn nav_spec() -> Vec<(String, Vec<(String, String)>)> {
    let nav = [("/n1", "N1"), ("/n2", "N2"), ("/n3", "N3")];
    let content: Vec<String> = std::iter::once("/".to_owned())
        .chain((1..17).map(|i| format!("/p{i}")))
        .collect();
    let mut pages = Vec::new();
    for (i, path) in content.iter().enumerate() {
        let mut links: Vec<(String, String)> = nav
            .iter()
            .map(|(h, a)| ((*h).to_owned(), (*a).to_owned()))
            .collect();
        if let Some(next) = content.get(i + 1) {
            links.push((next.clone(), "Next".to_owned()));
        }
        pages.push((path.clone(), links));
    }
    for (path, _) in nav {
        let links = nav
            .iter()
            .map(|(h, a)| ((*h).to_owned(), (*a).to_owned()))
            .collect();
        pages.push((path.to_owned(), links));
    }
    pages
}

async fn nav_site(spec: &[(String, Vec<(String, String)>)]) -> TestSite {
    let mut b = SiteBuilder::new();
    for (path, links) in spec {
        let anchors: String = links
            .iter()
            .map(|(h, a)| format!(r#"<a href="{h}">{a}</a>"#))
            .collect();
        b = b.page(path, Page::html(&format!("<title>{path}</title>{anchors}")));
    }
    b.start().await
}

fn distinct_anchor_texts_in(spec: &[(String, Vec<(String, String)>)]) -> usize {
    spec.iter()
        .flat_map(|(_, links)| links.iter().map(|(_, a)| a.as_str()))
        .collect::<HashSet<_>>()
        .len()
}

#[tokio::test]
async fn repeated_navigation_anchors_are_interned() {
    let spec = nav_spec();
    assert_eq!(spec.len(), 20);
    assert_eq!(distinct_anchor_texts_in(&spec), 4); // N1, N2, N3, Next
    let site = nav_site(&spec).await;
    let out = crawl(cfg(site.url("/")), |_| {}).await.unwrap();
    assert_eq!(out.pages.len(), 20);
    assert_eq!(out.links.anchors.len(), distinct_anchor_texts_in(&spec));
    // 17 content pages × 3 nav links + 16 "Next" links + 3 nav pages × 2 (self-links dropped).
    assert_eq!(out.links.edges.len(), 17 * 3 + 16 + 3 * 2);
    assert_edges_are_sound(&out);
}

#[tokio::test]
async fn an_endless_calendar_stops_at_the_page_limit() {
    let site = SiteBuilder::new()
        .html("/", "Home", &["/list?page=1"])
        .endless(20)
        .start()
        .await;
    let out = crawl(cfg_pages(site.url("/"), 100), |_| {}).await.unwrap();
    assert_eq!(out.stop, StopReason::PageLimit);
    assert_eq!(out.pages.len(), 100);
    assert!(out.pages.iter().all(|p| p.status == 200));
    assert_edges_are_sound(&out);
}

#[tokio::test]
async fn the_time_limit_stops_the_crawl_and_keeps_pages() {
    // 50 pages of 300 ms over 2 connections would take about 7.5 s.
    let links: Vec<String> = (0..50).map(|i| format!("/s/{i}")).collect();
    let site = SiteBuilder::new()
        .html("/", "Home", &refs(&links))
        .every_path(Page::slow(Duration::from_millis(300)))
        .start()
        .await;
    let started = std::time::Instant::now();
    let out = crawl(cfg_time(site.url("/"), Duration::from_secs(1)), |_| {})
        .await
        .unwrap();
    assert_eq!(out.stop, StopReason::TimeLimit);
    assert!(!out.pages.is_empty());
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn the_deadline_also_bounds_preflight() {
    let site = SiteBuilder::new()
        .every_path(Page::slow(Duration::from_secs(3)))
        .start()
        .await;
    let started = std::time::Instant::now();
    let out = crawl(cfg_time(site.url("/"), Duration::from_millis(300)), |_| {})
        .await
        .unwrap();
    assert_eq!(out.stop, StopReason::TimeLimit);
    assert!(out.pages.is_empty());
    assert_eq!(out.origin, site.url("/"));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn consecutive_timeouts_make_the_site_unreachable() {
    let links: Vec<String> = (0..30).map(|i| format!("/slow/{i}")).collect();
    let site = SiteBuilder::new()
        .html("/", "Home", &refs(&links))
        .every_path(Page::slow(Duration::from_secs(2)))
        .start()
        .await;
    let out = crawl(
        cfg_timeout(site.url("/"), Duration::from_millis(200)),
        |_| {},
    )
    .await
    .unwrap();
    assert!(
        matches!(out.stop, StopReason::Unreachable(_)),
        "{:?}",
        out.stop
    );
    assert_eq!(page(&out, "/").status, 200);
    assert_eq!(out.pages.len(), 21); // home + 20 timeouts, the rest never recorded
}

#[tokio::test]
async fn ten_blocked_responses_stop_the_crawl() {
    let links: Vec<String> = (0..30).map(|i| format!("/p/{i}")).collect();
    let site_429 = SiteBuilder::new()
        .html("/", "Home", &refs(&links))
        .every_path(Page::status(429, ""))
        .start()
        .await;
    let out = crawl(cfg(site_429.url("/")), |_| {}).await.unwrap();
    assert_eq!(out.stop, StopReason::Blocked(BLOCKED_MSG.into()));
    assert!(site_429.hits() <= 30, "{} hits", site_429.hits());
}

#[tokio::test]
async fn a_503_with_retry_after_is_retried_once() {
    let site = SiteBuilder::new()
        .html("/", "Home", &["/busy"])
        .page(
            "/busy",
            Page::sequence(vec![
                Page::status(503, "").header("retry-after", "1"),
                Page::html("<title>Busy</title>"),
            ]),
        )
        .start()
        .await;
    let out = crawl(cfg(site.url("/")), |_| {}).await.unwrap();
    assert_eq!(page(&out, "/busy").status, 200);
    assert_eq!(site.path_hits("/busy"), 2);
    assert_eq!(out.pages.len(), 2);
    assert_eq!(out.stop, StopReason::Completed);
}

#[tokio::test]
async fn progress_fires_once_per_page_with_increasing_counts() {
    let site = SiteBuilder::new()
        .html("/", "Home", &["/a", "/b"])
        .html("/a", "A", &["/c"])
        .html("/b", "B", &[])
        .html("/c", "C", &[])
        .start()
        .await;
    let seen = Arc::new(Mutex::new(vec![]));
    let s2 = seen.clone();
    let out = crawl(cfg(site.url("/")), move |p| {
        s2.lock().unwrap().push(p.pages_done)
    })
    .await
    .unwrap();
    let seen = seen.lock().unwrap();
    assert!(seen.windows(2).all(|w| w[1] > w[0]), "{seen:?}");
    assert_eq!(seen.last().copied(), Some(out.pages.len() as u32));
}

#[tokio::test]
async fn inspect_page_follows_the_chain() {
    let site = SiteBuilder::new()
        .page("/start", Page::redirect(301, "/home"))
        .html("/home", "Home", &["/a"])
        .start()
        .await;
    let rec = inspect_page(&cfg(site.url("/start"))).await.unwrap();
    assert_eq!(rec.redirect_chain.len(), 1);
    assert_eq!(rec.status, 200);
    assert_eq!(rec.url, site.url("/home"));
    assert_eq!(rec.redirect_target, Some(site.url("/home")));
    assert_eq!(rec.fields.title.as_deref(), Some("Home"));
    assert_eq!(rec.outlinks_internal, 1);
    assert_eq!(site.path_hits("/robots.txt"), 0);
}

#[tokio::test]
async fn invalid_start_urls_are_errors() {
    let bad = cfg(Url::parse("ftp://127.0.0.1/").unwrap());
    assert!(matches!(
        crawl(bad.clone(), |_| {}).await,
        Err(CrawlError::InvalidStart(_))
    ));
    assert!(matches!(
        inspect_page(&bad).await,
        Err(CrawlError::InvalidStart(_))
    ));
}

#[test]
fn the_crawl_future_is_send() {
    fn assert_send<T: Send>(_: T) {}
    let c = cfg(Url::parse("http://127.0.0.1:1/").unwrap());
    assert_send(crawl(c, |_| {}));
}
