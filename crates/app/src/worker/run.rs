//! The worker loop: one claim-and-run cycle ([`worker_loop_once`]), and the real poll loop
//! around it ([`worker_loop`]) with Postgres-down backoff and a graceful-shutdown drain.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use codoseo_checks::run_checks;
use codoseo_core::crawl::{AddressPolicy, CrawlConfig, CrawlLimits, Politeness, USER_AGENT};
use codoseo_core::output::{
    BLOCKED_REASON_PREFIX, Progress, StopReason, UNREACHABLE_REASON_PREFIX,
};
use codoseo_core::plan::{Plan, PlanLimits};
use codoseo_crawler::crawl::crawl;
use codoseo_diff::{diff, key_pages};
use codoseo_geo::report::{AccessReport, build_report, important_urls};
use codoseo_geo::robots::{RobotsAvailability, availability};
use codoseo_store::crawl_queue::{ClaimedCrawl, CrawlQueue, CrawlTrigger};
use codoseo_store::finalize::finalize;
use codoseo_store::geo::{self, GeoInput};
use codoseo_store::jobs::JobQueue;
use codoseo_web::metrics;
use sqlx::PgPool;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
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
/// Default memory budget (`budget::memory_budget_bytes`'s fallback) when no cgroup limit is
/// readable — 1 GB, generous for a dev machine or an unrestricted CI runner.
pub const DEFAULT_MEMORY_BUDGET: u64 = 1_000_000_000;
/// A `running` crawl whose heartbeat is older than this is considered dead and eligible for
/// [`requeue_stale_sweep`] — comfortably past [`LIVENESS_INTERVAL`] so a merely slow (not dead)
/// worker's own liveness heartbeat has time to land first.
pub const STALE_AFTER: Duration = Duration::from_secs(45);
/// How often [`requeue_stale_sweep`] checks for stale crawls.
pub const SWEEP_PERIOD: Duration = Duration::from_secs(30);
/// Postgres's SQLSTATE for a unique-constraint violation.
const UNIQUE_VIOLATION: &str = "23505";

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
    default_budget: u64,
    policy: AddressPolicy,
) -> Result<bool, WorkerError> {
    let Some((crawl_id, handle)) = claim_and_spawn(
        pool,
        crawl_queue,
        job_queue,
        worker_id,
        default_budget,
        policy,
    )
    .await?
    else {
        return Ok(false);
    };
    handle_outcome(crawl_queue, crawl_id, worker_id, handle.await).await?;
    Ok(true)
}

