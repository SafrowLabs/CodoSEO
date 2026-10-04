//! The crawl worker: claims queued crawls from Postgres, runs them end to end (crawl → checks
//! → diff → finalize), and drains in-flight work on shutdown.

pub mod budget;
pub mod run;

pub use run::{WorkerError, worker_loop, worker_loop_once};
