//! Crawl entry points and errors. The orchestrator joins this module in a later task.

use std::sync::Arc;
use std::time::Duration;

use codoseo_core::crawl::CrawlConfig;
use tokio::sync::Semaphore;
use tokio::time::Instant;

use crate::fetch::{Fetcher, FetcherConfig};
use crate::politeness::Limiter;
use crate::preflight::{Preflight, preflight};

/// Why a crawl could not run at all. A crawl that ran and stopped early is a
/// `StopReason`, not an error.
#[derive(Debug, thiserror::Error)]
pub enum CrawlError {
    #[error("invalid start address: {0}")]
    InvalidStart(String),
    #[error("address not allowed: {0}")]
    AddressBlocked(String),
    #[error("could not set up the crawler: {0}")]
    Client(String),
}

/// Runs only the preflight step with a fetcher and limiter built from `cfg`.
#[doc(hidden)]
pub async fn preflight_for_tests(cfg: CrawlConfig) -> Result<Preflight, CrawlError> {
    let mut fetcher_cfg = FetcherConfig::new(cfg.address_policy);
    fetcher_cfg.user_agent = cfg.user_agent.clone();
    fetcher_cfg.request_timeout = cfg.limits.request_timeout;
    fetcher_cfg.connect_timeout = fetcher_cfg.connect_timeout.min(cfg.limits.request_timeout);
    fetcher_cfg.max_redirects = cfg.limits.max_redirects;
    fetcher_cfg.max_body_bytes = cfg.limits.max_page_bytes;
    let fetcher = Fetcher::new(fetcher_cfg).map_err(|e| CrawlError::Client(e.to_string()))?;
    let limiter = Limiter::new(&cfg.politeness, None, Arc::new(Semaphore::new(64)));
    let deadline = Instant::now() + cfg.limits.max_duration.max(Duration::from_secs(1));
    preflight(&cfg, &fetcher, &limiter, deadline).await
}
