//! Synthetic AI-agent fetch probes, the network half: fetches each planned URL once with
//! CodoSEObot's user agent (the baseline) and once per probed bot with that bot's documented user
//! agent, and records the verdicts in a [`ProbeReport`]. What to probe and how to judge a response
//! live in [`codoseo_geo::probe`].
//!
//! Every request carries the same headers apart from `User-Agent`: the client defaults plus
//! `From`. Requests go one at a time through one [`Limiter`], at least `min_gap` apart or the
//! robots.txt `Crawl-delay` (capped at 10 s) when that is longer. The budget and the request cap
//! are checked before each URL, which is started only when both cover all of its requests, and
//! retries and confirmations only use what is left once the URL's planned requests are kept
//! aside. A hard deadline past the budget stops the run even inside a URL, keeping what finished.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use codoseo_core::check::IssueBits;
use codoseo_core::crawl::{AddressPolicy, PROBE_FROM, Politeness, SitemapSummary, USER_AGENT};
use codoseo_core::output::{CrawlOutput, LinkGraph, SiteSignals, StopReason};
use codoseo_core::page::{Indexability, PageFields, PageRecord};
use codoseo_core::url::url_hash;
use codoseo_geo::probe::{
    FetchErrorKind, InconclusiveReason, MAX_PROBE_BODY, ProbeBot, ProbeFetch, ProbePlan,
    ProbeReport, ProbeResult, ProbeStop, ProbedUrl, SkipReason, Verdict, candidates, cap_url,
    classify, detect_challenge, media_type, page_title, plan_probes, robots_skips,
};
use codoseo_geo::report::{AccessReport, build_report, important_urls};
use codoseo_geo::robots::RobotsTxt;
use reqwest::header::{FROM, HeaderMap, HeaderValue, USER_AGENT as USER_AGENT_HEADER};
use tokio::sync::Semaphore;
use tokio::time::{Instant, sleep};
use url::Url;

use crate::fetch::{FetchError, FetchResult, Fetcher, FetcherConfig, is_html};
use crate::politeness::{Limiter, retry_after_of};
use crate::robots::fetch_robots;

/// The shortest gap between two probe requests.
pub const DEFAULT_MIN_GAP: Duration = Duration::from_millis(500);
/// The longest `Crawl-delay` and `Retry-After` the probes wait for.
pub const MAX_PROBE_WAIT: Duration = Duration::from_secs(10);
/// The wait before a retry when the site gave no `Retry-After`.
pub const DEFAULT_RETRY_WAIT: Duration = Duration::from_secs(2);
/// Per-request timeout: short, as a probe run has a budget of a minute or two.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long past its budget [`ProbeConfig::new`] lets a run go before its deadline. The budget is
/// checked between URLs only, so the last URL's requests (each with its own timeout and maybe a
/// retry) can run over.
pub const MAX_OVERRUN: Duration = Duration::from_secs(60);

/// One probe run.
#[derive(Debug, Clone)]
pub struct ProbeConfig {
    /// The URL of each planned index into `AccessReport::important`.
    pub origin_urls: Vec<(u16, Url)>,
    pub plan: ProbePlan,
    /// (URL index, token) pairs robots.txt disallows: never requested, reported as
    /// [`Verdict::SkippedRobots`].
    pub disallowed: Vec<(u16, String)>,
    /// The `From` header on every request; [`PROBE_FROM`] when it is not a valid header value.
    pub from: String,
    pub address_policy: AddressPolicy,
    /// Wall time for the run; a URL is started only when the rest covers its requests at the
    /// current gap or the site's average answer time, whichever is longer.
    pub budget: Duration,
    /// Wall time after which the run stops even inside a URL: the request under way is dropped,
    /// the bots of that URL still unanswered are [`InconclusiveReason::Stopped`], and the URLs
    /// that finished are kept. `stop` is then [`ProbeStop::Budget`].
    pub deadline: Duration,
    /// Requests for the whole run, retries and confirmations included.
    pub max_requests: u16,
    pub min_gap: Duration,
    pub request_timeout: Duration,
}

