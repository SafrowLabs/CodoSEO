//! The crawl orchestrator: preflight, then a bounded, polite crawl one depth level at a
//! time, ending in a [`CrawlOutput`].
//!
//! Every request goes through one [`Limiter`]. A level starts only when every fetch of
//! the previous one has finished, so depth is exact even with parallel connections.
//! Pages found only in sitemaps come after link exploration, with no depth.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use codoseo_core::crawl::{CrawlConfig, RobotsFile, SitemapSummary};
use codoseo_core::output::{CrawlOutput, Edge, LinkGraph, Progress, SiteSignals, StopReason};
use codoseo_core::page::PageRecord;
use codoseo_core::url::{normalize, url_hash};
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use tokio::sync::Semaphore;
use tokio::time::{Instant, sleep_until, timeout_at};
use url::Url;

use crate::fetch::{FetchError, FetchResult, Fetcher, FetcherConfig};
use crate::frontier::{Frontier, Queued};
use crate::politeness::{Limiter, retry_after_of};
use crate::preflight::{BLOCKED_MSG, LOGIN_MSG, Preflight, fetch_tdmrep, origin_of, preflight};
use crate::record::{self, Built};
use crate::robots::RobotsRules;
use crate::scope::SiteScope;

/// This many 401/403/429 responses in a row stop the crawl as `Blocked`.
const MAX_BLOCKED_STREAK: u32 = 10;

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

/// Crawls `cfg.start_url` with its own in-flight cap of `politeness.max_in_flight`.
pub async fn crawl(
    cfg: CrawlConfig,
    on_progress: impl Fn(Progress) + Send + Sync,
) -> Result<CrawlOutput, CrawlError> {
    let global = Arc::new(Semaphore::new(cfg.politeness.max_in_flight.max(1) as usize));
    crawl_shared(cfg, global, on_progress).await
}

/// Crawls `cfg.start_url`, taking a slot of `global` for every request, so several
/// crawls can share one in-flight cap. `on_progress` is called after each recorded page.
pub async fn crawl_shared(
    cfg: CrawlConfig,
    global: Arc<Semaphore>,
    on_progress: impl Fn(Progress) + Send + Sync,
) -> Result<CrawlOutput, CrawlError> {
    let started = Instant::now();
    let start = valid_start(&cfg)?;
    let fetcher = fetcher_for(&cfg)?;
    let deadline = started + cfg.limits.max_duration;
    let limiter = Limiter::new(&cfg.politeness, None, global);

    let Ok(pre) = timeout_at(deadline, preflight(&cfg, &fetcher, &limiter, deadline)).await else {
        return Ok(CrawlOutput {
            origin: origin_of(&start),
            pages: Vec::new(),
            links: LinkGraph::default(),
            robots: None,
            sitemap: SitemapSummary::default(),
            stop: StopReason::TimeLimit,
            duration_ms: elapsed_ms(started),
            signals: SiteSignals::default(),
        });
    };
    let Preflight {
        origin,
        start,
        rules,
        robots,
        sitemap_urls,
        sitemap,
        mut signals,
        stop,
    } = pre?;
    limiter.set_crawl_delay(rules.crawl_delay());
    // After the crawl delay, so even this one extra request keeps to it.
    if stop.is_none() && cfg.site_signals {
        signals.tdmrep = timeout_at(deadline, fetch_tdmrep(&fetcher, &limiter, &origin, &rules))
            .await
            .ok()
            .flatten();
    }

    let mut run = Run::new(&cfg, &origin, rules, &sitemap_urls, started);
    run.stop = stop;
    if let Some((url, result)) = start {
        run.record_start(url, result);
        on_progress(run.progress(0));
    }
    if run.stop.is_none() {
        run.crawl(&fetcher, &limiter, sitemap_urls, deadline, &on_progress)
            .await;
    }
    Ok(run.finish(origin, robots, sitemap, signals))
}

