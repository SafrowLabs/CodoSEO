//! Request pacing for one site: a minimum gap between requests, a cap on parallel
//! connections, and a back-off when the site answers 429 or 503.
//!
//! Callers take a [`Permit`] with [`Limiter::acquire`] before every request and report
//! the status with [`Limiter::on_response`] afterwards. Time comes from
//! `tokio::time`, so tests can run on a paused clock.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use codoseo_core::crawl::Politeness;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, sleep_until};

/// A `Retry-After` longer than this is cut to it.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

struct State {
    interval: Duration,
    /// The earliest start time still free for the next request.
    next_slot: Instant,
    paused_until: Instant,
}

/// Paces the requests to one site and shares a global in-flight cap with other sites.
pub struct Limiter {
    state: Mutex<State>,
    max_interval: Duration,
    /// `1 / requests_per_sec`, before any crawl delay or back-off.
    rate_gap: Duration,
    site: Arc<Semaphore>,
    global: Arc<Semaphore>,
}

/// Holds the site and global connection slots until dropped.
#[derive(Debug)]
pub struct Permit {
    _site: OwnedSemaphorePermit,
    _global: OwnedSemaphorePermit,
}

impl Limiter {
    /// The gap between requests is the slower of `1 / requests_per_sec` and the site's
    /// `Crawl-delay`, never above `Politeness::max_crawl_delay`.
    pub fn new(p: &Politeness, crawl_delay: Option<Duration>, global: Arc<Semaphore>) -> Limiter {
        let max_interval = p.max_crawl_delay;
        let rate_gap = if p.requests_per_sec.is_finite() && p.requests_per_sec > 0.0 {
            Duration::from_secs_f64(1.0 / f64::from(p.requests_per_sec))
        } else {
            max_interval
        };
        let interval = rate_gap
            .max(crawl_delay.unwrap_or(Duration::ZERO))
            .min(max_interval);
        let now = Instant::now();
        Limiter {
            state: Mutex::new(State {
                interval,
                next_slot: now,
                paused_until: now,
            }),
            max_interval,
            rate_gap,
            site: Arc::new(Semaphore::new(p.per_site_connections.max(1) as usize)),
            global,
        }
    }

    /// Waits for a site connection, then for this request's start time, then for a
    /// global slot. The start time is reserved before waiting, so concurrent callers
    /// get evenly spaced slots. A pause set while waiting (a 429 or 503 on another
    /// connection) still applies: the request then reserves a new slot after it.
    pub async fn acquire(&self) -> Permit {
        let site = self
            .site
            .clone()
            .acquire_owned()
            .await
            .expect("the site semaphore is never closed");
        loop {
            let slot = {
                let mut s = self.lock();
                let slot = Instant::now().max(s.next_slot).max(s.paused_until);
                s.next_slot = slot + s.interval;
                slot
            };
            sleep_until(slot).await;
            if self.lock().paused_until <= Instant::now() {
                break;
            }
        }
        let global = self
            .global
            .clone()
            .acquire_owned()
            .await
            .expect("the global semaphore is never closed");
        Permit {
            _site: site,
            _global: global,
        }
    }

    /// Reports a response. A 429 or 503 doubles the interval (up to the cap) and holds
    /// back the next requests for `retry_after`, or two new intervals when the site
    /// gave none.
    pub fn on_response(&self, status: u16, retry_after: Option<Duration>) {
        if status != 429 && status != 503 {
            return;
        }
        let mut s = self.lock();
        s.interval = s.interval.saturating_mul(2).min(self.max_interval);
        let pause = retry_after
            .unwrap_or_else(|| s.interval.saturating_mul(2))
            .min(MAX_RETRY_AFTER);
        s.paused_until = s.paused_until.max(Instant::now() + pause);
    }

    /// Applies a `Crawl-delay` learned after creation (robots.txt is read through this
    /// limiter). The gap becomes the slower of `1 / requests_per_sec` and `delay`, capped
    /// as in [`Limiter::new`]; a longer gap from a 429/503 back-off is kept.
    pub fn set_crawl_delay(&self, delay: Option<Duration>) {
        let base = self
            .rate_gap
            .max(delay.unwrap_or(Duration::ZERO))
            .min(self.max_interval);
        let mut s = self.lock();
        s.interval = s.interval.max(base);
    }

    /// The current gap between requests.
    pub fn interval(&self) -> Duration {
        self.lock().interval
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // The state is plain data, so a poisoned lock is still consistent.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Reads a `Retry-After` header: a number of seconds or an HTTP date. A date in the
/// past gives zero. The result is not capped; see [`MAX_RETRY_AFTER`].
pub fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let at = httpdate::parse_http_date(value).ok()?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}