impl ProbeConfig {
    /// A run of `plan` for the pages of `report`, skipping what its robots verdicts disallow, with
    /// the default `From`, gap and timeout.
    pub fn new(
        report: &AccessReport,
        plan: ProbePlan,
        address_policy: AddressPolicy,
        budget: Duration,
        max_requests: u16,
    ) -> ProbeConfig {
        let origin_urls = plan
            .urls
            .iter()
            .filter_map(|&i| {
                let url = report.important.get(usize::from(i))?;
                Some((i, Url::parse(&url.url).ok()?))
            })
            .collect();
        let disallowed = plan
            .urls
            .iter()
            .flat_map(|&i| {
                plan.bots
                    .iter()
                    .filter(move |b| robots_skips(report, i, &b.token))
                    .map(move |b| (i, b.token.clone()))
            })
            .collect();
        ProbeConfig {
            origin_urls,
            plan,
            disallowed,
            from: PROBE_FROM.to_owned(),
            address_policy,
            budget,
            deadline: budget + MAX_OVERRUN,
            max_requests,
            min_gap: DEFAULT_MIN_GAP,
            request_timeout: PROBE_TIMEOUT,
        }
    }
}

/// Runs the probes of `cfg`. Never fails: a URL that can't be fetched gets inconclusive verdicts,
/// and a run cut short says why in `stop`. Bodies are read up to [`MAX_PROBE_BODY`] and dropped
/// once their title, size and challenge markers are taken.
pub async fn run_probes(cfg: ProbeConfig) -> ProbeReport {
    run_probes_watched(cfg, |_, _| {}).await
}

