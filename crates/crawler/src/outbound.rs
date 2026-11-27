//! Status checks for what pages point at beyond the site's own pages: links to other sites,
//! and the images, scripts and stylesheets pages load (on any host).
//!
//! The crawl collects the addresses as it reads pages; when it is over, each distinct address
//! gets one request, up to `CrawlLimits::outbound_checks`. Only an answer that plainly means
//! "gone" is reported, because other sites often turn crawlers away with 403, 429 or a
//! challenge page while serving browsers normally:
//!
//! - 404 and 410 on any host;
//! - any other 4xx or 5xx on the site's own host, except 401, 403 and 429;
//! - a host that does not resolve or refuses the connection, twice in a row.
//!
//! Timeouts and everything else count as checked and fine. Requests to the site's own host
//! keep to robots.txt and the crawl's rate limit; other hosts get at most two requests at a
//! time each.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use codoseo_core::output::{BrokenTarget, OutboundKind, OutboundReport};
use codoseo_core::url::url_hash;
use futures_util::StreamExt;
use futures_util::stream;
use tokio::sync::Semaphore;
use tokio::time::{Instant, timeout_at};
use url::Url;

use crate::fetch::{FetchError, Fetcher};
use crate::politeness::Limiter;
use crate::robots::RobotsRules;
use crate::scope::SiteScope;

/// Distinct addresses remembered during one crawl; later ones are not tracked at all.
const MAX_TRACKED: usize = 50_000;
/// Requests in flight at once, over all hosts.
const CONCURRENCY: usize = 8;
/// Requests in flight at once to one host that is not the crawled site.
const PER_HOST: usize = 2;
/// The longest one request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// The longest the whole stage may take, inside the crawl's own time limit.
const BUDGET: Duration = Duration::from_secs(300);

struct Target {
    url: Url,
    kind: OutboundKind,
    from: Vec<u32>,
}

/// The addresses seen so far, in first-seen order, each with the pages that use it.
#[derive(Default)]
pub(crate) struct Targets {
    index: HashMap<u64, usize>,
    list: Vec<Target>,
    /// Addresses seen after `MAX_TRACKED` was reached.
    dropped: u32,
}

impl Targets {
    /// Records that page `from` uses `url`. The first kind an address is seen as is kept.
    pub(crate) fn note(&mut self, url: &Url, kind: OutboundKind, from: u32) {
        let hash = url_hash(url);
        if let Some(&i) = self.index.get(&hash) {
            let pages = &mut self.list[i].from;
            if pages.last() != Some(&from) {
                pages.push(from);
            }
            return;
        }
        if self.list.len() >= MAX_TRACKED {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.index.insert(hash, self.list.len());
        self.list.push(Target {
            url: url.clone(),
            kind,
            from: vec![from],
        });
    }
}

pub(crate) struct Check<'a> {
    pub fetcher: &'a Fetcher,
    pub limiter: &'a Limiter,
    pub rules: &'a RobotsRules,
    pub scope: &'a SiteScope,
    /// At most this many requests' worth of addresses (`CrawlLimits::outbound_checks`).
    pub max_checks: u32,
    pub deadline: Instant,
}

/// Whether `status` from a host means the address is gone. `own` is the crawled site's host.
fn is_dead(status: u16, own: bool) -> bool {
    match status {
        404 | 410 => true,
        401 | 403 | 429 => false,
        400..=599 => own,
        _ => false,
    }
}

/// The status to report when the address is gone, `None` when it is fine or cannot be told.
async fn probe(fetcher: &Fetcher, url: &Url, own: bool, until: Instant) -> Option<u16> {
    for attempt in 0..2 {
        let limit = until.min(Instant::now() + REQUEST_TIMEOUT);
        match timeout_at(limit, fetcher.fetch_raw(url, 0)).await {
            Ok(Ok(res)) => return is_dead(res.status, own).then_some(res.status),
            // No such host, or nothing listening: once more, in case it was a blip.
            Ok(Err(FetchError::Connect(_))) if attempt == 0 => {}
            Ok(Err(FetchError::Connect(_))) => return Some(0),
            _ => return None,
        }
    }
    None
}