/// Claims the next eligible crawl (if any) and spawns a task that runs it end to end. Returns
/// the crawl's id and the task's handle, so callers can either await it immediately
/// ([`worker_loop_once`]) or race it against a shutdown signal ([`worker_loop`]).
///
/// A claim that fails on a Postgres unique-violation (two workers racing the same domain's
/// `NOT EXISTS` guard — see `CrawlQueue::claim`'s doc comment) is reported as `Ok(None)`, the
/// same as an empty queue, rather than as an error: it is a benign scheduling race, not a
/// Postgres outage, and `worker_loop` must not back off for it.
async fn claim_and_spawn(
    pool: &PgPool,
    crawl_queue: &CrawlQueue,
    _job_queue: &JobQueue,
    worker_id: &str,
    default_budget: u64,
    policy: AddressPolicy,
) -> Result<Option<(uuid::Uuid, JoinHandle<Result<(), String>>)>, WorkerError> {
    let claimed = match crawl_queue.claim(worker_id).await {
        Ok(Some(claimed)) => claimed,
        Ok(None) => return Ok(None),
        Err(e) if is_benign_claim_race(&e) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let crawl_id = claimed.id;
    metrics::queue_wait(claimed.priority, claimed.queue_wait_secs);
    // Every log line of the crawl carries these (the domain is public, never an email or key).
    let span = tracing::info_span!(
        "crawl",
        %crawl_id,
        site_id = %claimed.site_id,
        domain = %claimed.domain,
    );
    let pool_for_task = pool.clone();
    let crawl_queue_for_task = crawl_queue.clone();
    let worker_id = worker_id.to_owned();
    let handle = tokio::spawn(async move {
        run_one_crawl(
            &pool_for_task,
            &crawl_queue_for_task,
            &worker_id,
            default_budget,
            policy,
            claimed,
        )
        .instrument(span)
        .await
    });
    Ok(Some((crawl_id, handle)))
}

/// True for a Postgres unique-constraint violation — the shape `claim()`'s domain guard can hit
/// when two workers race the same domain's queued rows (see `CrawlQueue::claim`).
fn is_benign_claim_race(e: &sqlx::Error) -> bool {
    matches!(
        e,
        sqlx::Error::Database(db_err)
            if db_err.code() == Some(Cow::Borrowed(UNIQUE_VIOLATION))
    )
}

/// Turns a finished (or panicked) crawl task's result into a `finish_failed` call when needed.
/// A panic (`Err(JoinError)`) and a handled-but-failed crawl (`Ok(Err(reason))`) both end the
/// crawl the same way, so one bad crawl never stalls the queue.
async fn handle_outcome(
    crawl_queue: &CrawlQueue,
    crawl_id: uuid::Uuid,
    worker_id: &str,
    result: Result<Result<(), String>, tokio::task::JoinError>,
) -> Result<(), WorkerError> {
    metrics::crawl_finished(matches!(result, Ok(Ok(()))));
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(reason)) => {
            tracing::warn!(%crawl_id, %reason, "crawl failed");
            crawl_queue
                .finish_failed(crawl_id, &reason, worker_id)
                .await?;
            Ok(())
        }
        Err(_join_error) => {
            tracing::error!(%crawl_id, "crawl task panicked");
            crawl_queue
                .finish_failed(crawl_id, "internal error", worker_id)
                .await?;
            Ok(())
        }
    }
}

/// The address policy for a `CODOSEO_MODE` value: the cloud (`cloud`) refuses private and
/// internal addresses, everything else (self-hosted, unset) allows them.
pub fn address_policy_for_mode(mode: Option<&str>) -> AddressPolicy {
    match mode {
        Some("cloud") => AddressPolicy::Public,
        _ => AddressPolicy::AllowPrivate,
    }
}

/// The policy for this process, from the `CODOSEO_MODE` environment variable.
pub fn address_policy_from_env() -> AddressPolicy {
    address_policy_for_mode(std::env::var("CODOSEO_MODE").ok().as_deref())
}

/// Resolves the crawl limits that actually govern this run: a quick (unclaimed) audit uses
/// `PlanLimits::quick_audit()`; otherwise the site's account's plan via `PlanLimits::for_plan`.
/// `crawl_settings.max_pages`, if present, may only narrow the plan's cap, never exceed it.
pub async fn resolve_limits(
    pool: &PgPool,
    site_id: uuid::Uuid,
    crawl_settings: &serde_json::Value,
) -> Result<PlanLimits, String> {
    let plan_text: Option<String> =
        sqlx::query_scalar("SELECT a.plan::text FROM sites s LEFT JOIN accounts a ON a.id = s.account_id WHERE s.id = $1")
            .bind(site_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("could not resolve plan for site: {e}"))?
            .flatten();
    let limits = match plan_text {
        None => PlanLimits::quick_audit(),
        Some(text) => {
            let plan: Plan = serde_json::from_value(serde_json::Value::String(text.clone()))
                .map_err(|e| format!("unknown plan '{text}': {e}"))?;
            PlanLimits::for_plan(plan)
        }
    };

    let override_max_pages = crawl_settings
        .get("max_pages")
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok());
    let max_pages = match (override_max_pages, limits.max_pages) {
        (Some(o), Some(plan_cap)) => o.min(plan_cap),
        (Some(o), None) => o,
        (None, Some(plan_cap)) => plan_cap,
        (None, None) => CrawlLimits::default().max_pages,
    };

    Ok(PlanLimits {
        max_pages: Some(max_pages),
        ..limits
    })
}

