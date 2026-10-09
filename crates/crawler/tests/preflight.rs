use std::time::Duration;

use codoseo_core::Url;
use codoseo_core::crawl::{AddressPolicy, CrawlConfig, CrawlLimits, Politeness, USER_AGENT};
use codoseo_core::output::StopReason;
use codoseo_crawler::crawl::{CrawlError, preflight_for_tests};
use codoseo_crawler::preflight::{BLOCKED_MSG, LOGIN_MSG};
use codoseo_testkit::{Page, SiteBuilder};

fn config(start: Url, policy: AddressPolicy) -> CrawlConfig {
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
        address_policy: policy,
        user_agent: USER_AGENT.to_owned(),
    }
}

fn cfg(start: Url) -> CrawlConfig {
    config(start, AddressPolicy::AllowPrivate)
}

async fn preflight(start: Url) -> codoseo_crawler::preflight::Preflight {
    preflight_for_tests(cfg(start)).await.unwrap()
}

async fn preflight_public(start: Url) -> Result<codoseo_crawler::preflight::Preflight, CrawlError> {
    preflight_for_tests(config(start, AddressPolicy::Public)).await
}

fn ok_status(p: &codoseo_crawler::preflight::Preflight) -> u16 {
    p.start
        .as_ref()
        .and_then(|(_, r)| r.as_ref().ok())
        .map(|r| r.status)
        .expect("the start page was fetched")
}

#[tokio::test]
async fn settles_a_redirecting_start_address() {
    let site = SiteBuilder::new()
        .page("/start", Page::redirect(301, "/home"))
        .html("/home", "Home", &[])
        .start()
        .await;
    let p = preflight(site.url("/start")).await;
    assert_eq!(p.origin, site.url("/"));
    assert!(p.stop.is_none());
    let (requested, result) = p.start.as_ref().unwrap();
    assert_eq!(requested, &site.url("/start"));
    let result = result.as_ref().unwrap();
    assert_eq!(result.final_url, site.url("/home"));
    assert_eq!(result.chain.len(), 1);
    assert_eq!(result.status, 200);
}

#[tokio::test]
async fn a_redirect_to_another_origin_uses_that_origins_robots() {
    let target = SiteBuilder::new()
        .robots(200, "User-agent: *\nCrawl-delay: 3\nSitemap: /map.xml\n")
        .html("/home", "Home", &[])
        .start()
        .await;
    let source = SiteBuilder::new()
        .robots(200, "User-agent: *\nDisallow:\n")
        .page("/start", Page::redirect(301, target.url("/home").as_str()))
        .start()
        .await;
    let p = preflight(source.url("/start")).await;
    assert_eq!(p.origin, target.url("/"));
    assert!(p.stop.is_none());
    assert_eq!(p.rules.crawl_delay(), Some(Duration::from_secs(3)));
    assert_eq!(target.path_hits("/robots.txt"), 1);
    assert_eq!(p.robots.as_ref().unwrap().status, 200);
}

#[tokio::test]
async fn the_final_origins_robots_can_block_the_crawl() {
    let target = SiteBuilder::new()
        .robots(200, "User-agent: *\nDisallow: /")
        .html("/home", "Home", &[])
        .start()
        .await;
    let source = SiteBuilder::new()
        .page("/start", Page::redirect(301, target.url("/home").as_str()))
        .start()
        .await;
    let p = preflight(source.url("/start")).await;
    assert_eq!(p.stop, Some(StopReason::RobotsBlocked));
    assert!(
        p.start.is_some(),
        "the start page was fetched before the move"
    );
    assert_eq!(p.origin, target.url("/"));
}

#[tokio::test]
async fn a_full_robots_block_stops_before_fetching_the_start_page() {
    let site = SiteBuilder::new()
        .robots(200, "User-agent: *\nDisallow: /")
        .html("/", "Home", &[])
        .start()
        .await;
    let p = preflight(site.url("/")).await;
    assert_eq!(p.stop, Some(StopReason::RobotsBlocked));
    assert!(p.start.is_none());
    assert_eq!(site.path_hits("/"), 0);
    assert_eq!(p.robots.as_ref().unwrap().status, 200);
}

#[tokio::test]
async fn a_start_path_that_robots_disallows_is_blocked() {
    let site = SiteBuilder::new()
        .robots(200, "User-agent: *\nDisallow: /private")
        .html("/private/x", "Private", &[])
        .start()
        .await;
    let p = preflight(site.url("/private/x")).await;
    assert_eq!(p.stop, Some(StopReason::RobotsBlocked));
    assert!(p.start.is_none());
    assert_eq!(site.path_hits("/private/x"), 0);
}

#[tokio::test]
async fn robots_429_means_blocked() {
    let site = SiteBuilder::new()
        .robots(429, "slow down")
        .html("/", "Home", &[])
        .start()
        .await;
    let p = preflight(site.url("/")).await;
    // Named, so a failed crawl's reason tells it from a site that blocks every page.
    assert_eq!(
        p.stop,
        Some(StopReason::Blocked("robots.txt returned HTTP 429".into()))
    );
    assert!(p.start.is_none());
}

