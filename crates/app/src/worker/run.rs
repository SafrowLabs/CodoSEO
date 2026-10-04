//! The worker loop: one claim-and-run cycle ([`worker_loop_once`]), and the real poll loop
//! around it ([`worker_loop`]) with Postgres-down backoff and a graceful-shutdown drain.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use codoseo_checks::run_checks;
use codoseo_core::crawl::{AddressPolicy, CrawlConfig, CrawlLimits, Politeness, USER_AGENT};
use codoseo_core::output::{Progress, StopReason};
use codoseo_crawler::crawl::crawl;
use codoseo_diff::diff;
use codoseo_store::crawl_queue::{ClaimedCrawl, CrawlQueue};
use codoseo_store::finalize::finalize;
use codoseo_store::jobs::JobQueue;
use sqlx::PgPool;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use url::Url;

/// How often a crawl's progress callback is allowed to write a heartbeat.
const HEARTBEAT_THROTTLE: Duration = Duration::from_secs(2);
/// A liveness heartbeat is forced at least this often, even if progress is silent.
const LIVENESS_INTERVAL: Duration = Duration::from_secs(15);
/// How long `worker_loop` waits for an in-flight crawl to finish on shutdown before abandoning
/// it (it will be requeued once `requeue_stale` notices its heartbeat has gone stale).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(60);
/// How long an idle `worker_loop` waits between `claim()` polls.
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// The cap on Postgres-down backoff.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error(transparent)]
    Store(#[from] sqlx::Error),
}

/// One claim-and-run cycle: claims the next eligible crawl (if any) and runs it to completion
/// in a spawned task, isolating a panic to that crawl. Returns `Ok(true)` when a crawl was
/// claimed and handled (whether it ended `done` or `failed`), `Ok(false)` when the queue was
/// empty.
pub async fn worker_loop_once(
    pool: &PgPool,
    crawl_queue: &CrawlQueue,
    job_queue: &JobQueue,
    worker_id: &str,
) -> Result<bool, WorkerError> {
    let Some((crawl_id, handle)) = claim_and_spawn(pool, crawl_queue, job_queue, worker_id).await?
    else {
        return Ok(false);
    };
    handle_outcome(crawl_queue, crawl_id, handle.await).await?;
    Ok(true)
}

/// Claims the next eligible crawl (if any) and spawns a task that runs it end to end. Returns
/// the crawl's id and the task's handle, so callers can either await it immediately
/// ([`worker_loop_once`]) or race it against a shutdown signal ([`worker_loop`]).
async fn claim_and_spawn(
    pool: &PgPool,
    crawl_queue: &CrawlQueue,
    _job_queue: &JobQueue,
    worker_id: &str,
) -> Result<Option<(uuid::Uuid, JoinHandle<Result<(), String>>)>, WorkerError> {
    let Some(claimed) = crawl_queue.claim(worker_id).await? else {
        return Ok(None);
    };
    let crawl_id = claimed.id;
    let pool = pool.clone();
    let crawl_queue = crawl_queue.clone();
    let handle = tokio::spawn(async move { run_one_crawl(&pool, &crawl_queue, claimed).await });
    Ok(Some((crawl_id, handle)))
}

/// Turns a finished (or panicked) crawl task's result into a `finish_failed` call when needed.
/// A panic (`Err(JoinError)`) and a handled-but-failed crawl (`Ok(Err(reason))`) both end the
/// crawl the same way, so one bad crawl never stalls the queue.
async fn handle_outcome(
    crawl_queue: &CrawlQueue,
    crawl_id: uuid::Uuid,
    result: Result<Result<(), String>, tokio::task::JoinError>,
) -> Result<(), WorkerError> {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(reason)) => {
            crawl_queue.finish_failed(crawl_id, &reason).await?;
            Ok(())
        }
        Err(_join_error) => {
            tracing::error!(%crawl_id, "crawl task panicked");
            crawl_queue
                .finish_failed(crawl_id, "internal error")
                .await?;
            Ok(())
        }
    }
}