/// Fetches `cfg.start_url` once, without reading robots.txt, and returns the record of
/// the page it settles on, with every redirect hop in `redirect_chain`.
pub async fn inspect_page(cfg: &CrawlConfig) -> Result<PageRecord, CrawlError> {
    let url = valid_start(cfg)?;
    let fetcher = fetcher_for(cfg)?;
    let mut res = match fetcher.fetch(&url).await {
        Ok(res) => res,
        Err(FetchError::Blocked(msg)) => return Err(CrawlError::AddressBlocked(msg)),
        Err(e) => return Ok(record::from_error(&url, Some(0), false, &e)),
    };
    let chain = std::mem::take(&mut res.chain);
    let final_url =
        normalize(&res.final_url, res.final_url.as_str()).unwrap_or_else(|| res.final_url.clone());
    let scope = SiteScope::new(&final_url);
    // Without hops of its own, the response gives exactly one record.
    let built = record::from_fetch(&final_url, Some(0), |_| false, &res, &scope, |_| false);
    let mut rec = built
        .into_iter()
        .next()
        .expect("a final response always gives a record")
        .record;
    if !chain.is_empty() {
        rec.redirect_chain = chain.into_iter().map(|h| (h.status, h.url)).collect();
        rec.redirect_target = Some(final_url);
        rec.key_hash = rec.compute_key_hash();
    }
    Ok(rec)
}

/// Runs only the preflight step with a fetcher and limiter built from `cfg`.
#[doc(hidden)]
pub async fn preflight_for_tests(cfg: CrawlConfig) -> Result<Preflight, CrawlError> {
    let fetcher = fetcher_for(&cfg)?;
    let limiter = Limiter::new(&cfg.politeness, None, Arc::new(Semaphore::new(64)));
    let deadline = Instant::now() + cfg.limits.max_duration.max(Duration::from_secs(1));
    preflight(&cfg, &fetcher, &limiter, deadline).await
}

fn valid_start(cfg: &CrawlConfig) -> Result<Url, CrawlError> {
    normalize(&cfg.start_url, cfg.start_url.as_str())
        .ok_or_else(|| CrawlError::InvalidStart(cfg.start_url.to_string()))
}

