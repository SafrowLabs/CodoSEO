//! Peak heap during a crawl of an endless faceted site: the frontier cap keeps it flat.
//! Its own binary because of the counting allocator.

use std::time::Duration;

use codoseo_core::crawl::{AddressPolicy, CrawlConfig, CrawlLimits, Politeness, USER_AGENT};
use codoseo_crawler::StopReason;
use codoseo_crawler::crawl::crawl;
use codoseo_testkit::SiteBuilder;
use peak_alloc::PeakAlloc;

#[global_allocator]
static PEAK: PeakAlloc = PeakAlloc;
const MB: usize = 1024 * 1024;

#[test]
fn an_endless_faceted_site_crawls_in_flat_memory() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let site = rt.block_on(
        SiteBuilder::new()
            .html("/", "Home", &["/list?page=1"])
            .endless(100)
            .start(),
    );
    let cfg = CrawlConfig {
        start_url: site.url("/"),
        limits: CrawlLimits {
            max_pages: 300,
            request_timeout: Duration::from_secs(5),
            max_duration: Duration::from_secs(60),
            ..CrawlLimits::default()
        },
        politeness: Politeness {
            requests_per_sec: 1000.0,
            ..Politeness::default()
        },
        address_policy: AddressPolicy::AllowPrivate,
        user_agent: USER_AGENT.to_owned(),
    };

    PEAK.reset_peak_usage();
    let base = PEAK.current_usage();
    let out = rt.block_on(crawl(cfg, |_| {})).unwrap();
    let peak = PEAK.peak_usage().saturating_sub(base);
    println!(
        "crawl peak heap growth: {:.2} MB ({peak} bytes)",
        peak as f64 / MB as f64
    );

    assert_eq!(out.pages.len(), 300);
    assert_eq!(out.stop, StopReason::PageLimit);
    // Without the frontier cap the queue would hold about 30,000 URLs.
    assert!(peak < 24 * MB, "peak {} MB", peak / MB);
}