#[tokio::test]
async fn robots_503_means_unreachable() {
    let site = SiteBuilder::new()
        .robots(503, "down")
        .html("/", "Home", &[])
        .start()
        .await;
    let p = preflight(site.url("/")).await;
    let Some(StopReason::Unreachable(msg)) = &p.stop else {
        panic!("expected Unreachable, got {:?}", p.stop);
    };
    assert!(msg.contains("robots.txt returned HTTP 503"), "{msg}");
    assert!(p.start.is_none());
}

#[tokio::test]
async fn a_challenge_site_is_blocked_within_ten_requests() {
    let site = SiteBuilder::new()
        .every_path(Page::status(403, "<title>Just a moment...</title>"))
        .start()
        .await;
    let p = preflight(site.url("/")).await;
    assert_eq!(p.stop, Some(StopReason::Blocked(BLOCKED_MSG.into())));
    assert!(site.hits() <= 10, "{} requests", site.hits());
    assert_eq!(ok_status(&p), 403);
}

#[tokio::test]
async fn a_login_wall_is_reported_as_a_login() {
    let site = SiteBuilder::new()
        .every_path(Page::status(401, "<title>Sign in</title>"))
        .start()
        .await;
    let p = preflight(site.url("/")).await;
    assert_eq!(p.stop, Some(StopReason::Blocked(LOGIN_MSG.into())));
}

#[tokio::test]
async fn a_start_page_that_recovers_after_a_503_is_fine() {
    let site = SiteBuilder::new()
        .page(
            "/",
            Page::sequence(vec![
                Page::status(503, "busy").header("Retry-After", "0"),
                Page::html("<title>Home</title>"),
            ]),
        )
        .start()
        .await;
    let p = preflight(site.url("/")).await;
    assert!(p.stop.is_none(), "{:?}", p.stop);
    assert_eq!(ok_status(&p), 200);
    assert_eq!(site.path_hits("/"), 2);
}

#[tokio::test]
async fn a_start_page_that_stays_at_503_is_blocked_after_one_retry() {
    let site = SiteBuilder::new()
        .page("/", Page::status(503, "busy").header("Retry-After", "0"))
        .start()
        .await;
    let p = preflight(site.url("/")).await;
    assert_eq!(p.stop, Some(StopReason::Blocked(BLOCKED_MSG.into())));
    assert_eq!(site.path_hits("/"), 2);
}

#[tokio::test]
async fn a_closed_port_is_unreachable() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let p = preflight(Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap()).await;
    assert!(
        matches!(p.stop, Some(StopReason::Unreachable(_))),
        "{:?}",
        p.stop
    );
    assert!(p.robots.is_none());
    let (_, result) = p.start.as_ref().unwrap();
    assert!(result.is_err());
}

#[tokio::test]
async fn a_robots_timeout_means_no_rules_and_the_start_page_decides() {
    let site = SiteBuilder::new()
        .page("/robots.txt", Page::slow(Duration::from_secs(5)))
        .html("/", "Home", &[])
        .start()
        .await;
    let mut config = cfg(site.url("/"));
    config.limits.request_timeout = Duration::from_millis(500);
    let p = preflight_for_tests(config).await.unwrap();
    assert!(p.robots.is_none());
    assert!(p.rules.allowed("/anything"));
    assert!(p.stop.is_none());
    assert_eq!(ok_status(&p), 200);
}

#[tokio::test]
async fn sitemaps_come_from_robots_and_the_default_location() {
    let site = SiteBuilder::new()
        .robots(200, "User-agent: *\nSitemap: /extra.xml\n")
        .html("/", "Home", &[])
        .page(
            "/extra.xml",
            Page::status(
                200,
                "<urlset><url><loc>/a</loc></url><url><loc>/b</loc></url></urlset>",
            )
            .header("content-type", "application/xml"),
        )
        .sitemap(&["/", "/only-in-sitemap"])
        .start()
        .await;
    let p = preflight(site.url("/")).await;
    assert_eq!(p.sitemap.url_count, 4);
    assert_eq!(p.sitemap.files.len(), 2);
    assert_eq!(p.sitemap_urls.len(), 4);
    assert!(p.sitemap_urls.contains(&site.url("/only-in-sitemap")));
    assert!(p.sitemap_urls.contains(&site.url("/a")));
}

#[tokio::test]
async fn the_sitemap_is_only_read_for_a_crawlable_site() {
    let site = SiteBuilder::new()
        .every_path(Page::status(403, "no"))
        .sitemap(&["/a"])
        .start()
        .await;
    let p = preflight(site.url("/")).await;
    assert!(p.stop.is_some());
    assert_eq!(p.sitemap_urls.len(), 0);
    assert_eq!(site.path_hits("/sitemap.xml"), 0);
}

#[tokio::test]
async fn the_public_policy_refuses_a_loopback_start_address() {
    let site = SiteBuilder::new().html("/", "Home", &[]).start().await;
    let res = preflight_public(site.url("/")).await;
    assert!(matches!(res, Err(CrawlError::AddressBlocked(_))));
    assert_eq!(site.hits(), 0);
}
