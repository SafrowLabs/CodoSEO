//! The metrics CodoSEO records, named and labelled in one place. The `metrics` facade does
//! nothing until a recorder is installed (the `codoseo` binary installs a Prometheus one for
//! its server roles when `CODOSEO_METRICS_BIND` is set), so library code and tests can call
//! these freely.
//!
//! Labels are a closed set of short words (a lane number, an outcome, a channel kind). Never
//! put a URL, a domain, an email or an id in one: every distinct value is a new time series.

use metrics::{counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram};

pub const CRAWL_QUEUE_WAIT_SECONDS: &str = "codoseo_crawl_queue_wait_seconds";
pub const CRAWLS_RUNNING: &str = "codoseo_crawls_running";
pub const CRAWLS_FINISHED_TOTAL: &str = "codoseo_crawls_finished_total";
pub const PAGES_CRAWLED_TOTAL: &str = "codoseo_pages_crawled_total";
pub const WORKER_MEMORY_BUDGET_BYTES: &str = "codoseo_worker_memory_budget_bytes";
pub const WORKER_MEMORY_RESERVED_BYTES: &str = "codoseo_worker_memory_reserved_bytes";
pub const DB_POOL_CONNECTIONS: &str = "codoseo_db_pool_connections";
pub const ALERT_DELIVERIES_TOTAL: &str = "codoseo_alert_deliveries_total";
pub const API_REQUESTS_TOTAL: &str = "codoseo_api_requests_total";
pub const SCHEDULER_LAST_TICK_TIMESTAMP_SECONDS: &str =
    "codoseo_scheduler_last_tick_timestamp_seconds";
pub const JOBS_FAILED_TOTAL: &str = "codoseo_jobs_failed_total";

/// Every metric name, for the tests and the docs.
pub const ALL: [&str; 11] = [
    CRAWL_QUEUE_WAIT_SECONDS,
    CRAWLS_RUNNING,
    CRAWLS_FINISHED_TOTAL,
    PAGES_CRAWLED_TOTAL,
    WORKER_MEMORY_BUDGET_BYTES,
    WORKER_MEMORY_RESERVED_BYTES,
    DB_POOL_CONNECTIONS,
    ALERT_DELIVERIES_TOTAL,
    API_REQUESTS_TOTAL,
    SCHEDULER_LAST_TICK_TIMESTAMP_SECONDS,
    JOBS_FAILED_TOTAL,
];

/// Crawl priorities run 0 (most urgent) to 5; the label is the lane.
pub const LANES: [&str; 6] = ["0", "1", "2", "3", "4", "5"];
/// The alert channel kinds (`ChannelKind::as_str`).
pub const CHANNELS: [&str; 4] = ["email", "slack", "discord", "webhook"];
/// The job kinds (`JobKind::slug`).
pub const JOB_KINDS: [&str; 4] = ["send_alert", "send_digest", "send_email", "cleanup"];
/// What `api_requests_total`'s `result` is: `ok`, an [`AgentError::code`](crate::agent::error::AgentError::code) or `error`.
pub const API_RESULTS: [&str; 10] = [
    "ok",
    "unauthorized",
    "not_found",
    "bad_request",
    "crawl_in_progress",
    "plan_limit",
    "quota_exceeded",
    "unavailable",
    "internal",
    // A no-key MCP tool call that failed (those errors carry no code).
    "error",
];

/// Which API an agent call came through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Rest,
    Mcp,
}

impl Surface {
    fn label(self) -> &'static str {
        match self {
            Surface::Rest => "rest",
            Surface::Mcp => "mcp",
        }
    }
}

/// Whether the caller sent an API key (`key`) or is one of the no-key MCP clients (`anon`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Key,
    Anon,
}

impl Tier {
    fn label(self) -> &'static str {
        match self {
            Tier::Key => "key",
            Tier::Anon => "anon",
        }
    }
}

