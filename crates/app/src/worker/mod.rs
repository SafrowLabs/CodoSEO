//! The crawl worker: claims queued crawls from Postgres, runs them end to end (crawl → checks
//! → diff → finalize), and drains in-flight work on shutdown.

pub mod budget;
pub mod run;

pub use run::{
    DEFAULT_MEMORY_BUDGET, WorkerError, address_policy_for_mode, address_policy_from_env,
    requeue_stale_sweep, resolve_limits, worker_loop, worker_loop_once,
};