/// Runs one claimed crawl end to end: crawl → checks → diff → finalize. Returns a human-sized
/// reason on failure so the caller can fail the crawl the same way whether this returned an
/// error or panicked.
async fn run_one_crawl(
    pool: &PgPool,
    crawl_queue: &CrawlQueue,
    worker_id: &str,
    default_budget: u64,
    policy: AddressPolicy,
    claimed: ClaimedCrawl,
) -> Result<(), String> {
    // Test-only panic seam: lets integration tests prove a panicking crawl is isolated to its
    // own task without needing the real crawler to panic. Never set by production code.
    if claimed
        .crawl_settings
        .get("test_panic_before_crawl")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        panic!("test_panic_before_crawl");
    }

    let start_url = Url::parse(&claimed.start_url).map_err(|e| format!("bad start URL: {e}"))?;
    let limits = resolve_limits(pool, claimed.site_id, &claimed.crawl_settings).await?;
    let max_pages = limits.max_pages.unwrap_or(CrawlLimits::default().max_pages);
    let max_duration = limits
        .max_duration
        .unwrap_or(CrawlLimits::default().max_duration);

    let budget = crate::worker::budget::memory_budget_bytes(default_budget);
    metrics::worker_memory_budget(budget);
    if !crate::worker::budget::fits(max_pages, budget) {
        return Err(format!(
            "exceeds worker memory budget: {max_pages} pages needs more than the {budget}-byte budget"
        ));
    }
    // Counted as running, with its memory reserved, until this function ends (or panics).
    let _running = metrics::crawl_started(crate::worker::budget::reserved_bytes(max_pages));
    tracing::info!(max_pages, attempt = claimed.attempt, "crawl started");

    let cfg = CrawlConfig {
        start_url,
        limits: CrawlLimits {
            max_pages,
            max_duration,
            ..CrawlLimits::default()
        },
        politeness: Politeness::default(),
        // The cloud refuses private and internal addresses; self-hosted behaves like the CLI.
        address_policy: policy,
        user_agent: USER_AGENT.to_owned(),
        // A no-signup audit keeps no AI access report, so it asks for nothing beyond pages.
        site_signals: claimed.trigger != CrawlTrigger::Quick,
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
    if let Ok(out) = &crawl_result {
        metrics::pages_crawled(out.pages.len() as u64);
    }

    let mut out = crawl_result.map_err(|e| format!("could not start crawl: {e}"))?;

    // A site that never responded usefully: nothing to finalize, fail the crawl outright
    // (spec section 12: unreachable/blocked sites are failures, not partial successes).
    if out.pages.is_empty()
        && matches!(
            out.stop,
            StopReason::Unreachable(_) | StopReason::Blocked(_)
        )
    {
        // A robots.txt that fails (5xx, 429) is itself an AI-access incident: record it before
        // the crawl fails for good, so the owner hears about it even though there is nothing
        // to finalize.
        record_failed_robots(pool, &claimed, worker_id, &out).await;
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
            // Key pages: the origin, the top 20 by inlinks and the pages the user starred.
            let starred = codoseo_store::sites::starred_key_pages(pool, claimed.site_id)
                .await
                .map_err(|e| format!("could not load starred pages: {e}"))?;
            let key = key_pages(&curr, &starred);
            diff(&prev, &curr, &key)
        }
        None => Vec::new(),
    };

    // A no-signup audit has no site owner to tell: the AI access state starts with the first
    // full crawl. The findings are drawn in `finalize`, under the intent the site has then.
    let geo = if claimed.trigger == CrawlTrigger::Quick {
        None
    } else {
        let starred = codoseo_store::sites::starred_key_pages(pool, claimed.site_id)
            .await
            .map_err(|e| format!("could not load starred pages: {e}"))?;
        let important = important_urls(&out.pages, &out.origin, &starred);
        Some(GeoInput::new(build_report(&out, &important)))
    };

    finalize(
        pool,
        claimed.id,
        claimed.site_id,
        worker_id,
        &out,
        &report,
        &changes,
        geo.as_ref(),
    )
    .await
    .map_err(|e| format!("finalize failed: {e}"))?;

    tracing::info!(pages = out.pages.len(), "crawl finished");
    Ok(())
}

