//! `/llms.txt`: one fetch after preflight, and a bounded check of the links it lists once the
//! crawl is over. Both go through the crawl's limiter, keep to robots.txt and stop at the
//! deadline.
//!
//! The file is informational ([`codoseo_geo::llms`]): nothing here can stop or slow the crawl
//! beyond its own few requests.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use codoseo_core::output::{LinkStatus, LlmsTxtFetch};
use codoseo_core::page::PageRecord;
use codoseo_core::url::{normalize, url_hash};
use codoseo_geo::llms::{LlmsState, MAX_BYTES, links, parse, state_of};
use tokio::time::{Instant, timeout_at};
use url::Url;

use crate::fetch::{FetchError, Fetcher};
use crate::politeness::{Limiter, retry_after_of};
use crate::robots::RobotsRules;

const PATH: &str = "/llms.txt";

/// The URL of `/llms.txt` on `origin`.
pub(crate) fn llms_url(origin: &Url) -> Option<Url> {
    origin.join(PATH).ok()
}

/// Fetches `/llms.txt` once, when robots.txt lets us, reading at most [`MAX_BYTES`] of it.
///
/// `None` when it was not fetched: robots.txt disallows it, the address is refused, or the
/// deadline came first (that is our budget, not the site's failure). A request that got no
/// response of its own (a timeout, a refused connection, a broken redirect) is kept as status 0
/// with `error`, so a file that stopped answering can be told apart from one never asked for.
pub(crate) async fn fetch_llms_txt(
    fetcher: &Fetcher,
    limiter: &Limiter,
    origin: &Url,
    rules: &RobotsRules,
    deadline: Instant,
) -> Option<LlmsTxtFetch> {
    if !rules.allowed(PATH) {
        return None;
    }
    let url = llms_url(origin)?;
    let fetched = timeout_at(deadline, async {
        let _permit = limiter.acquire().await;
        fetcher.fetch_raw(&url, MAX_BYTES).await
    })
    .await
    .ok()?;
    let res = match fetched {
        Ok(res) => res,
        Err(FetchError::Blocked(_)) => return None,
        Err(e) => {
            return Some(LlmsTxtFetch {
                error: Some(e.to_string()),
                ..LlmsTxtFetch::default()
            });
        }
    };
    limiter.on_response(res.status, retry_after_of(&res.headers));
    let ok = (200..300).contains(&res.status);
    let body = match res.body {
        Some(body) if ok => String::from_utf8_lossy(&body).into_owned(),
        _ => String::new(),
    };
    Some(LlmsTxtFetch {
        status: res.status,
        error: None,
        content_type: res.content_type,
        bytes: res.size_bytes,
        truncated: res.truncated,
        body,
        links: Vec::new(),
        external_unchecked: 0,
    })
}

/// Who may be asked about a link and how.
pub(crate) struct LinkCheck<'a> {
    pub fetcher: &'a Fetcher,
    pub limiter: &'a Limiter,
    pub rules: &'a RobotsRules,
    pub origin: &'a Url,
    /// At most this many requests (`CrawlLimits::llms_link_checks`).
    pub max_requests: u16,
    /// Per-request time limit.
    pub timeout: Duration,
    pub deadline: Instant,
    /// False once the crawl stopped because the site blocked or failed it: no new requests then.
    pub may_request: bool,
}

/// Fills `llms.links` and `llms.external_unchecked` for a file that was served.
///
/// Links on the site's origin that the crawl recorded take its status, with no request
/// (`from_crawl`). Others on the origin are requested, in file order, up to `max_requests`, when
/// robots.txt allows them and before the deadline; the rest are left out and count as unchecked.
/// Links on other origins are never requested, only counted.
pub(crate) async fn check_links(llms: &mut LlmsTxtFetch, pages: &[PageRecord], c: LinkCheck<'_>) {
    if state_of(llms) != LlmsState::Available {
        return;
    }
    let Some(base) = llms_url(c.origin) else {
        return;
    };
    let crawled: HashMap<u64, Option<u16>> = pages
        .iter()
        .filter_map(|p| match (p.status, &p.error) {
            // Not fetched: robots.txt disallowed it.
            (0, None) => None,
            (0, Some(_)) => Some((p.url_hash, None)),
            (status, _) => Some((p.url_hash, Some(status))),
        })
        .collect();

    let mut seen: HashSet<u64> = HashSet::new();
    let mut requests = 0u16;
    let doc = parse(&llms.body);
    for href in links(&doc) {
        let Some(url) = normalize(&base, &href) else {
            continue;
        };
        if !matches!(url.scheme(), "http" | "https") || !seen.insert(url_hash(&url)) {
            continue;
        }
        if url.origin() != c.origin.origin() {
            llms.external_unchecked = llms.external_unchecked.saturating_add(1);
            continue;
        }
        if let Some(&status) = crawled.get(&url_hash(&url)) {
            llms.links.push(LinkStatus {
                url: url.to_string(),
                status,
                from_crawl: true,
            });
            continue;
        }
        if !c.may_request
            || requests >= c.max_requests
            || !c.rules.allowed(url.as_str())
            || Instant::now() >= c.deadline
        {
            continue;
        }
        requests += 1;
        let until = c.deadline.min(Instant::now() + c.timeout);
        let res = timeout_at(until, async {
            let _permit = c.limiter.acquire().await;
            // The status is all we need: no body is kept.
            c.fetcher.fetch_raw(&url, 0).await
        })
        .await;
        let status = match res {
            // Cut short by the deadline: not checked.
            Err(_) if Instant::now() >= c.deadline => break,
            Err(_) | Ok(Err(_)) => None,
            Ok(Ok(r)) => {
                c.limiter.on_response(r.status, retry_after_of(&r.headers));
                Some(r.status)
            }
        };
        llms.links.push(LinkStatus {
            url: url.to_string(),
            status,
            from_crawl: false,
        });
    }
}
