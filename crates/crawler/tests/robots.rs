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

// Fixes from the M1 review.

/// 500 KiB of wildcard rules that end in `$` (review C2).
pub fn hostile_robots_txt() -> Vec<u8> {
    let mut body = b"User-agent: *\n".to_vec();
    let mut i = 0;
    while body.len() < 500 * 1024 {
        body.extend_from_slice(format!("Disallow: /*a*b*c*d*e*f*g*h*{i}$\n").as_bytes());
        i += 1;
    }
    body
}

#[test]
fn hostile_rule_lists_stay_fast() {
    let started = std::time::Instant::now();
    let r = RobotsRules::parse(&hostile_robots_txt(), AGENT);
    for i in 0..200 {
        r.allowed(&format!("/products/item-{i}?colour=green"));
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn huge_or_odd_crawl_delays_do_not_panic() {
    for (value, want_ms) in [
        ("1e30", Some(10_000)),
        ("99999999999999999999999", Some(10_000)),
        ("2.5", Some(2_500)),
        ("inf", None),
        ("-5", None),
        ("NaN", None),
        ("soon", None),
    ] {
        let r = RobotsRules::parse(
            format!("User-agent: *\nCrawl-delay: {value}\n").as_bytes(),
            AGENT,
        );
        assert_eq!(
            r.crawl_delay().map(|d| d.as_millis()),
            want_ms,
            "Crawl-delay: {value}"
        );
    }
}

#[test]
fn rate_limited_robots_txt_blocks_everything() {
    assert!(RobotsRules::from_status(429).blocks_everything());
}

#[test]
fn versioned_agent_lines_match_our_product_token() {
    let r = RobotsRules::parse(
        b"User-agent: *\nDisallow: /\n\nUser-agent: CodoSEObot/0.1\nAllow: /\n",
        AGENT,
    );
    assert!(r.allowed("/anything"));
}

#[test]
fn an_oversized_rule_is_skipped_not_the_whole_file() {
    let body = format!(
        "User-agent: *\nDisallow: /{}\nDisallow: /private\n",
        "*a".repeat(30_000)
    );
    assert!(!RobotsRules::parse(body.as_bytes(), AGENT).allowed("/private"));
}

mod cross_check {
    use codoseo_geo::robots::RobotsTxt;
    use proptest::prelude::*;

    use super::{AGENT, RobotsRules};

    fn line() -> impl Strategy<Value = String> {
        let agent = prop_oneof![
            Just("User-agent: *".to_owned()),
            Just("User-agent: CodoSEObot".to_owned()),
            Just("user-agent: codoseobot/0.1".to_owned()),
            Just("User-agent: GPTBot".to_owned()),
            Just("User-agent: Googlebot".to_owned()),
        ];
        let pattern = "(/|\\*)?([a-c]{1,2}|\\*){0,3}(/[a-c]{1,2}){0,2}\\$?";
        let rule = (prop::bool::ANY, pattern)
            .prop_map(|(allow, p)| format!("{}: {p}", if allow { "Allow" } else { "Disallow" }));
        prop_oneof![
            3 => agent,
            6 => rule,
            1 => Just("# a comment".to_owned()),
            1 => Just("Crawl-delay: 2".to_owned()),
            1 => Just("Content-Signal: ai-train=no".to_owned()),
            1 => Just("Sitemap: https://e.test/s.xml".to_owned()),
            1 => Just(String::new()),
        ]
    }

    proptest! {
        /// The crawler's wrapper and the shared parser must never disagree.
        #[test]
        fn wrapper_agrees_with_the_shared_parser(
            lines in prop::collection::vec(line(), 0..14),
            paths in prop::collection::vec("(/[a-c]{1,2}){0,3}/?(\\?[a-c]=[a-c])?", 1..8),
        ) {
            let body = lines.join("\n");
            let rules = RobotsRules::parse(body.as_bytes(), AGENT);
            let txt = RobotsTxt::parse(body.as_bytes());
            for path in paths {
                let path = if path.is_empty() { "/".to_owned() } else { path };
                prop_assert_eq!(
                    rules.allowed(&path),
                    txt.verdict(AGENT, &path).allowed,
                    "{} on {:?}", path, body
                );
            }
            prop_assert_eq!(rules.sitemaps(), txt.sitemaps());
        }
    }
}