/// Requests each tracked address once and reports the dead ones.
pub(crate) async fn check(targets: Targets, c: Check<'_>) -> OutboundReport {
    let Targets { list, dropped, .. } = targets;
    let mut report = OutboundReport {
        unchecked: dropped,
        ..OutboundReport::default()
    };
    if c.max_checks == 0 || list.is_empty() {
        report.unchecked = report
            .unchecked
            .saturating_add(u32::try_from(list.len()).unwrap_or(u32::MAX));
        if c.max_checks == 0 {
            // Switched off: say nothing rather than "0 checked".
            return OutboundReport::default();
        }
        return report;
    }

    let until = c.deadline.min(Instant::now() + BUDGET);
    let mut hosts: HashMap<String, Arc<Semaphore>> = HashMap::new();
    let mut jobs = Vec::new();
    for t in list {
        let own = c.scope.is_internal(&t.url);
        if (own && !c.rules.allowed(t.url.as_str())) || jobs.len() >= c.max_checks as usize {
            report.unchecked = report.unchecked.saturating_add(1);
            continue;
        }
        let gate = (!own).then(|| {
            let host = t.url.host_str().unwrap_or("").to_ascii_lowercase();
            Arc::clone(
                hosts
                    .entry(host)
                    .or_insert_with(|| Arc::new(Semaphore::new(PER_HOST))),
            )
        });
        jobs.push((t, own, gate));
    }

    let fetcher = c.fetcher;
    let limiter = c.limiter;
    let mut results = stream::iter(jobs.into_iter().map(|(t, own, gate)| async move {
        if Instant::now() >= until {
            return (t, None, false);
        }
        let dead = match gate {
            // The site's own host: the crawl's rate limit and connection cap.
            None => {
                let _permit = limiter.acquire().await;
                probe(fetcher, &t.url, own, until).await
            }
            Some(gate) => {
                let _permit = gate.acquire().await;
                probe(fetcher, &t.url, own, until).await
            }
        };
        (t, dead, true)
    }))
    .buffer_unordered(CONCURRENCY);

    while let Some((t, dead, asked)) = results.next().await {
        if !asked {
            report.unchecked = report.unchecked.saturating_add(1);
            continue;
        }
        report.checked += 1;
        if let Some(status) = dead {
            report.broken.push(BrokenTarget {
                url: t.url,
                kind: t.kind,
                status,
                from: t.from,
            });
        }
    }
    // Completion order is not stable; the report is.
    report
        .broken
        .sort_by(|a, b| a.url.as_str().cmp(b.url.as_str()));
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn only_plain_gone_answers_count_on_other_hosts() {
        for status in [404, 410] {
            assert!(is_dead(status, false), "{status}");
            assert!(is_dead(status, true), "{status}");
        }
        // Bot walls and server trouble on someone else's site prove nothing.
        for status in [200, 301, 400, 401, 403, 429, 500, 503, 999] {
            assert!(!is_dead(status, false), "{status}");
        }
        // On the site's own host any error is the site's to fix, except the access answers.
        for status in [400, 405, 500, 503] {
            assert!(is_dead(status, true), "{status}");
        }
        for status in [200, 304, 401, 403, 429] {
            assert!(!is_dead(status, true), "{status}");
        }
    }

    #[test]
    fn an_address_is_tracked_once_with_every_page_that_uses_it() {
        let mut t = Targets::default();
        t.note(&u("https://cdn.e.com/a.png"), OutboundKind::Image, 0);
        t.note(&u("https://cdn.e.com/a.png"), OutboundKind::Image, 0);
        t.note(&u("https://cdn.e.com/a.png"), OutboundKind::Link, 3);
        t.note(&u("https://other.org/"), OutboundKind::Link, 3);
        assert_eq!(t.list.len(), 2);
        assert_eq!(t.list[0].from, vec![0, 3]);
        assert_eq!(
            t.list[0].kind,
            OutboundKind::Image,
            "the first kind is kept"
        );
        assert_eq!(t.list[1].from, vec![3]);
    }
}
