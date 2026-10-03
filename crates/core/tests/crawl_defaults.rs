use std::time::Duration;

use codoseo_core::crawl::{CrawlLimits, Politeness, USER_AGENT};

#[test]
fn crawl_limits_default_to_the_global_constraints() {
    let l = CrawlLimits::default();
    assert_eq!(l.max_pages, 500);
    assert_eq!(l.max_duration, Duration::from_secs(600));
    assert_eq!(l.max_page_bytes, 5 * 1024 * 1024);
    assert_eq!(l.request_timeout, Duration::from_secs(30));
    assert_eq!(l.max_redirects, 10);
    assert_eq!(l.max_sitemap_urls, 50_000);
}

#[test]
fn politeness_defaults_to_the_global_constraints() {
    let p = Politeness::default();
    assert_eq!(p.requests_per_sec, 5.0);
    assert_eq!(p.per_site_connections, 2);
    assert_eq!(p.max_crawl_delay, Duration::from_secs(10));
    assert_eq!(p.max_in_flight, 64);
    assert_eq!(p.max_consecutive_failures, 20);
}

#[test]
fn user_agent_names_the_bot_page() {
    assert_eq!(USER_AGENT, "CodoSEObot/0.1 (+https://codoseo.com/bot)");
}