/// Runs one claimed crawl end to end: crawl → checks → diff → finalize. Returns a human-sized
/// reason on failure so the caller can fail the crawl the same way whether this returned an
/// error or panicked.
async fn run_one_crawl(
    pool: &PgPool,
    crawl_queue: &CrawlQueue,
    claimed: ClaimedCrawl,
) -> Result<(), String> {
    let start_url = Url::parse(&claimed.start_url).map_err(|e| format!("bad start URL: {e}"))?;
    let max_pages = claimed
        .crawl_settings
        .get("max_pages")
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok());

    let cfg = CrawlConfig {
        start_url,
        limits: CrawlLimits {
            max_pages: max_pages.unwrap_or(CrawlLimits::default().max_pages),
            ..CrawlLimits::default()
        },
        politeness: Politeness::default(),
        // Self-hosted default for now: the cloud/self-hosted split (`CODOSEO_MODE`) lands in
        // M9. Until then the worker behaves like the CLI and local MCP.
        address_policy: AddressPolicy::AllowPrivate,
        user_agent: USER_AGENT.to_owned(),
    };

    let crawl_id = claimed.id;
    let last_heartbeat_ms = Arc::new(AtomicI64::new(0));
    let on_progress = {
        let last_heartbeat_ms = Arc::clone(&last_heartbeat_ms);
        let crawl_queue = crawl_queue.clone();
        move |progress: Progress| {
            let now = now_millis();
            let prev = last_heartbeat_ms.load(Ordering::Relaxed);
            if now - prev < HEARTBEAT_THROTTLE.as_millis() as i64 {
                return;
            }
            if last_heartbeat_ms
                .compare_exchange(prev, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                let crawl_queue = crawl_queue.clone();
                tokio::spawn(async move {
                    let _ = crawl_queue.heartbeat(crawl_id, &progress).await;
                });
            }
        }
    };

    // A liveness heartbeat independent of progress, so a crawl stuck between progress calls
    // (e.g. one very slow page) doesn't look dead to `requeue_stale`. Cancelled once the crawl
    // finishes, below.
    let liveness = {
        let crawl_queue = crawl_queue.clone();
        tokio::spawn(async move {
            let idle = Progress {
                pages_done: 0,
                queued: 0,
                failures: 0,
                depth: 0,
                elapsed_ms: 0,
            };
            loop {
                tokio::time::sleep(LIVENESS_INTERVAL).await;
                if crawl_queue.heartbeat(crawl_id, &idle).await.is_err() {
                    return;
                }
            }
        })
    };

    let crawl_result = crawl(cfg, on_progress).await;
    liveness.abort();

    let mut out = crawl_result.map_err(|e| format!("could not start crawl: {e}"))?;

    // A site that never responded usefully: nothing to finalize, fail the crawl outright
    // (spec section 12: unreachable/blocked sites are failures, not partial successes).
    if out.pages.is_empty()
        && matches!(
            out.stop,
            StopReason::Unreachable(_) | StopReason::Blocked(_)
        )
    {
        return Err(stop_reason_message(&out.stop));
    }

    let report = run_checks(&mut out);
    let previous = crawl_queue
        .previous_snapshot(claimed.site_id)
        .await
        .map_err(|e| format!("could not load previous snapshot: {e}"))?;
    let changes = match previous {
        Some(prev) => {
            let curr = codoseo_core::snapshot::Snapshot::from_output(&out);
            diff(&prev, &curr, &HashSet::new())
        }
        None => Vec::new(),
    };

    finalize(pool, claimed.id, claimed.site_id, &out, &report, &changes)
        .await
        .map_err(|e| format!("finalize failed: {e}"))?;

    Ok(())
}

fn stop_reason_message(stop: &StopReason) -> String {
    match stop {
        StopReason::Unreachable(reason) => format!("site unreachable: {reason}"),
        StopReason::Blocked(reason) => format!("site blocked our crawler: {reason}"),
        other => format!("{other:?}"),
    }
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The real poll loop: calls `claim()` every [`POLL_INTERVAL`] when idle, backs off
/// exponentially (capped at [`MAX_BACKOFF`]) when Postgres is unreachable, and on cancellation
/// stops claiming new work and waits up to [`SHUTDOWN_GRACE`] for an in-flight crawl before
/// abandoning it.
pub async fn worker_loop(
    pool: &PgPool,
    crawl_queue: &CrawlQueue,
    job_queue: &JobQueue,
    worker_id: &str,
    shutdown: CancellationToken,
) {
    let mut backoff = Duration::from_secs(1);

    loop {
        if shutdown.is_cancelled() {
            break;
        }

        let claimed = claim_and_spawn(pool, crawl_queue, job_queue, worker_id).await;
        match claimed {
            Ok(Some((crawl_id, mut handle))) => {
                backoff = Duration::from_secs(1);
                tokio::select! {
                    result = &mut handle => {
                        let _ = handle_outcome(crawl_queue, crawl_id, result).await;
                    }
                    _ = shutdown.cancelled() => {
                        // Give the in-flight crawl up to SHUTDOWN_GRACE to finish before
                        // abandoning it: an abandoned crawl stays `running` in Postgres with a
                        // staling heartbeat, and a `requeue_stale` sweep later puts it back in
                        // the queue for another worker.
                        match tokio::time::timeout(SHUTDOWN_GRACE, &mut handle).await {
                            Ok(result) => {
                                let _ = handle_outcome(crawl_queue, crawl_id, result).await;
                            }
                            Err(_elapsed) => {
                                tracing::warn!(
                                    %crawl_id,
                                    "worker: shutdown grace period elapsed, abandoning in-flight crawl"
                                );
                                handle.abort();
                            }
                        }
                        break;
                    }
                }
            }
            Ok(None) => {
                backoff = Duration::from_secs(1);
                tokio::select! {
                    _ = tokio::time::sleep(POLL_INTERVAL) => {}
                    _ = shutdown.cancelled() => break,
                }
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    backoff_secs = backoff.as_secs(),
                    "worker: Postgres unreachable, backing off"
                );
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = shutdown.cancelled() => break,
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}
