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
    // Facet pages link to fresh facets of their own, so the crawl really does walk
    // into a growing space: some fetched pages are facets of facets (f > 100).
    let deep_facets = out
        .pages
        .iter()
        .filter(|p| {
            p.url
                .query_pairs()
                .any(|(k, v)| k == "f" && v.parse::<usize>().is_ok_and(|f| f > 100))
        })
        .count();
    assert!(deep_facets > 50, "only {deep_facets} facet-of-facet pages");
    // Each of the ~300 fetched pages links 100 fresh URLs, so without the frontier cap
    // the queue would hold about 30,000 URLs by the end.
    assert!(peak < 24 * MB, "peak {} MB", peak / MB);
}

/// The nightly workload: a crawl of 50,000 pages of an endless faceted site. The result keeps
/// every page record, so the heap grows with the page count, but it must stay within a fixed
/// per-page budget (no leak per fetch, the frontier stays capped). Ignored in the normal test run;
/// `.github/workflows/nightly.yml` runs it with `-- --ignored`. `CODOSEO_MEMORY_PAGES` lowers the
/// page count for a quick local check.
#[test]
#[ignore = "50,000-page crawl, run by the nightly workflow"]
fn fifty_thousand_pages_stay_within_the_memory_budget() {
    let pages: u32 = std::env::var("CODOSEO_MEMORY_PAGES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50_000);
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
            max_pages: pages,
            request_timeout: Duration::from_secs(10),
            max_duration: Duration::from_secs(30 * 60),
            ..CrawlLimits::default()
        },
        politeness: Politeness {
            requests_per_sec: 100_000.0,
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
        "{pages}-page crawl peak heap growth: {:.2} MB ({peak} bytes)",
        peak as f64 / MB as f64
    );

    assert_eq!(out.pages.len(), pages as usize);
    assert_eq!(out.stop, StopReason::PageLimit);
    // Measured at about 1.7 KiB per page in a debug build (87 MB for 50,000); the budget leaves
    // room for allocator noise but catches a per-fetch leak.
    assert!(peak < pages as usize * 3 * 1024, "peak {} MB", peak / MB);
}
