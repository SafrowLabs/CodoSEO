//! The first step of a crawl: settle the real start address, read robots.txt and the
//! sitemaps, and decide the stop reasons that don't need a crawl.
//!
//! Order: robots.txt of the requested origin, then the start page (one retry after a
//! 429 or 503), then robots.txt again when the start page moved to another origin.
//! Every request goes through the limiter.

use std::time::{Duration, SystemTime};

use codoseo_core::crawl::{CrawlConfig, RobotsFile, SitemapSummary};
use codoseo_core::output::StopReason;
use codoseo_core::url::normalize;
use reqwest::header::RETRY_AFTER;
use tokio::time::Instant;
use url::Url;

use crate::crawl::CrawlError;
use crate::fetch::{FetchError, FetchResult, Fetcher};
use crate::guard::check_url;
use crate::politeness::{Limiter, parse_retry_after};
use crate::robots::{RobotsRules, fetch_robots};
use crate::sitemap::discover;

pub const BLOCKED_MSG: &str = "site blocked our crawler";
pub const LOGIN_MSG: &str = "site requires a login";

/// How long sitemap discovery may take, whatever the crawl's own deadline.
const SITEMAP_BUDGET: Duration = Duration::from_secs(60);

/// What the crawl learned before fetching its first page.
#[doc(hidden)]
pub struct Preflight {
    /// Scheme, host and port of the settled start address, with path `/`.
    pub origin: Url,
    /// The requested start address and what fetching it gave; `None` when robots.txt
    /// stopped the crawl first.
    pub start: Option<(Url, Result<FetchResult, FetchError>)>,
    pub rules: RobotsRules,
    /// `None` when robots.txt could not be reached.
    pub robots: Option<RobotsFile>,
    pub sitemap_urls: Vec<Url>,
    pub sitemap: SitemapSummary,
    /// Set when the crawl should not go on.
    pub stop: Option<StopReason>,
}

pub(crate) async fn preflight(
    cfg: &CrawlConfig,
    fetcher: &Fetcher,
    limiter: &Limiter,
    deadline: Instant,
) -> Result<Preflight, CrawlError> {
    let requested = normalize(&cfg.start_url, cfg.start_url.as_str())
        .ok_or_else(|| CrawlError::InvalidStart(cfg.start_url.to_string()))?;
    check_url(&requested, cfg.address_policy)
        .map_err(|e| CrawlError::AddressBlocked(e.to_string()))?;

    let (rules, robots) = robots_for(fetcher, limiter, &requested).await?;
    let stop = robots_stop(&rules, robots.as_ref()).or_else(|| {
        let path = path_and_query(&requested);
        (!rules.allowed(&path)).then_some(StopReason::RobotsBlocked)
    });
    if stop.is_some() {
        return Ok(Preflight {
            origin: origin_of(&requested),
            start: None,
            rules,
            robots,
            sitemap_urls: Vec::new(),
            sitemap: SitemapSummary::default(),
            stop,
        });
    }

    let result = fetch_start(fetcher, limiter, &requested).await;
    if let Err(FetchError::Blocked(msg)) = &result {
        return Err(CrawlError::AddressBlocked(msg.clone()));
    }
    let final_url = match &result {
        Ok(res) => res.final_url.clone(),
        Err(_) => requested.clone(),
    };
    let origin = origin_of(&final_url);

    let (rules, robots) = if final_url.origin() == requested.origin() {
        (rules, robots)
    } else {
        robots_for(fetcher, limiter, &final_url).await?
    };

    let stop = match &result {
        Err(e) => Some(StopReason::Unreachable(e.to_string())),
        Ok(res) => robots_stop(&rules, robots.as_ref()).or(match res.status {
            401 => Some(StopReason::Blocked(LOGIN_MSG.to_owned())),
            403 | 429 | 503 => Some(StopReason::Blocked(BLOCKED_MSG.to_owned())),
            _ => None,
        }),
    };
    let mut pre = Preflight {
        origin,
        start: Some((requested, result)),
        rules,
        robots,
        sitemap_urls: Vec::new(),
        sitemap: SitemapSummary::default(),
        stop,
    };
    if pre.stop.is_none() {
        let seeds = sitemap_seeds(&pre.origin, &pre.rules);
        let deadline = deadline.min(Instant::now() + SITEMAP_BUDGET);
        let found = discover(
            fetcher,
            Some(limiter),
            &seeds,
            cfg.limits.max_sitemap_urls,
            deadline,
        )
        .await;
        pre.sitemap_urls = found.urls;
        pre.sitemap = found.summary;
    }
    Ok(pre)
}

/// Fetches robots.txt for the site `url` belongs to. A connection-level failure gives
/// no file and allow-all rules; a refused address is an error.
async fn robots_for(
    fetcher: &Fetcher,
    limiter: &Limiter,
    url: &Url,
) -> Result<(RobotsRules, Option<RobotsFile>), CrawlError> {
    let fetched = {
        let _permit = limiter.acquire().await;
        fetch_robots(fetcher, url).await
    };
    match fetched {
        Ok((rules, file)) => Ok((rules, Some(file))),
        Err(FetchError::Blocked(msg)) => Err(CrawlError::AddressBlocked(msg)),
        Err(_) => Ok((RobotsRules::from_status(404), None)),
    }
}

/// The stop reasons robots.txt decides on its own, in order: 429, 5xx, a full block.
fn robots_stop(rules: &RobotsRules, file: Option<&RobotsFile>) -> Option<StopReason> {
    match file.map(|f| f.status) {
        Some(429) => return Some(StopReason::Blocked(BLOCKED_MSG.to_owned())),
        Some(status) if status >= 500 => {
            return Some(StopReason::Unreachable(format!(
                "robots.txt returned HTTP {status}"
            )));
        }
        _ => {}
    }
    rules
        .blocks_everything()
        .then_some(StopReason::RobotsBlocked)
}

/// Fetches the start page, retrying once after a 429 or 503.
async fn fetch_start(
    fetcher: &Fetcher,
    limiter: &Limiter,
    url: &Url,
) -> Result<FetchResult, FetchError> {
    let first = {
        let _permit = limiter.acquire().await;
        fetcher.fetch(url).await
    };
    let Ok(res) = &first else { return first };
    if !matches!(res.status, 429 | 503) {
        return first;
    }
    let retry_after = res
        .headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| parse_retry_after(v, SystemTime::now()));
    limiter.on_response(res.status, retry_after);
    let _permit = limiter.acquire().await;
    fetcher.fetch(url).await
}

/// The `Sitemap:` lines of robots.txt plus `/sitemap.xml`.
fn sitemap_seeds(origin: &Url, rules: &RobotsRules) -> Vec<Url> {
    let mut seeds: Vec<Url> = rules
        .sitemaps()
        .iter()
        .filter_map(|s| normalize(origin, s))
        .collect();
    if let Some(default) = normalize(origin, "/sitemap.xml") {
        seeds.push(default);
    }
    seeds
}

/// Scheme, host and port of `url`, with path `/`.
fn origin_of(url: &Url) -> Url {
    let mut origin = url.clone();
    origin.set_path("/");
    origin.set_query(None);
    origin.set_fragment(None);
    let _ = origin.set_username("");
    let _ = origin.set_password(None);
    normalize(&origin, origin.as_str()).unwrap_or(origin)
}

fn path_and_query(url: &Url) -> String {
    match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_owned(),
    }
}
