//! `/llms.txt`: one fetch after preflight, and a bounded check of the links it lists once the
//! crawl is over. Both go through the crawl's limiter, keep to robots.txt (on every redirect hop:
//! link checks follow none, the file's fetch checks each) and stop at the
//! deadline.
//!
//! The file is informational ([`codoseo_geo::llms`]): nothing here can stop or slow the crawl
//! beyond its own few requests.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use codoseo_core::output::{LinkStatus, LlmsSkip, LlmsTxtFetch};
use codoseo_core::page::PageRecord;
use codoseo_core::url::{normalize, url_hash};
use codoseo_geo::llms::{LlmsState, MAX_BYTES, links, parse, state_of};
use reqwest::header::LOCATION;
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

/// Wait before the retry when the answer gave no `Retry-After`.
const RETRY_WAIT: Duration = Duration::from_secs(1);
/// The longest `Retry-After` honoured; a longer one means no retry.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(5);

/// Fetches `/llms.txt` when robots.txt lets us, reading at most [`MAX_BYTES`] of it.
///
/// A 429, a 5xx or no response at all is retried once, after the answer's `Retry-After` (at most
/// [`MAX_RETRY_WAIT`], else [`RETRY_WAIT`]) and only when that still fits before the deadline: one
/// blip should not read as a file that stopped serving. The retry's outcome is kept, or the first
/// one's when the retry could not be made.
///
/// `Err` with the reason when it was not fetched: robots.txt disallows it (or a redirect's
/// target), the address is refused, or the deadline came first (that is our budget, not the
/// site's failure). A request that got no response of its own (a timeout, a refused connection, a
/// broken redirect) is kept as status 0 with `error`, so a file that stopped answering can be
/// told apart from one never asked for.
pub(crate) async fn fetch_llms_txt(
    fetcher: &Fetcher,
    limiter: &Limiter,
    origin: &Url,
    rules: &RobotsRules,
    deadline: Instant,
) -> Result<LlmsTxtFetch, LlmsSkip> {
    if !rules.allowed(PATH) {
        return Err(LlmsSkip::RobotsDisallowed);
    }
    let url = llms_url(origin).ok_or(LlmsSkip::Refused)?;
    let first = fetch_once(fetcher, limiter, &url, rules, deadline).await?;
    let Some(wait) = first.retry else {
        return Ok(first.fetch);
    };
    if wait > MAX_RETRY_WAIT || Instant::now() + wait >= deadline {
        return Ok(first.fetch);
    }
    tokio::time::sleep(wait).await;
    match fetch_once(fetcher, limiter, &url, rules, deadline).await {
        Ok(again) => Ok(again.fetch),
        Err(_) => Ok(first.fetch),
    }
}

/// One fetch of `/llms.txt`, and how long to wait before retrying it when it failed in a way
/// worth one retry.
struct Attempt {
    fetch: LlmsTxtFetch,
    retry: Option<Duration>,
}

/// Redirects followed on the way to the file.
const MAX_HOPS: usize = 5;

/// Fetches `url`, following redirects one hop at a time so that robots.txt is applied to each:
/// the site's rules on its own origin, the target's own robots.txt on another. `Err` when a hop
/// is disallowed, an address is refused, or the deadline came first.
async fn fetch_once(
    fetcher: &Fetcher,
    limiter: &Limiter,
    url: &Url,
    rules: &RobotsRules,
    deadline: Instant,
) -> Result<Attempt, LlmsSkip> {
    let failed = |e: FetchError| {
        let transient = matches!(
            e,
            FetchError::Timeout | FetchError::Connect(_) | FetchError::Http(_)
        );
        Attempt {
            fetch: LlmsTxtFetch {
                error: Some(e.to_string()),
                ..LlmsTxtFetch::default()
            },
            retry: transient.then_some(RETRY_WAIT),
        }
    };
    timeout_at(deadline, async {
        let mut current = url.clone();
        let mut hops = 0;
        loop {
            let fetched = {
                let _permit = limiter.acquire().await;
                fetcher.fetch_raw_once(&current, MAX_BYTES).await
            };
            let res = match fetched {
                Ok(res) => res,
                Err(FetchError::Blocked(_)) => return Err(LlmsSkip::Refused),
                Err(e) => return Ok(failed(e)),
            };
            let retry_after = retry_after_of(&res.headers);
            limiter.on_response(res.status, retry_after);
            if (300..400).contains(&res.status)
                && let Some(location) = res.headers.get(LOCATION)
            {
                let location = String::from_utf8_lossy(location.as_bytes()).into_owned();
                let Some(next) = current
                    .join(&location)
                    .ok()
                    .filter(|u| matches!(u.scheme(), "http" | "https"))
                else {
                    return Ok(failed(FetchError::InvalidRedirect { location }));
                };
                hops += 1;
                if hops > MAX_HOPS {
                    return Ok(failed(FetchError::TooManyRedirects { chain: Vec::new() }));
                }
                if !may_follow(fetcher, limiter, url, rules, &next).await {
                    return Err(LlmsSkip::RobotsDisallowed);
                }
                current = next;
                continue;
            }
            let ok = (200..300).contains(&res.status);
            let retry =
                (res.status == 429 || res.status >= 500).then(|| retry_after.unwrap_or(RETRY_WAIT));
            let body = match res.body {
                Some(body) if ok => String::from_utf8_lossy(&body).into_owned(),
                _ => String::new(),
            };
            return Ok(Attempt {
                fetch: LlmsTxtFetch {
                    status: res.status,
                    error: None,
                    content_type: res.content_type,
                    bytes: res.size_bytes,
                    truncated: res.truncated,
                    body,
                    final_url: Some(res.final_url.to_string()),
                    links: Vec::new(),
                    external_unchecked: 0,
                },
                retry,
            });
        }
    })
    .await
    .unwrap_or(Err(LlmsSkip::Deadline))
}

