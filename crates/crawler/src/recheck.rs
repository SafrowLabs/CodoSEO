//! The fetches of one AI access recheck: robots.txt, a few named pages, `/llms.txt` and the home
//! page as Markdown, as CodoSEObot and as politely as a crawl. What to fetch and what it means live in
//! [`codoseo_geo::recheck`]; the synthetic probes run on their own ([`crate::probe`]).
//!
//! robots.txt is always read first, whatever the recheck is about: it gives the crawl delay and
//! keeps CodoSEObot off the pages it disallows, which are then not requested. Every request goes
//! through one [`Limiter`] with one connection, and everything ends by the deadline.

use std::sync::Arc;
use std::time::Duration;

use codoseo_core::crawl::{AddressPolicy, Politeness, RobotsFile, USER_AGENT};
use codoseo_core::output::{LlmsSkip, LlmsTxtFetch, MarkdownProbe};
use codoseo_core::page::PageRecord;
use tokio::sync::Semaphore;
use tokio::time::{Instant, timeout_at};
use url::Url;

use crate::crawl::inspected;
use crate::fetch::{FetchError, Fetcher, FetcherConfig};
use crate::llms::fetch_llms_txt;
use crate::markdown::fetch_markdown;
use crate::politeness::{Limiter, retry_after_of};
use crate::robots::fetch_robots;

/// What one recheck fetches.
#[derive(Debug, Clone)]
pub struct RecheckFetch {
    /// Any URL of the site; robots.txt and `/llms.txt` are read from its origin.
    pub site: Url,
    /// Pages to fetch again, in order, as a crawl would (HTML bodies only).
    pub pages: Vec<Url>,
    /// Fetch `/llms.txt` (no link checks).
    pub llms: bool,
    /// Ask for `site` as Markdown, and for the Markdown alternate it advertises.
    pub markdown: bool,
    pub address_policy: AddressPolicy,
    pub politeness: Politeness,
    /// Per-request time limit.
    pub request_timeout: Duration,
    /// Everything ends by then: what has not started is not fetched.
    pub deadline: Instant,
}

/// What a recheck fetched.
#[derive(Debug, Clone)]
pub struct RecheckFetched {
    pub robots: RobotsFile,
    /// The planned pages that were requested, as the crawler records them.
    pub pages: Vec<PageRecord>,
    /// Planned pages robots.txt keeps CodoSEObot off: never requested.
    pub robots_skipped: Vec<Url>,
    /// Planned pages the deadline came before.
    pub not_reached: Vec<Url>,
    /// `/llms.txt`, when asked for: the fetch, or why it was not made.
    pub llms: Option<Result<LlmsTxtFetch, LlmsSkip>>,
    /// The Markdown requests, when asked for; `None` inside when none was made (robots.txt
    /// disallows the page, the address is refused, the time ran out).
    pub markdown: Option<Option<MarkdownProbe>>,
    /// Requests sent, every redirect hop counted.
    pub requests: u32,
}

/// Why a recheck fetched nothing: the site could not be asked at all.
#[derive(Debug, thiserror::Error)]
pub enum RecheckFetchError {
    #[error("could not set up the fetcher: {0}")]
    Client(String),
    #[error("the address is refused: {0}")]
    AddressBlocked(String),
    #[error("robots.txt got no answer: {0}")]
    Unreachable(String),
    #[error("the time ran out before robots.txt answered")]
    Deadline,
}

/// Fetches what `cfg` names. robots.txt that got no answer at all (or an address that is
/// refused) is an error: nothing else is requested then. A page or `/llms.txt` that fails is kept
/// as it failed.
pub async fn recheck_fetch(cfg: RecheckFetch) -> Result<RecheckFetched, RecheckFetchError> {
    let fetcher = Fetcher::new(FetcherConfig {
        user_agent: USER_AGENT.to_owned(),
        request_timeout: cfg.request_timeout,
        connect_timeout: cfg.request_timeout.min(Duration::from_secs(10)),
        ..FetcherConfig::new(cfg.address_policy)
    })
    .map_err(|e| RecheckFetchError::Client(e.to_string()))?;
    let limiter = Limiter::new(&cfg.politeness, None, Arc::new(Semaphore::new(1)));
    let origin = cfg
        .site
        .join("/")
        .map_err(|e| RecheckFetchError::Client(format!("not a site address: {e}")))?;

    let (rules, robots) = {
        let _permit = limiter.acquire().await;
        match timeout_at(cfg.deadline, fetch_robots(&fetcher, &origin)).await {
            Err(_) => return Err(RecheckFetchError::Deadline),
            Ok(Err(FetchError::Blocked(msg))) => {
                return Err(RecheckFetchError::AddressBlocked(msg));
            }
            Ok(Err(e)) => return Err(RecheckFetchError::Unreachable(e.to_string())),
            Ok(Ok(got)) => got,
        }
    };
    limiter.on_response(robots.status, None);
    limiter.set_crawl_delay(rules.crawl_delay());

    let mut out = RecheckFetched {
        robots,
        pages: Vec::new(),
        robots_skipped: Vec::new(),
        not_reached: Vec::new(),
        llms: None,
        markdown: None,
        requests: 0,
    };
    for url in &cfg.pages {
        if !rules.allowed(url.as_str()) {
            out.robots_skipped.push(url.clone());
            continue;
        }
        if Instant::now() >= cfg.deadline {
            out.not_reached.push(url.clone());
            continue;
        }
        let _permit = limiter.acquire().await;
        if Instant::now() >= cfg.deadline {
            out.not_reached.push(url.clone());
            continue;
        }
        let res = timeout_at(cfg.deadline, fetcher.fetch(url))
            .await
            .unwrap_or(Err(FetchError::Timeout));
        if let Ok(r) = &res {
            limiter.on_response(r.status, retry_after_of(&r.headers));
        }
        out.pages.push(inspected(url, res));
    }
    if cfg.llms {
        out.llms = Some(fetch_llms_txt(&fetcher, &limiter, &origin, &rules, cfg.deadline).await);
    }
    if cfg.markdown {
        out.markdown =
            Some(fetch_markdown(&fetcher, &limiter, &cfg.site, &rules, cfg.deadline).await);
    }
    out.requests = fetcher.requests();
    Ok(out)
}
