//! Synthetic AI-agent fetch probes, the network half: fetches each planned URL once with
//! CodoSEObot's user agent (the baseline) and once per probed bot with that bot's documented user
//! agent, and records the verdicts in a [`ProbeReport`]. What to probe and how to judge a response
//! live in [`codoseo_geo::probe`].
//!
//! Every request carries the same headers apart from `User-Agent`: the client defaults plus
//! `From`. Requests go one at a time through one [`Limiter`], at least `min_gap` apart or the
//! robots.txt `Crawl-delay` (capped at 10 s) when that is longer. A URL is probed for every bot or
//! not at all: the budget and the request cap are checked before each URL, and retries and
//! confirmations only use what is left once the URL's planned requests are kept aside.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use codoseo_core::crawl::{AddressPolicy, PROBE_FROM, Politeness, USER_AGENT};
use codoseo_geo::probe::{
    FetchErrorKind, InconclusiveReason, MAX_PROBE_BODY, ProbeBot, ProbeFetch, ProbePlan,
    ProbeReport, ProbeResult, ProbeStop, ProbedUrl, Verdict, classify, detect_challenge,
    page_title, robots_skips,
};
use codoseo_geo::report::AccessReport;
use reqwest::header::{FROM, HeaderMap, HeaderValue, USER_AGENT as USER_AGENT_HEADER};
use tokio::sync::Semaphore;
use tokio::time::{Instant, sleep};
use url::Url;

use crate::fetch::{FetchError, FetchResult, Fetcher, FetcherConfig, is_html};
use crate::politeness::{Limiter, retry_after_of};

/// The shortest gap between two probe requests.
pub const DEFAULT_MIN_GAP: Duration = Duration::from_millis(500);
/// The longest `Crawl-delay` and `Retry-After` the probes wait for.
pub const MAX_PROBE_WAIT: Duration = Duration::from_secs(10);
/// The wait before a retry when the site gave no `Retry-After`.
pub const DEFAULT_RETRY_WAIT: Duration = Duration::from_secs(2);
/// Per-request timeout: short, as a probe run has a budget of a minute or two.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

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
    /// current gap.
    pub budget: Duration,
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
    let from =
        HeaderValue::from_str(&cfg.from).unwrap_or_else(|_| HeaderValue::from_static(PROBE_FROM));
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
    let mut run = Run {
        fetcher,
        limiter: limiter(cfg.min_gap, cfg.plan.crawl_delay),
        from,
        deadline: Instant::now() + cfg.budget,
        max_requests: cfg.max_requests,
        requests: 0,
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
        let need = run.limiter.interval().saturating_mul(u32::from(planned));
        if run.deadline.saturating_duration_since(Instant::now()) < need {
            report.stop = Some(ProbeStop::Budget);
            break;
        }

        let mut left = wanted.len() as u16;
        let baseline = run.fetch(url, USER_AGENT, left).await;
        // The baseline against itself: inconclusive exactly when it can't serve as a baseline.
        let base_verdict = classify(url.as_str(), &baseline, &baseline);
        let usable = !matches!(base_verdict, Verdict::Inconclusive { .. });

        let mut results = Vec::with_capacity(cfg.plan.bots.len());
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
            let mut fetch = run.fetch(url, &bot.user_agent, left).await;
            let mut verdict = classify(url.as_str(), &baseline, &fetch);
            if let Some(cause) = verdict.cause_slug()
                && !confirmed.contains(&(bot.token.clone(), cause))
                && run.can_extra(left)
            {
                fetch = run.fetch(url, &bot.user_agent, left).await;
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
        report.urls.push(ProbedUrl {
            url: idx,
            baseline,
            results,
        });
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
    max_requests: u16,
    requests: u16,
}

impl Run {
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
        self.fetcher.fetch_with(url, headers, MAX_PROBE_BODY).await
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
        final_url: (r.final_url != *requested).then(|| r.final_url.to_string()),
        redirects: r.chain.len().min(usize::from(u8::MAX)) as u8,
        bytes: body.len().min(u32::MAX as usize) as u32,
        truncated: r.truncated,
        html: is_html(r.content_type.as_deref()),
        title: page_title(body),
        challenge: detect_challenge(r.status, &headers, body),
        retried,
    }
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
