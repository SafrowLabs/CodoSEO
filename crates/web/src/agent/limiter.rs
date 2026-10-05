//! A small in-memory limiter for the no-key MCP tools: how many calls one client address may
//! make in a sliding window. It guards the database from a client that polls or probes in a
//! tight loop; the audit and email limits that cost something live in the store. One web
//! container is enough for that, so the counts live here and a restart forgets them.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Calls a direct (non-shared) no-key client may make per window.
pub const ANON_CALLS_PER_WINDOW: usize = 30;
pub const ANON_WINDOW: Duration = Duration::from_secs(60);

/// Past this many tracked clients, idle ones are dropped on the next call.
const PRUNE_ABOVE: usize = 4096;

pub struct CallLimiter {
    max: usize,
    window: Duration,
    calls: Mutex<HashMap<Vec<u8>, VecDeque<Instant>>>,
}

impl CallLimiter {
    pub fn new(max: usize, window: Duration) -> CallLimiter {
        CallLimiter {
            max,
            window,
            calls: Mutex::new(HashMap::new()),
        }
    }

    /// The limit for the no-key tools: 30 calls a minute.
    pub fn for_anon_tools() -> CallLimiter {
        CallLimiter::new(ANON_CALLS_PER_WINDOW, ANON_WINDOW)
    }

    /// Counts one call by `key` at `now`. `Err` holds how long until the oldest counted call
    /// leaves the window (nothing is counted then).
    pub fn check(&self, key: &[u8], now: Instant) -> Result<(), Duration> {
        let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        if calls.len() > PRUNE_ABOVE {
            calls.retain(|_, times| {
                times
                    .back()
                    .is_some_and(|last| now.saturating_duration_since(*last) < self.window)
            });
        }
        let times = calls.entry(key.to_vec()).or_default();
        while times
            .front()
            .is_some_and(|first| now.saturating_duration_since(*first) >= self.window)
        {
            times.pop_front();
        }
        if times.len() >= self.max {
            let oldest = times.front().copied().unwrap_or(now);
            return Err((oldest + self.window).saturating_duration_since(now));
        }
        times.push_back(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_allows_the_limit_per_window_per_client_and_forgets_old_calls() {
        let limiter = CallLimiter::new(3, Duration::from_secs(60));
        let start = Instant::now();
        for _ in 0..3 {
            assert!(limiter.check(b"a", start).is_ok());
        }
        let wait = limiter
            .check(b"a", start + Duration::from_secs(10))
            .unwrap_err();
        assert_eq!(wait, Duration::from_secs(50));
        // A refusal counts nothing, and another client is unaffected.
        assert!(limiter.check(b"b", start).is_ok());
        // The window slides: the first three age out together.
        assert!(limiter.check(b"a", start + Duration::from_secs(60)).is_ok());
    }

    #[test]
    fn idle_clients_are_dropped_when_many_are_tracked() {
        let limiter = CallLimiter::new(1, Duration::from_secs(60));
        let start = Instant::now();
        for n in 0..=PRUNE_ABOVE {
            limiter.check(&n.to_be_bytes(), start).unwrap();
        }
        limiter
            .check(b"late", start + Duration::from_secs(120))
            .unwrap();
        assert_eq!(limiter.calls.lock().unwrap().len(), 1);
    }
}