/// Whether robots.txt lets CodoSEObot follow a redirect to `next`: the site's `rules` on the
/// origin of `start`, else the robots.txt of `next`'s own origin (one more request; one that
/// can't be read means no).
async fn may_follow(
    fetcher: &Fetcher,
    limiter: &Limiter,
    start: &Url,
    rules: &RobotsRules,
    next: &Url,
) -> bool {
    if next.origin() == start.origin() {
        return rules.allowed(next.as_str());
    }
    let _permit = limiter.acquire().await;
    match crate::robots::fetch_robots(fetcher, next).await {
        Ok((theirs, _)) => theirs.allowed(next.as_str()),
        Err(_) => false,
    }
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

/// Fills `llms.links` and `llms.external_unchecked` for a file that was served. Relative links
/// resolve against the file's final URL (after redirects).
///
/// Links on the site's origin that the crawl recorded take its status, with no request
/// (`from_crawl`). Others on the origin are requested, in file order, up to `max_requests`, when
/// robots.txt allows them and before the deadline; the rest are left out and count as unchecked.
/// Links on other origins are never requested, only counted.
pub(crate) async fn check_links(llms: &mut LlmsTxtFetch, pages: &[PageRecord], c: LinkCheck<'_>) {
    if state_of(llms) != LlmsState::Available {
        return;
    }
    // Relative links resolve against where the file was served from, after redirects. Links that
    // land on another origin are counted, not checked: only the site's robots.txt was read.
    let Some(base) = llms
        .final_url
        .as_deref()
        .and_then(|u| Url::parse(u).ok())
        .or_else(|| llms_url(c.origin))
    else {
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
            // The status is all we need: no body is kept. A redirect is not followed (its target
            // may be one robots.txt disallows) and counts as resolved, as in Lighthouse.
            c.fetcher.fetch_raw_once(&url, 0).await
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

/// How long a one-off [`check_llms_txt`] may take in all, robots.txt and link checks included.
pub const PAGE_CHECK_DEADLINE: Duration = Duration::from_secs(30);

/// What a one-off look at a site's `/llms.txt` found.
#[derive(Debug, Clone)]
pub struct LlmsLookup {
    /// The URL that was fetched.
    pub url: String,
    /// The fetch, with `links` filled for a file that was served.
    pub fetch: LlmsTxtFetch,
}

/// Fetches `/llms.txt` on the site of `url` as CodoSEObot, after reading its robots.txt, and
/// checks up to `link_checks` of the links it lists on the same origin. For `codoseo llms-txt`
/// and the local MCP server; nothing is stored. Errors are messages for the user: an unreadable
/// robots.txt, a robots.txt that keeps CodoSEObot off `/llms.txt`, an address that is refused.
pub async fn check_llms_txt(
    url: &Url,
    address_policy: codoseo_core::crawl::AddressPolicy,
    link_checks: u16,
    deadline: Duration,
) -> Result<LlmsLookup, String> {
    use crate::fetch::FetcherConfig;
    let until = Instant::now() + deadline;
    let origin = url
        .join("/")
        .map_err(|e| format!("not a site address: {e}"))?;
    let llms_url = llms_url(&origin).ok_or_else(|| "not a site address".to_owned())?;
    let fetcher = Fetcher::new(FetcherConfig::new(address_policy)).map_err(|e| e.to_string())?;
    let (rules, _) = timeout_at(until, crate::robots::fetch_robots(&fetcher, &origin))
        .await
        .map_err(|_| "could not fetch robots.txt in time; nothing was checked".to_owned())?
        .map_err(|e| format!("could not fetch robots.txt: {e}"))?;
    if !rules.allowed(PATH) {
        return Err(
            "robots.txt disallows CodoSEObot on /llms.txt, so it is not fetched; nothing was checked"
                .to_owned(),
        );
    }
    let politeness = codoseo_core::crawl::Politeness::default();
    let limiter = Limiter::new(
        &politeness,
        rules.crawl_delay(),
        std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
    );
    let mut fetch = fetch_llms_txt(&fetcher, &limiter, &origin, &rules, until)
        .await
        .map_err(|skip| match skip {
            LlmsSkip::RobotsDisallowed => {
                "robots.txt disallows CodoSEObot on /llms.txt, so it is not fetched; nothing was checked"
                    .to_owned()
            }
            LlmsSkip::Refused => "/llms.txt was not fetched: the address is refused".to_owned(),
            LlmsSkip::Deadline => "/llms.txt was not fetched: the time ran out".to_owned(),
        })?;
    check_links(
        &mut fetch,
        &[],
        LinkCheck {
            fetcher: &fetcher,
            limiter: &limiter,
            rules: &rules,
            origin: &origin,
            max_requests: link_checks,
            timeout: Duration::from_secs(10),
            deadline: until,
            may_request: true,
        },
    )
    .await;
    Ok(LlmsLookup {
        url: llms_url.to_string(),
        fetch,
    })
}