/// [`run_probes`], with each URL passed to `on_url` as soon as every bot has its verdict there,
/// so a caller can show the results as they come.
pub async fn run_probes_watched(
    cfg: ProbeConfig,
    on_url: impl Fn(&Url, &ProbedUrl) + Send + Sync,
) -> ProbeReport {
    let from = valid_from(&cfg.from)
        .and_then(|f| HeaderValue::from_str(&f).ok())
        .unwrap_or_else(|| HeaderValue::from_static(PROBE_FROM));
    // The value actually sent.
    let mut report = ProbeReport::new(from.to_str().unwrap_or(PROBE_FROM), &cfg.plan);
    let fetcher = match Fetcher::new(FetcherConfig {
        request_timeout: cfg.request_timeout,
        connect_timeout: cfg.request_timeout.min(Duration::from_secs(10)),
        max_body_bytes: MAX_PROBE_BODY,
        ..FetcherConfig::new(cfg.address_policy)
    }) {
        Ok(f) => f,
        Err(_) => return report,
    };
    let start = Instant::now();
    let mut run = Run {
        fetcher,
        limiter: limiter(cfg.min_gap, cfg.plan.crawl_delay),
        from,
        deadline: start + cfg.budget,
        hard_deadline: start + cfg.deadline,
        max_requests: cfg.max_requests,
        requests: 0,
        answered: 0,
        answer_time: Duration::ZERO,
    };
    let disallowed: HashSet<(u16, &str)> = cfg
        .disallowed
        .iter()
        .map(|(i, t)| (*i, t.as_str()))
        .collect();
    // (token, cause) pairs already confirmed by a re-fetch.
    let mut confirmed: HashSet<(String, &'static str)> = HashSet::new();

    for &idx in &cfg.plan.urls {
        let Some((_, url)) = cfg.origin_urls.iter().find(|(i, _)| *i == idx) else {
            continue;
        };
        let wanted: Vec<&ProbeBot> = cfg
            .plan
            .bots
            .iter()
            .filter(|b| !disallowed.contains(&(idx, b.token.as_str())))
            .collect();
        let planned = 1 + wanted.len() as u16;
        if u32::from(run.requests) + u32::from(planned) > u32::from(run.max_requests) {
            report.stop = Some(ProbeStop::RequestCap);
            break;
        }
        let need = run.per_request().saturating_mul(u32::from(planned));
        if run.deadline.saturating_duration_since(Instant::now()) < need {
            report.stop = Some(ProbeStop::Budget);
            break;
        }

        let mut left = wanted.len() as u16;
        let Some(baseline) = run.fetch_by_deadline(url, USER_AGENT, left).await else {
            report.stop = Some(ProbeStop::Budget);
            break;
        };
        // The baseline against itself: inconclusive exactly when it can't serve as a baseline.
        let base_verdict = classify(url.as_str(), &baseline, &baseline);
        let usable = !matches!(base_verdict, Verdict::Inconclusive { .. });

        let mut results = Vec::with_capacity(cfg.plan.bots.len());
        let mut stopped = false;
        for bot in &cfg.plan.bots {
            if disallowed.contains(&(idx, bot.token.as_str())) {
                results.push(ProbeResult {
                    token: bot.token.clone(),
                    fetch: None,
                    verdict: Verdict::SkippedRobots,
                });
                continue;
            }
            left -= 1;
            if !usable {
                results.push(ProbeResult {
                    token: bot.token.clone(),
                    fetch: None,
                    verdict: base_verdict.clone(),
                });
                continue;
            }
            let first = if stopped {
                None
            } else {
                run.fetch_by_deadline(url, &bot.user_agent, left).await
            };
            let Some(mut fetch) = first else {
                stopped = true;
                results.push(ProbeResult {
                    token: bot.token.clone(),
                    fetch: None,
                    verdict: Verdict::Inconclusive {
                        reason: InconclusiveReason::Stopped,
                    },
                });
                continue;
            };
            let mut verdict = classify(url.as_str(), &baseline, &fetch);
            if let Some(cause) = verdict.cause_slug()
                && !confirmed.contains(&(bot.token.clone(), cause))
            {
                if !run.can_extra(left) {
                    // No room for the second fetch a finding needs.
                    results.push(ProbeResult {
                        token: bot.token.clone(),
                        fetch: Some(fetch),
                        verdict: Verdict::Inconclusive {
                            reason: InconclusiveReason::Unconfirmed,
                        },
                    });
                    continue;
                }
                let Some(second) = run.fetch_by_deadline(url, &bot.user_agent, left).await else {
                    // The deadline came during the second fetch a finding needs.
                    stopped = true;
                    results.push(ProbeResult {
                        token: bot.token.clone(),
                        fetch: Some(fetch),
                        verdict: Verdict::Inconclusive {
                            reason: InconclusiveReason::Unconfirmed,
                        },
                    });
                    continue;
                };
                fetch = second;
                let again = classify(url.as_str(), &baseline, &fetch);
                if again.cause_slug() == Some(cause) {
                    confirmed.insert((bot.token.clone(), cause));
                    verdict = again;
                } else {
                    verdict = Verdict::Inconclusive {
                        reason: InconclusiveReason::Unstable,
                    };
                }
            }
            results.push(ProbeResult {
                token: bot.token.clone(),
                fetch: Some(fetch),
                verdict,
            });
        }
        let probed = ProbedUrl {
            url: idx,
            baseline,
            results,
        };
        on_url(url, &probed);
        report.urls.push(probed);
        if stopped {
            report.stop = Some(ProbeStop::Budget);
            break;
        }
    }
    report.requests = run.requests;
    report
}

/// One limiter for the run: a single connection, the gap the larger of `min_gap` and the crawl
/// delay, capped at [`MAX_PROBE_WAIT`].
fn limiter(min_gap: Duration, crawl_delay: Option<f64>) -> Limiter {
    let requests_per_sec = if min_gap.is_zero() {
        1000.0
    } else {
        (1.0 / min_gap.as_secs_f64()) as f32
    };
    let politeness = Politeness {
        requests_per_sec,
        per_site_connections: 1,
        max_crawl_delay: MAX_PROBE_WAIT,
        ..Politeness::default()
    };
    let crawl_delay = crawl_delay
        .filter(|d| d.is_finite() && *d > 0.0)
        .map(|d| Duration::from_secs_f64(d.min(MAX_PROBE_WAIT.as_secs_f64())));
    Limiter::new(&politeness, crawl_delay, Arc::new(Semaphore::new(1)))
}

struct Run {
    fetcher: Fetcher,
    limiter: Limiter,
    from: HeaderValue,
    deadline: Instant,
    hard_deadline: Instant,
    max_requests: u16,
    requests: u16,
    /// Requests that got an answer or an error, and the time they took, for the budget.
    answered: u32,
    answer_time: Duration,
}

impl Run {
    /// What one request costs: the gap, or the site's average answer time when it is slower.
    fn per_request(&self) -> Duration {
        let gap = self.limiter.interval();
        match self.answered {
            0 => gap,
            n => gap.max(self.answer_time / n),
        }
    }

    /// [`Run::fetch`] unless the hard deadline comes first; `None` when it does.
    async fn fetch_by_deadline(
        &mut self,
        url: &Url,
        user_agent: &str,
        reserved: u16,
    ) -> Option<ProbeFetch> {
        let deadline = self.hard_deadline;
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::timeout_at(deadline, self.fetch(url, user_agent, reserved))
            .await
            .ok()
    }

    /// Room for one request beyond the plan, with `reserved` planned requests of this URL still
    /// to come, and time left.
    fn can_extra(&self, reserved: u16) -> bool {
        u32::from(self.requests) + 1 + u32::from(reserved) <= u32::from(self.max_requests)
            && Instant::now() < self.deadline
    }

    /// Fetches `url` as `user_agent`, retrying once after a 429, a 503, a timeout or a connection
    /// error when the cap and the budget allow it.
    async fn fetch(&mut self, url: &Url, user_agent: &str, reserved: u16) -> ProbeFetch {
        let Ok(ua) = HeaderValue::from_str(user_agent) else {
            return ProbeFetch {
                error: Some(FetchErrorKind::Other),
                ..ProbeFetch::default()
            };
        };
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT_HEADER, ua);
        headers.insert(FROM, self.from.clone());

        let first = self.send(url, &headers).await;
        if let Some(wait) = retry_wait(&first)
            && self.can_extra(reserved)
            && Instant::now() + wait < self.deadline
        {
            sleep(wait).await;
            let second = self.send(url, &headers).await;
            return probe_fetch(url, second, true);
        }
        probe_fetch(url, first, false)
    }

    async fn send(&mut self, url: &Url, headers: &HeaderMap) -> Result<FetchResult, FetchError> {
        let _permit = self.limiter.acquire().await;
        self.requests = self.requests.saturating_add(1);
        let sent = Instant::now();
        let result = self.fetcher.fetch_with(url, headers, MAX_PROBE_BODY).await;
        self.answered += 1;
        self.answer_time += sent.elapsed();
        result
    }
}