/// For a crawl that is about to fail for good with no pages: when robots.txt answered 5xx or
/// 429 (which crawlers must treat as "everything is off limits"), records the AI access report
/// and the `RobotsUnavailable` incident it shows. A first failure is retried in 15 minutes
/// (`finish_failed`) and records nothing, so a short blip is not announced and then resolved.
/// Best effort: a failure here is logged and the crawl fails exactly as it would have.
async fn record_failed_robots(
    pool: &PgPool,
    claimed: &ClaimedCrawl,
    worker_id: &str,
    out: &codoseo_core::output::CrawlOutput,
) {
    if claimed.trigger == CrawlTrigger::Quick || !claimed.is_final_attempt() {
        return;
    }
    let Some(robots) = &out.robots else { return };
    // Only what crawlers must read as "everything is off limits": 5xx and 429.
    if availability(Some(robots.status)) != RobotsAvailability::Unavailable
        || !(robots.status >= 500 || robots.status == 429)
    {
        return;
    }
    let result = async {
        // No pages came back, so the only important URL is the origin itself.
        let important = important_urls(&out.pages, &out.origin, &Default::default());
        let report: AccessReport = build_report(out, &important);
        let input = GeoInput::new(report);
        geo::record_failed_crawl(pool, claimed.site_id, claimed.id, worker_id, &input).await
    }
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "could not record the AI access report of a failed crawl");
    }
}

fn stop_reason_message(stop: &StopReason) -> String {
    match stop {
        StopReason::Unreachable(reason) => format!("{UNREACHABLE_REASON_PREFIX}: {reason}"),
        StopReason::Blocked(reason) => format!("{BLOCKED_REASON_PREFIX}: {reason}"),
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
    default_budget: u64,
    policy: AddressPolicy,
    shutdown: CancellationToken,
) {
    let mut backoff = Duration::from_secs(1);

    loop {
        if shutdown.is_cancelled() {
            break;
        }

        let claimed = claim_and_spawn(
            pool,
            crawl_queue,
            job_queue,
            worker_id,
            default_budget,
            policy,
        )
        .await;
        match claimed {
            Ok(Some((crawl_id, mut handle))) => {
                backoff = Duration::from_secs(1);
                tokio::select! {
                    result = &mut handle => {
                        let _ = handle_outcome(crawl_queue, crawl_id, worker_id, result).await;
                    }
                    _ = shutdown.cancelled() => {
                        // Give the in-flight crawl up to SHUTDOWN_GRACE to finish before
                        // abandoning it: an abandoned crawl stays `running` in Postgres with a
                        // staling heartbeat, and a `requeue_stale` sweep later puts it back in
                        // the queue for another worker.
                        match tokio::time::timeout(SHUTDOWN_GRACE, &mut handle).await {
                            Ok(result) => {
                                let _ = handle_outcome(crawl_queue, crawl_id, worker_id, result).await;
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

/// A sibling background task to [`worker_loop`]: every [`SWEEP_PERIOD`], moves `running` crawls
/// whose heartbeat is older than [`STALE_AFTER`] back to `queued`, rescuing crawls left behind
/// by a worker that died without a graceful shutdown (SIGKILL, not SIGTERM — `worker_loop`
/// itself already handles the graceful case via `SHUTDOWN_GRACE`). Runs until `shutdown` fires.
pub async fn requeue_stale_sweep(crawl_queue: CrawlQueue, shutdown: CancellationToken) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(SWEEP_PERIOD) => {
                match crawl_queue.requeue_stale(STALE_AFTER).await {
                    Ok(0) => {}
                    Ok(n) => tracing::warn!(count = n, "worker: requeued stale crawl(s)"),
                    Err(e) => tracing::error!(error = %e, "worker: requeue_stale sweep failed"),
                }
            }
            _ = shutdown.cancelled() => break,
        }
    }
}