/// Describes every metric and creates the series that are known up front at zero, so a scrape
/// right after startup already lists them (and `rate()` sees the first increment).
pub fn register() {
    describe_histogram!(
        CRAWL_QUEUE_WAIT_SECONDS,
        metrics::Unit::Seconds,
        "Seconds a crawl waited in the queue before a worker claimed it, by priority lane"
    );
    describe_gauge!(CRAWLS_RUNNING, "Crawls this process is running right now");
    describe_counter!(
        CRAWLS_FINISHED_TOTAL,
        "Crawl runs that ended, by outcome (completed or failed; a failed first attempt is retried)"
    );
    describe_counter!(PAGES_CRAWLED_TOTAL, "Pages fetched by crawls");
    describe_gauge!(
        WORKER_MEMORY_BUDGET_BYTES,
        metrics::Unit::Bytes,
        "Memory the worker may spend on crawls (70% of its cgroup limit)"
    );
    describe_gauge!(
        WORKER_MEMORY_RESERVED_BYTES,
        metrics::Unit::Bytes,
        "Memory reserved by the crawls running now (page cap times the per-page estimate)"
    );
    describe_gauge!(
        DB_POOL_CONNECTIONS,
        "Postgres pool connections, by state (idle or active)"
    );
    describe_counter!(
        ALERT_DELIVERIES_TOTAL,
        "Alert delivery attempts, by channel kind and result"
    );
    describe_counter!(
        API_REQUESTS_TOTAL,
        "Agent API calls, by surface (rest, mcp), tier (key, anon) and result (ok or an error code)"
    );
    describe_gauge!(
        SCHEDULER_LAST_TICK_TIMESTAMP_SECONDS,
        metrics::Unit::Seconds,
        "Unix time of the scheduler's last tick"
    );
    describe_counter!(JOBS_FAILED_TOTAL, "Job runs that failed, by job kind");

    gauge!(CRAWLS_RUNNING).set(0.0);
    gauge!(WORKER_MEMORY_RESERVED_BYTES).set(0.0);
    counter!(PAGES_CRAWLED_TOTAL).absolute(0);
    for outcome in ["completed", "failed"] {
        counter!(CRAWLS_FINISHED_TOTAL, "outcome" => outcome).absolute(0);
    }
    for state in ["idle", "active"] {
        gauge!(DB_POOL_CONNECTIONS, "state" => state).set(0.0);
    }
    for channel in CHANNELS {
        for result in ["ok", "error"] {
            counter!(ALERT_DELIVERIES_TOTAL, "channel" => channel, "result" => result).absolute(0);
        }
    }
    for kind in JOB_KINDS {
        counter!(JOBS_FAILED_TOTAL, "kind" => kind).absolute(0);
    }
    for surface in [Surface::Rest, Surface::Mcp] {
        for tier in [Tier::Key, Tier::Anon] {
            counter!(API_REQUESTS_TOTAL, "surface" => surface.label(), "tier" => tier.label(), "result" => "ok")
                .absolute(0);
        }
    }
    for lane in LANES {
        let _ = histogram!(CRAWL_QUEUE_WAIT_SECONDS, "lane" => lane);
    }
}

/// A claimed crawl waited `seconds` in the queue. `priority` is the crawl's priority (its lane).
pub fn queue_wait(priority: i16, seconds: f64) {
    let lane = LANES[usize::try_from(priority).unwrap_or(0).min(LANES.len() - 1)];
    histogram!(CRAWL_QUEUE_WAIT_SECONDS, "lane" => lane).record(seconds.max(0.0));
}

/// Holds one running crawl in the gauges: `crawls_running` goes up by one and the reserved
/// memory by `reserved_bytes`, both undone when this is dropped (a panic included).
#[must_use = "the crawl counts as running until this is dropped"]
pub struct RunningCrawl {
    reserved_bytes: f64,
}

/// Marks a crawl as running, reserving `reserved_bytes` of the worker's memory budget.
pub fn crawl_started(reserved_bytes: u64) -> RunningCrawl {
    let reserved_bytes = reserved_bytes as f64;
    gauge!(CRAWLS_RUNNING).increment(1.0);
    gauge!(WORKER_MEMORY_RESERVED_BYTES).increment(reserved_bytes);
    RunningCrawl { reserved_bytes }
}

impl Drop for RunningCrawl {
    fn drop(&mut self) {
        gauge!(CRAWLS_RUNNING).decrement(1.0);
        gauge!(WORKER_MEMORY_RESERVED_BYTES).decrement(self.reserved_bytes);
    }
}

/// A crawl run ended: `completed` when its results were stored, otherwise `failed`.
pub fn crawl_finished(completed: bool) {
    let outcome = if completed { "completed" } else { "failed" };
    counter!(CRAWLS_FINISHED_TOTAL, "outcome" => outcome).increment(1);
}

pub fn pages_crawled(pages: u64) {
    counter!(PAGES_CRAWLED_TOTAL).increment(pages);
}

pub fn worker_memory_budget(bytes: u64) {
    gauge!(WORKER_MEMORY_BUDGET_BYTES).set(bytes as f64);
}

/// The pool's connections right now: `idle` ones are free, `active` ones are checked out.
pub fn db_pool(idle: u32, active: u32) {
    gauge!(DB_POOL_CONNECTIONS, "state" => "idle").set(f64::from(idle));
    gauge!(DB_POOL_CONNECTIONS, "state" => "active").set(f64::from(active));
}

/// One delivery attempt on a channel of kind `channel` (`ChannelKind::as_str`).
pub fn alert_delivery(channel: &'static str, ok: bool) {
    let result = if ok { "ok" } else { "error" };
    counter!(ALERT_DELIVERIES_TOTAL, "channel" => channel, "result" => result).increment(1);
}

/// One agent API call that ended with `result` (`ok` or an error code).
pub fn api_request(surface: Surface, tier: Tier, result: &'static str) {
    counter!(API_REQUESTS_TOTAL, "surface" => surface.label(), "tier" => tier.label(), "result" => result)
        .increment(1);
}

pub fn scheduler_tick(unix_seconds: f64) {
    gauge!(SCHEDULER_LAST_TICK_TIMESTAMP_SECONDS).set(unix_seconds);
}

/// A job of kind `kind` (`JobKind::slug`) failed or panicked.
pub fn job_failed(kind: &'static str) {
    counter!(JOBS_FAILED_TOTAL, "kind" => kind).increment(1);
}