/// How long to wait before retrying, or `None` when the attempt is not worth retrying.
fn retry_wait(attempt: &Result<FetchResult, FetchError>) -> Option<Duration> {
    match attempt {
        Ok(r) if r.status == 429 || r.status == 503 => Some(
            retry_after_of(&r.headers)
                .unwrap_or(DEFAULT_RETRY_WAIT)
                .min(MAX_PROBE_WAIT),
        ),
        Ok(_) => None,
        Err(FetchError::Timeout | FetchError::Connect(_) | FetchError::Http(_)) => {
            Some(DEFAULT_RETRY_WAIT)
        }
        Err(_) => None,
    }
}

/// What the report keeps of one fetch; the body is dropped here.
fn probe_fetch(
    requested: &Url,
    attempt: Result<FetchResult, FetchError>,
    retried: bool,
) -> ProbeFetch {
    let r = match attempt {
        Ok(r) => r,
        Err(e) => {
            let (error, redirects) = match e {
                FetchError::Timeout => (FetchErrorKind::Timeout, 0),
                FetchError::Connect(_) => (FetchErrorKind::Connect, 0),
                // A request that failed after connecting, before a full response: the connection
                // was reset or closed.
                FetchError::Http(_) => (FetchErrorKind::Dropped, 0),
                FetchError::TooManyRedirects { chain } | FetchError::RedirectLoop { chain } => {
                    (FetchErrorKind::TooManyRedirects, chain.len())
                }
                FetchError::Blocked(_) => (FetchErrorKind::Blocked, 0),
                FetchError::InvalidRedirect { .. } | FetchError::Client(_) => {
                    (FetchErrorKind::Other, 0)
                }
            };
            return ProbeFetch {
                error: Some(error),
                redirects: redirects.min(usize::from(u8::MAX)) as u8,
                retried,
                ..ProbeFetch::default()
            };
        }
    };
    let body = r.body.as_deref().unwrap_or_default();
    let headers: Vec<(String, String)> = r
        .headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_owned(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    ProbeFetch {
        status: Some(r.status),
        error: None,
        final_url: (r.final_url != *requested).then(|| cap_url(r.final_url.as_str())),
        redirects: r.chain.len().min(usize::from(u8::MAX)) as u8,
        bytes: body.len().min(u32::MAX as usize) as u32,
        truncated: r.truncated,
        html: is_html(r.content_type.as_deref()),
        content_type: r.content_type.as_deref().and_then(media_type),
        title: page_title(body),
        challenge: detect_challenge(r.status, &headers, body),
        retried,
    }
}

// ---- One page on its own --------------------------------------------------------------------

/// The overall time limit of a single-page run (`codoseo probe`, the local MCP's
/// `probe_ai_fetch`), robots.txt included: past it the run stops and returns what it has, with
/// `stop` set to `budget`.
pub const PAGE_PROBE_DEADLINE: Duration = Duration::from_secs(120);
/// The request cap of a single-page run: the self-hosted cap, far more than one page needs.
pub const PAGE_PROBE_MAX_REQUESTS: u16 = 160;
/// The environment variable that overrides the `From` header on self-run probes.
pub const PROBE_FROM_ENV: &str = "CODOSEO_PROBE_FROM";

/// The `From` header for probes run by the CLI or the local MCP server: `CODOSEO_PROBE_FROM`
/// when set to a valid value, else [`PROBE_FROM`] (see [`probe_from_value`]).
pub fn probe_from_env() -> String {
    probe_from_value(std::env::var(PROBE_FROM_ENV).ok().as_deref())
}

/// The `From` header for a `CODOSEO_PROBE_FROM` value: the value trimmed, or [`PROBE_FROM`] when
/// it is unset or blank, or (with a warning in the log) not a valid header value.
pub fn probe_from_value(value: Option<&str>) -> String {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return PROBE_FROM.to_owned();
    };
    valid_from(value).unwrap_or_else(|| {
        tracing::warn!("{PROBE_FROM_ENV} is not a valid header value; using {PROBE_FROM}");
        PROBE_FROM.to_owned()
    })
}

