//! Crawl configuration and the small per-crawl records stored alongside pages.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;

pub const USER_AGENT: &str = "CodoSEObot/0.1 (+https://codoseo.com/bot)";

/// Whether the crawler may connect to private and internal addresses.
/// The cloud uses `Public`; self-hosted, the CLI and local MCP use `AllowPrivate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressPolicy {
    Public,
    AllowPrivate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrawlLimits {
    pub max_pages: u32,
    pub max_duration: Duration,
    pub max_page_bytes: usize,
    pub request_timeout: Duration,
    pub max_redirects: u8,
    pub max_sitemap_urls: u32,
}

impl Default for CrawlLimits {
    fn default() -> Self {
        CrawlLimits {
            max_pages: 500,
            max_duration: Duration::from_secs(600),
            max_page_bytes: 5 * 1024 * 1024,
            request_timeout: Duration::from_secs(30),
            max_redirects: 10,
            max_sitemap_urls: 50_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Politeness {
    pub requests_per_sec: f32,
    pub per_site_connections: u32,
    pub max_crawl_delay: Duration,
    pub max_in_flight: u32,
    pub max_consecutive_failures: u32,
}

impl Default for Politeness {
    fn default() -> Self {
        Politeness {
            requests_per_sec: 5.0,
            per_site_connections: 2,
            max_crawl_delay: Duration::from_secs(10),
            max_in_flight: 64,
            max_consecutive_failures: 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrawlConfig {
    pub start_url: Url,
    pub limits: CrawlLimits,
    pub politeness: Politeness,
    pub address_policy: AddressPolicy,
    pub user_agent: String,
    /// Also fetch what the site declares about AI use outside robots.txt and its pages
    /// (`/.well-known/tdmrep.json`). Off for no-signup audits, which keep no AI access report.
    #[serde(default = "yes")]
    pub site_signals: bool,
}

fn yes() -> bool {
    true
}

/// robots.txt as fetched at the start of a crawl (kept for change detection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RobotsFile {
    pub status: u16,
    pub body: String,
    pub hash: u64,
}

/// What we keep about a site's sitemaps (the URL list itself is not stored).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SitemapSummary {
    pub files: Vec<Url>,
    pub url_count: u32,
    pub hash: u64,
    /// The URL cap was reached.
    pub truncated: bool,
    /// Sitemap files that could not be fetched or parsed.
    pub failed_files: u32,
    /// False when discovery stopped early (file cap or deadline), so the URL
    /// list may be partial and must not be read as the site shrinking.
    pub complete: bool,
}
