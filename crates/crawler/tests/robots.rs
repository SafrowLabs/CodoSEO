mod support;

use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use codoseo_core::crawl::AddressPolicy;
use codoseo_crawler::fetch::{Fetcher, FetcherConfig};
use codoseo_crawler::robots::{RobotsRules, fetch_robots};
use support::server::TestServer;

const AGENT: &str = "CodoSEObot";

#[test]
fn longest_match_wins_and_allow_wins_ties() {
    let r = RobotsRules::parse(
        b"User-agent: *\nDisallow: /private\nAllow: /private/ok\nDisallow: /*.pdf$\n",
        AGENT,
    );
    assert!(!r.allowed("/private/x"));
    assert!(r.allowed("/private/ok"));
    assert!(!r.allowed("/a.pdf"));
    assert!(r.allowed("/a.pdf?x"));
    assert!(r.allowed("https://northwind.test/public"));
    assert!(!r.blocks_everything());
}

#[test]
fn crawl_delay_is_capped_at_10_seconds() {
    let slow = RobotsRules::parse(b"User-agent: *\nCrawl-delay: 30\n", AGENT);
    assert_eq!(slow.crawl_delay(), Some(Duration::from_secs(10)));
    let fast = RobotsRules::parse(b"User-agent: *\nCrawl-delay: 0.5\n", AGENT);
    assert_eq!(fast.crawl_delay(), Some(Duration::from_millis(500)));
    assert_eq!(RobotsRules::parse(b"", AGENT).crawl_delay(), None);
}

#[test]
fn collects_sitemap_lines() {
    let r = RobotsRules::parse(
        b"Sitemap: https://e.test/s.xml\nUser-agent: *\nDisallow:\n",
        AGENT,
    );
    assert_eq!(r.sitemaps(), ["https://e.test/s.xml"]);
}

#[test]
fn our_own_group_wins_over_star() {
    let r = RobotsRules::parse(
        b"User-agent: *\nDisallow: /\n\nUser-agent: CodoSEObot\nAllow: /\n",
        AGENT,
    );
    assert!(r.allowed("/anything"));
    let blocked = RobotsRules::parse(b"User-agent: *\nDisallow: /\n", AGENT);
    assert!(blocked.blocks_everything());
}

#[test]
fn status_codes_follow_googles_rules() {
    assert!(RobotsRules::from_status(404).allowed("/x"));
    assert!(!RobotsRules::from_status(404).blocks_everything());
    assert!(RobotsRules::from_status(503).blocks_everything());
    assert!(!RobotsRules::from_status(503).allowed("/x"));
}

#[test]
fn ignores_everything_after_500_kib() {
    let mut body = b"User-agent: *\nDisallow: /early\n".to_vec();
    body.extend(std::iter::repeat_n(b'#', 600 * 1024));
    body.extend_from_slice(b"\nDisallow: /late\n");
    let r = RobotsRules::parse(&body, AGENT);
    assert!(!r.allowed("/early"));
    assert!(r.allowed("/late"));
}

fn fetcher() -> Fetcher {
    Fetcher::new(FetcherConfig::new(AddressPolicy::AllowPrivate)).unwrap()
}

#[tokio::test]
async fn fetches_and_keeps_the_file() {
    let srv = TestServer::start(Router::new().route(
        "/robots.txt",
        get(|| async { "User-agent: *\nDisallow: /cart\n" }),
    ))
    .await;
    let (rules, file) = fetch_robots(&fetcher(), &srv.url("/some/page"))
        .await
        .unwrap();
    assert!(!rules.allowed("/cart"));
    assert_eq!(file.status, 200);
    assert_eq!(file.body, "User-agent: *\nDisallow: /cart\n");
    assert_ne!(file.hash, 0);
}

#[tokio::test]
async fn missing_robots_txt_allows_everything() {
    let srv = TestServer::start(Router::new()).await;
    let (rules, file) = fetch_robots(&fetcher(), &srv.url("/")).await.unwrap();
    assert!(rules.allowed("/anything"));
    assert_eq!(file.status, 404);
}

#[tokio::test]
async fn server_error_blocks_everything() {
    let srv = TestServer::start(Router::new().route(
        "/robots.txt",
        get(|| async { (StatusCode::SERVICE_UNAVAILABLE, "down") }),
    ))
    .await;
    let (rules, _) = fetch_robots(&fetcher(), &srv.url("/")).await.unwrap();
    assert!(rules.blocks_everything());
}