/// `value` trimmed when it is a `From` the probes can send: visible ASCII and spaces only.
fn valid_from(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.bytes().all(|b| (0x20..0x7f).contains(&b)))
        .then(|| value.to_owned())
}

/// The probe candidates named in `wanted` (any case), in registry order. An unknown token, one
/// that is never probed, or an empty list is an error that lists the valid tokens.
pub fn select_bots(wanted: &[String]) -> Result<Vec<ProbeBot>, String> {
    let (bots, skipped) = candidates();
    let valid = bots
        .iter()
        .map(|b| b.token.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if wanted.iter().all(|w| w.trim().is_empty()) {
        return Err(format!("no bots given; probeable bots: {valid}"));
    }
    for w in wanted.iter().map(|w| w.trim()).filter(|w| !w.is_empty()) {
        if bots.iter().any(|b| b.token.eq_ignore_ascii_case(w)) {
            continue;
        }
        let why = match skipped.iter().find(|s| s.token.eq_ignore_ascii_case(w)) {
            Some(s) => format!(
                "{} is not probed ({})",
                s.token,
                match s.reason {
                    SkipReason::NoUserAgent => "its operator documents no full user agent",
                    SkipReason::VerifiedSearchEngine =>
                        "a verified search engine, refused from any other address",
                    SkipReason::NotProbedPurpose =>
                        "only search and user-triggered fetchers are probed",
                }
            ),
            None => format!("unknown bot {w}"),
        };
        return Err(format!("{why}; probeable bots: {valid}"));
    }
    Ok(bots
        .into_iter()
        .filter(|b| {
            wanted
                .iter()
                .any(|w| b.token.eq_ignore_ascii_case(w.trim()))
        })
        .collect())
}

/// A single page probed on its own: an access report over just that page (and the home page,
/// unrequested, when the page is not it) with the probes in [`AccessReport::probes`].
#[derive(Debug, Clone)]
pub struct PageProbe {
    pub report: AccessReport,
}

impl PageProbe {
    /// The probe run; always present on a `PageProbe`.
    pub fn probes(&self) -> &ProbeReport {
        self.report
            .probes
            .as_ref()
            .expect("a PageProbe always carries its probes")
    }

    /// The probed page's URL.
    pub fn page_url(&self) -> &str {
        self.probes()
            .urls
            .first()
            .and_then(|u| self.report.important.get(usize::from(u.url)))
            .map_or("", |u| u.url.as_str())
    }
}

/// Probes `url` alone with every candidate bot, or the `bots` subset (see [`select_bots`]): reads
/// the site's robots.txt, skips what it disallows, and compares each bot with a CodoSEObot
/// baseline. The page itself is not fetched beforehand: the baseline says whether it is an HTML
/// 200. Everything, robots.txt included, ends by `deadline` (see [`PAGE_PROBE_DEADLINE`]): a run
/// cut short returns the bots it reached, the rest inconclusive and `stop` set to `budget`.
/// Errors are messages for the user: bad bot names, an unreadable robots.txt.
pub async fn probe_page(
    url: &Url,
    bots: Option<&[String]>,
    address_policy: AddressPolicy,
    from: &str,
    min_gap: Duration,
    deadline: Duration,
) -> Result<PageProbe, String> {
    let until = Instant::now() + deadline;
    let selected = bots.map(select_bots).transpose()?;
    let mut url = url.clone();
    url.set_fragment(None);
    let origin = url
        .join("/")
        .map_err(|e| format!("not a site address: {e}"))?;

    let fetcher = Fetcher::new(FetcherConfig::new(address_policy)).map_err(|e| e.to_string())?;
    let (rules, file) = tokio::time::timeout_at(until, fetch_robots(&fetcher, &url))
        .await
        .map_err(|_| "could not fetch robots.txt in time; nothing was probed".to_owned())?
        .map_err(|e| format!("could not fetch robots.txt: {e}"))?;
    let robots = RobotsTxt::from_response(Some(file.status), file.body.as_bytes());

    // A one-page crawl standing in for the real one: the page counts as an HTML 200 so the plan
    // takes it, and the baseline then shows what it really is.
    let page = PageRecord {
        url_hash: url_hash(&url),
        url: url.clone(),
        status: 200,
        redirect_chain: Vec::new(),
        response_ms: 0,
        size_bytes: 0,
        content_type: Some("text/html".to_owned()),
        depth: Some(0),
        in_sitemap: false,
        indexability: Indexability::Indexable,
        fields: PageFields::default(),
        inlinks: 0,
        outlinks_internal: 0,
        outlinks_external: 0,
        issues: IssueBits::default(),
        key_hash: 0,
        redirect_target: None,
        outlinks_nofollow: 0,
        error: None,
    };
    let robots_status = file.status;
    let out = CrawlOutput {
        origin: origin.clone(),
        pages: vec![page],
        links: LinkGraph::default(),
        robots: Some(file),
        sitemap: SitemapSummary::default(),
        stop: StopReason::Completed,
        duration_ms: 0,
        signals: SiteSignals::default(),
    };
    let important = important_urls(&out.pages, &origin, &HashSet::new());
    let mut report = build_report(&out, &important);
    if !report.has_verdicts() {
        return Err(format!(
            "robots.txt answered HTTP {robots_status}, so crawlers are told to stay away for now; \
             nothing was probed"
        ));
    }
    // The baseline is a request as CodoSEObot, so CodoSEObot's own rules apply to it, as in a
    // crawl.
    if !rules.allowed(url.as_str()) {
        return Err(
            "robots.txt disallows CodoSEObot on this page, so it is not fetched; nothing was probed"
                .to_owned(),
        );
    }
    let Some(mut plan) = plan_probes(&report, robots.as_ref(), 1) else {
        return Err("nothing to probe on this page".to_owned());
    };
    if let Some(selected) = selected {
        plan.bots = selected;
    }
    let left = until.saturating_duration_since(Instant::now());
    let mut cfg = ProbeConfig::new(&report, plan, address_policy, left, PAGE_PROBE_MAX_REQUESTS);
    cfg.deadline = left;
    cfg.from = from.to_owned();
    cfg.min_gap = min_gap;
    report.probes = Some(run_probes(cfg).await);
    Ok(PageProbe { report })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, retry_after: Option<&str>) -> Result<FetchResult, FetchError> {
        let mut headers = HeaderMap::new();
        if let Some(v) = retry_after {
            headers.insert("retry-after", HeaderValue::from_str(v).unwrap());
        }
        let url = Url::parse("https://example.com/").unwrap();
        Ok(FetchResult {
            final_url: url,
            status,
            chain: Vec::new(),
            headers,
            content_type: None,
            x_robots_tag: None,
            response_ms: 1,
            size_bytes: 0,
            body: None,
            truncated: false,
        })
    }

    #[test]
    fn retries_wait_for_retry_after_capped_at_ten_seconds_or_two_by_default() {
        let secs = Duration::from_secs;
        assert_eq!(retry_wait(&response(429, Some("3"))), Some(secs(3)));
        assert_eq!(retry_wait(&response(503, Some("3600"))), Some(secs(10)));
        assert_eq!(retry_wait(&response(429, None)), Some(secs(2)));
        assert_eq!(retry_wait(&response(503, Some("soon"))), Some(secs(2)));
        assert_eq!(retry_wait(&response(429, Some("0"))), Some(secs(0)));
        for status in [200, 403, 404, 500, 502] {
            assert_eq!(retry_wait(&response(status, Some("1"))), None, "{status}");
        }
        assert_eq!(retry_wait(&Err(FetchError::Timeout)), Some(secs(2)));
        assert_eq!(
            retry_wait(&Err(FetchError::Connect("refused".into()))),
            Some(secs(2))
        );
        assert_eq!(
            retry_wait(&Err(FetchError::Http("reset".into()))),
            Some(secs(2))
        );
        assert_eq!(
            retry_wait(&Err(FetchError::Blocked("10.0.0.1".into()))),
            None
        );
        assert_eq!(
            retry_wait(&Err(FetchError::TooManyRedirects { chain: Vec::new() })),
            None
        );
    }

    #[test]
    fn the_gap_is_the_minimum_or_the_crawl_delay_capped_at_ten_seconds() {
        let ms = Duration::from_millis;
        assert_eq!(limiter(DEFAULT_MIN_GAP, None).interval(), ms(500));
        assert_eq!(limiter(DEFAULT_MIN_GAP, Some(0.2)).interval(), ms(500));
        assert_eq!(limiter(DEFAULT_MIN_GAP, Some(3.0)).interval(), ms(3_000));
        assert_eq!(limiter(DEFAULT_MIN_GAP, Some(120.0)).interval(), ms(10_000));
        assert_eq!(
            limiter(DEFAULT_MIN_GAP, Some(f64::INFINITY)).interval(),
            ms(500)
        );
        assert_eq!(limiter(DEFAULT_MIN_GAP, Some(-1.0)).interval(), ms(500));
        assert!(limiter(Duration::ZERO, None).interval() < ms(5));
    }

    #[tokio::test(start_paused = true)]
    async fn the_limiter_spaces_requests_by_the_gap() {
        let l = limiter(DEFAULT_MIN_GAP, Some(3.0));
        let start = Instant::now();
        for _ in 0..3 {
            drop(l.acquire().await);
        }
        assert_eq!(start.elapsed(), Duration::from_secs(6));
    }

    #[test]
    fn a_fetch_keeps_no_body_only_what_the_comparison_needs() {
        let requested = Url::parse("https://example.com/a").unwrap();
        let body = b"<html><head><title>Just a moment...</title></head></html>".to_vec();
        let mut r = match response(403, None) {
            Ok(r) => r,
            Err(_) => unreachable!(),
        };
        r.final_url = Url::parse("https://example.com/b").unwrap();
        r.chain = vec![crate::fetch::Hop {
            status: 302,
            url: requested.clone(),
        }];
        r.content_type = Some("text/html".to_owned());
        r.body = Some(body.clone().into());
        r.truncated = true;
        let f = probe_fetch(&requested, Ok(r), true);
        assert_eq!(f.status, Some(403));
        assert_eq!(f.final_url.as_deref(), Some("https://example.com/b"));
        assert_eq!(f.redirects, 1);
        assert_eq!(f.bytes, body.len() as u32);
        assert!(f.truncated && f.html && f.retried);
        assert_eq!(f.title.as_deref(), Some("Just a moment..."));
        assert_eq!(
            f.challenge.map(|c| c.vendor),
            Some(codoseo_geo::probe::Vendor::Cloudflare)
        );

        let long = format!("https://example.com/{}", "x".repeat(4_000));
        let far = response(200, None).map(|mut r| {
            r.final_url = Url::parse(&long).unwrap();
            r
        });
        let capped = probe_fetch(&requested, far, false).final_url.unwrap();
        assert_eq!(capped.chars().count(), codoseo_geo::probe::MAX_URL);
        assert!(capped.ends_with('…'));

        let same = response(200, None).map(|mut r| {
            r.final_url = requested.clone();
            r
        });
        assert_eq!(probe_fetch(&requested, same, false).final_url, None);

        for (e, kind) in [
            (FetchError::Timeout, FetchErrorKind::Timeout),
            (FetchError::Connect("x".into()), FetchErrorKind::Connect),
            (FetchError::Http("x".into()), FetchErrorKind::Dropped),
            (FetchError::Blocked("x".into()), FetchErrorKind::Blocked),
            (
                FetchError::RedirectLoop { chain: Vec::new() },
                FetchErrorKind::TooManyRedirects,
            ),
            (
                FetchError::InvalidRedirect {
                    location: "x".into(),
                },
                FetchErrorKind::Other,
            ),
        ] {
            let f = probe_fetch(&requested, Err(e), false);
            assert_eq!((f.status, f.error), (None, Some(kind)));
        }
    }
}
