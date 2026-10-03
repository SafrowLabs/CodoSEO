use std::sync::Arc;
use std::time::{Duration, SystemTime};

use codoseo_core::crawl::Politeness;
use codoseo_crawler::politeness::{Limiter, parse_retry_after};
use tokio::sync::Semaphore;
use tokio::time::Instant;

fn lim(rps: f32, delay: Option<Duration>) -> Limiter {
    let p = Politeness {
        requests_per_sec: rps,
        ..Politeness::default()
    };
    Limiter::new(&p, delay, Arc::new(Semaphore::new(64)))
}

async fn gaps(l: &Limiter, n: usize) -> Vec<Duration> {
    let start = Instant::now();
    let mut t = vec![];
    for _ in 0..n {
        let _p = l.acquire().await;
        t.push(start.elapsed());
    }
    t.windows(2).map(|w| w[1] - w[0]).collect()
}

#[tokio::test(start_paused = true)]
async fn five_per_second_spaces_requests_200ms() {
    assert!(
        gaps(&lim(5.0, None), 6)
            .await
            .iter()
            .all(|g| *g >= Duration::from_millis(200))
    );
}

#[tokio::test(start_paused = true)]
async fn crawl_delay_wins_when_slower() {
    assert!(
        gaps(&lim(5.0, Some(Duration::from_secs(3))), 3)
            .await
            .iter()
            .all(|g| *g >= Duration::from_secs(3))
    );
}

#[tokio::test(start_paused = true)]
async fn retry_after_is_respected() {
    let l = lim(5.0, None);
    drop(l.acquire().await);
    let t = Instant::now();
    l.on_response(429, Some(Duration::from_secs(5)));
    drop(l.acquire().await);
    assert!(t.elapsed() >= Duration::from_secs(5));
}

#[tokio::test(start_paused = true)]
async fn speed_halves_after_429_and_503() {
    let l = lim(5.0, None);
    l.on_response(429, None);
    assert_eq!(l.interval(), Duration::from_millis(400));
    l.on_response(503, None);
    assert_eq!(l.interval(), Duration::from_millis(800));
    for _ in 0..20 {
        l.on_response(429, None);
    }
    assert_eq!(l.interval(), Duration::from_secs(10));
}

#[tokio::test(start_paused = true)]
async fn missing_retry_after_pauses_two_intervals() {
    let l = lim(5.0, None);
    drop(l.acquire().await);
    let t = Instant::now();
    l.on_response(429, None);
    assert_eq!(l.interval(), Duration::from_millis(400));
    drop(l.acquire().await);
    assert!(t.elapsed() >= Duration::from_millis(800));
}

#[tokio::test(start_paused = true)]
async fn retry_after_is_capped_at_60s() {
    let l = lim(5.0, None);
    drop(l.acquire().await);
    let t = Instant::now();
    l.on_response(503, Some(Duration::from_secs(3600)));
    drop(l.acquire().await);
    let waited = t.elapsed();
    assert!(waited >= Duration::from_secs(60));
    assert!(waited <= Duration::from_secs(60) + l.interval());
}

#[tokio::test(start_paused = true)]
async fn ok_responses_dont_change_speed() {
    let l = lim(5.0, None);
    l.on_response(200, None);
    l.on_response(404, None);
    assert_eq!(l.interval(), Duration::from_millis(200));
}

#[tokio::test(start_paused = true)]
async fn per_site_connection_limit() {
    let l = lim(1000.0, None);
    let _a = l.acquire().await;
    let _b = l.acquire().await;
    assert!(
        tokio::time::timeout(Duration::from_secs(1), l.acquire())
            .await
            .is_err()
    );
}

#[tokio::test(start_paused = true)]
async fn global_limit_is_shared() {
    let g = Arc::new(Semaphore::new(1));
    let p = Politeness {
        requests_per_sec: 1000.0,
        ..Default::default()
    };
    let a = Limiter::new(&p, None, g.clone());
    let b = Limiter::new(&p, None, g);
    let _held = a.acquire().await;
    assert!(
        tokio::time::timeout(Duration::from_secs(1), b.acquire())
            .await
            .is_err()
    );
}

#[test]
fn retry_after_parsing() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    assert_eq!(
        parse_retry_after("120", now),
        Some(Duration::from_secs(120))
    );
    let later = httpdate::fmt_http_date(now + Duration::from_secs(30));
    assert_eq!(
        parse_retry_after(&later, now),
        Some(Duration::from_secs(30))
    );
    let past = httpdate::fmt_http_date(now - Duration::from_secs(30));
    assert_eq!(parse_retry_after(&past, now), Some(Duration::ZERO));
    assert_eq!(parse_retry_after("soon", now), None);
}