fn fetcher_for(cfg: &CrawlConfig) -> Result<Fetcher, CrawlError> {
    let mut fetcher_cfg = FetcherConfig::new(cfg.address_policy);
    fetcher_cfg.user_agent = cfg.user_agent.clone();
    fetcher_cfg.request_timeout = cfg.limits.request_timeout;
    fetcher_cfg.connect_timeout = fetcher_cfg.connect_timeout.min(cfg.limits.request_timeout);
    fetcher_cfg.max_redirects = cfg.limits.max_redirects;
    fetcher_cfg.max_body_bytes = cfg.limits.max_page_bytes;
    Fetcher::new(fetcher_cfg).map_err(|e| CrawlError::Client(e.to_string()))
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// One page fetch: wait for the limiter, then fetch within `limit` and the deadline.
async fn fetch_one(
    fetcher: &Fetcher,
    limiter: &Limiter,
    q: Queued,
    deadline: Instant,
    limit: Duration,
) -> (Queued, Result<FetchResult, FetchError>) {
    let _permit = limiter.acquire().await;
    let until = deadline.min(Instant::now() + limit);
    let res = timeout_at(until, fetcher.fetch(&q.url))
        .await
        .unwrap_or(Err(FetchError::Timeout));
    (q, res)
}

/// A link between two pages, kept by target hash until every page has its index.
struct PendingEdge {
    from: u32,
    to: u64,
    anchor: u32,
    nofollow: bool,
}

/// The state of one crawl after preflight.
struct Run {
    scope: SiteScope,
    rules: RobotsRules,
    frontier: Frontier,
    in_sitemap: HashSet<u64>,
    pages: Vec<PageRecord>,
    edges: Vec<PendingEdge>,
    anchors: HashMap<String, u32>,
    /// URLs already retried once after a 429 or 503.
    retried: HashSet<u64>,
    connections: usize,
    fetch_timeout: Duration,
    max_failures: u32,
    consecutive_failures: u32,
    failures: u32,
    last_error: String,
    blocked_streak: u32,
    /// The status that last extended the blocked streak.
    blocked_status: u16,
    /// The deepest link level recorded so far, for progress.
    depth: u16,
    started: Instant,
    stop: Option<StopReason>,
}

impl Run {
    fn new(
        cfg: &CrawlConfig,
        origin: &Url,
        rules: RobotsRules,
        sitemap_urls: &[Url],
        started: Instant,
    ) -> Run {
        Run {
            scope: SiteScope::new(origin),
            rules,
            frontier: Frontier::new(cfg.limits.max_pages),
            in_sitemap: sitemap_urls.iter().map(url_hash).collect(),
            pages: Vec::new(),
            edges: Vec::new(),
            anchors: HashMap::new(),
            retried: HashSet::new(),
            connections: cfg.politeness.per_site_connections.max(1) as usize,
            fetch_timeout: cfg.limits.request_timeout.saturating_mul(2),
            max_failures: cfg.politeness.max_consecutive_failures.max(1),
            consecutive_failures: 0,
            failures: 0,
            last_error: String::new(),
            blocked_streak: 0,
            blocked_status: 0,
            depth: 0,
            started,
            stop: None,
        }
    }

    /// Records the start page from preflight's fetch (already retried there). The
    /// requested URL is seeded so it counts toward the page limit.
    fn record_start(&mut self, url: Url, result: Result<FetchResult, FetchError>) {
        self.frontier.seed(url.clone());
        self.frontier.pop();
        self.record_result(
            Queued {
                url,
                depth: Some(0),
            },
            result,
        );
        self.check_stops();
    }

    async fn crawl(
        &mut self,
        fetcher: &Fetcher,
        limiter: &Limiter,
        sitemap_urls: Vec<Url>,
        deadline: Instant,
        on_progress: &(impl Fn(Progress) + Send + Sync),
    ) {
        let mut in_flight = FuturesUnordered::new();
        // Sitemap-only URLs join once link exploration is over, so linked pages keep
        // their depth and the page limit goes to them first.
        let mut sitemap_urls = Some(sitemap_urls);
        while self.stop.is_none() {
            while in_flight.len() < self.connections {
                let Some(q) = self.frontier.pop() else { break };
                if self.rules.allowed(q.url.as_str()) {
                    in_flight.push(fetch_one(fetcher, limiter, q, deadline, self.fetch_timeout));
                } else {
                    let in_sitemap = self.is_in_sitemap(&q.url);
                    self.pages
                        .push(record::robots_blocked(&q.url, q.depth, in_sitemap));
                    on_progress(self.progress(in_flight.len()));
                }
            }
            if in_flight.is_empty() {
                // The level barrier: nothing of the current level is left or in flight.
                if self.frontier.advance() {
                    continue;
                }
                if let Some(urls) = sitemap_urls.take() {
                    let scope = &self.scope;
                    self.frontier
                        .add_sitemap_urls(urls.into_iter().filter(|u| scope.is_internal(u)));
                    if self.frontier.advance() {
                        continue;
                    }
                }
                break;
            }
            // The deadline goes first: a fetch cut short by it ends on the same tick and
            // must not be recorded as a timeout.
            tokio::select! {
                biased;
                () = sleep_until(deadline) => self.stop = Some(StopReason::TimeLimit),
                Some((q, res)) = in_flight.next() => {
                    if Instant::now() >= deadline {
                        // Too late to count, and no new work may start.
                        self.stop = Some(StopReason::TimeLimit);
                    } else if self.on_result(limiter, q, res) {
                        on_progress(self.progress(in_flight.len()));
                    }
                }
            }
        }
    }

    /// Handles one finished fetch. False when nothing was recorded (a first 429 or 503,
    /// queued again for one retry).
    fn on_result(
        &mut self,
        limiter: &Limiter,
        q: Queued,
        res: Result<FetchResult, FetchError>,
    ) -> bool {
        if let Ok(r) = &res {
            // Counted before the retry, so a blocking site stops us after 10 responses.
            if matches!(r.status, 401 | 403 | 429) {
                self.blocked_streak += 1;
                self.blocked_status = r.status;
            } else {
                self.blocked_streak = 0;
            }
            limiter.on_response(r.status, retry_after_of(&r.headers));
            if matches!(r.status, 429 | 503) && self.retried.insert(url_hash(&q.url)) {
                self.frontier.requeue_front(q);
                self.check_stops();
                return false;
            }
        }
        self.record_result(q, res);
        self.check_stops();
        true
    }

    /// Builds the records of one result and updates the failure counter.
    fn record_result(&mut self, q: Queued, res: Result<FetchResult, FetchError>) {
        match res {
            Ok(r) => {
                if r.status >= 500 {
                    self.fail(format!("server returned HTTP {}", r.status));
                } else {
                    self.consecutive_failures = 0;
                }
                let in_sitemap = &self.in_sitemap;
                let rules = &self.rules;
                let frontier = &mut self.frontier;
                // An internal redirect target costs no fetch, so it is admitted past the
                // page limit; one robots.txt disallows is not recorded as a fetched page.
                let built = record::from_fetch(
                    &q.url,
                    q.depth,
                    |u| in_sitemap.contains(&url_hash(u)),
                    &r,
                    &self.scope,
                    |u| rules.allowed(u.as_str()) && frontier.admit_redirect_target(u),
                );
                for b in built {
                    self.add(b);
                }
            }
            Err(e) => {
                // Redirect and other errors are recorded but say nothing about reachability.
                if matches!(e, FetchError::Timeout | FetchError::Connect(_)) {
                    self.fail(e.to_string());
                }
                let in_sitemap = self.is_in_sitemap(&q.url);
                self.pages
                    .push(record::from_error(&q.url, q.depth, in_sitemap, &e));
            }
        }
    }

    /// Adds a record, queues its followed internal links and keeps its edges to pages
    /// the frontier knows. Pages with no depth (sitemap-only) are not followed.
    ///
    /// A page that links the same URL several times gets one edge: it keeps the first
    /// anchor text, and is `nofollow` only when every one of those links is.
    fn add(&mut self, b: Built) {
        let from = u32::try_from(self.pages.len()).unwrap_or(u32::MAX);
        let depth = b.record.depth;
        if let Some(d) = depth {
            self.depth = self.depth.max(d);
        }
        if b.follow_links
            && let Some(d) = depth
        {
            for link in &b.links {
                if !link.nofollow && self.scope.is_internal(&link.url) {
                    self.frontier.push_link(link.url.clone(), d);
                }
            }
        }
        let mut by_target: HashMap<u64, usize> = HashMap::new();
        for link in b.links {
            if !self.scope.is_internal(&link.url) || !self.frontier.is_seen(&link.url) {
                continue;
            }
            let to = url_hash(&link.url);
            if let Some(&i) = by_target.get(&to) {
                self.edges[i].nofollow &= link.nofollow;
                continue;
            }
            by_target.insert(to, self.edges.len());
            let anchor = self.intern(link.anchor);
            self.edges.push(PendingEdge {
                from,
                to,
                anchor,
                nofollow: link.nofollow,
            });
        }
        self.pages.push(b.record);
    }

    fn intern(&mut self, text: String) -> u32 {
        let next = u32::try_from(self.anchors.len()).unwrap_or(u32::MAX);
        *self.anchors.entry(text).or_insert(next)
    }

    fn fail(&mut self, error: String) {
        self.consecutive_failures += 1;
        self.failures += 1;
        self.last_error = error;
    }

    fn check_stops(&mut self) {
        if self.stop.is_some() {
            return;
        }
        if self.consecutive_failures >= self.max_failures {
            self.stop = Some(StopReason::Unreachable(self.last_error.clone()));
        } else if self.blocked_streak >= MAX_BLOCKED_STREAK {
            let msg = if self.blocked_status == 401 {
                LOGIN_MSG
            } else {
                BLOCKED_MSG
            };
            self.stop = Some(StopReason::Blocked(msg.to_owned()));
        }
    }

    fn is_in_sitemap(&self, url: &Url) -> bool {
        self.in_sitemap.contains(&url_hash(url))
    }

    fn progress(&self, in_flight: usize) -> Progress {
        Progress {
            pages_done: u32::try_from(self.pages.len()).unwrap_or(u32::MAX),
            queued: u32::try_from(self.frontier.queued() + in_flight).unwrap_or(u32::MAX),
            failures: self.failures,
            depth: self.depth,
            elapsed_ms: elapsed_ms(self.started),
        }
    }

    /// Settles the stop reason and turns pending edges into page indices, dropping
    /// targets that were never recorded and self-links.
    fn finish(
        self,
        origin: Url,
        robots: Option<RobotsFile>,
        sitemap: SitemapSummary,
        signals: SiteSignals,
    ) -> CrawlOutput {
        let stop = match self.stop {
            Some(stop) => stop,
            None if self.frontier.capped() => StopReason::PageLimit,
            None => StopReason::Completed,
        };
        let index: HashMap<u64, u32> = self
            .pages
            .iter()
            .enumerate()
            .map(|(i, p)| (p.url_hash, u32::try_from(i).unwrap_or(u32::MAX)))
            .collect();
        let edges = self
            .edges
            .into_iter()
            .filter_map(|e| {
                let to = *index.get(&e.to)?;
                (to != e.from).then_some(Edge {
                    from: e.from,
                    to,
                    anchor: e.anchor,
                    nofollow: e.nofollow,
                })
            })
            .collect();
        let mut anchors = vec![String::new(); self.anchors.len()];
        for (text, id) in self.anchors {
            anchors[id as usize] = text;
        }
        CrawlOutput {
            origin,
            pages: self.pages,
            links: LinkGraph { edges, anchors },
            robots,
            sitemap,
            stop,
            duration_ms: elapsed_ms(self.started),
            signals,
        }
    }
}
