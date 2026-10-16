//! The crawl worker: claims queued crawls from Postgres, runs them end to end (crawl → checks
//! → site policy → diff → synthetic probes → finalize), and drains in-flight work on shutdown.

pub mod budget;
pub mod probes;
pub mod run;
pub mod site_policy;

pub use probes::ProbeSettings;
pub use run::{
    DEFAULT_MEMORY_BUDGET, WorkerError, WorkerSettings, address_policy_for_mode,
    address_policy_from_env, requeue_stale_sweep, resolve_limits, worker_loop, worker_loop_once,
    worker_loop_once_with,
};
