//! `codoseo all`: the self-hosted single process. Applies migrations, then runs the web app and
//! a crawl worker side by side until SIGTERM or Ctrl-C.

use std::net::SocketAddr;

use clap::Args;
use codoseo_store::crawl_queue::CrawlQueue;
use codoseo_store::jobs::JobQueue;
use tokio_util::sync::CancellationToken;

use super::web::{cancel_on_signal, prepare, serve};
use super::{CliError, Outcome};
use crate::worker::{DEFAULT_MEMORY_BUDGET, requeue_stale_sweep, worker_loop};

#[derive(Debug, Args)]
pub struct AllArgs {
    /// Address to listen on (overrides CODOSEO_BIND, default 0.0.0.0:8080)
    #[arg(long)]
    pub bind: Option<SocketAddr>,
}

pub async fn run(args: AllArgs) -> Outcome {
    let state = prepare(args.bind)?;
    codoseo_store::pool::migrate(&state.pool)
        .await
        .map_err(|e| CliError::msg(format!("migration failed: {e}")))?;

    let shutdown = CancellationToken::new();
    cancel_on_signal(shutdown.clone());

    let pool = state.pool.clone();
    let crawl_queue = CrawlQueue::new(pool.clone());
    let job_queue = JobQueue::new(pool.clone());
    let worker_id = format!(
        "{}-{}",
        std::env::var("HOSTNAME").unwrap_or_else(|_| "codoseo".to_owned()),
        std::process::id()
    );
    let sweep = tokio::spawn(requeue_stale_sweep(crawl_queue.clone(), shutdown.clone()));
    let worker = {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            worker_loop(
                &pool,
                &crawl_queue,
                &job_queue,
                &worker_id,
                DEFAULT_MEMORY_BUDGET,
                shutdown,
            )
            .await;
        })
    };
    let outcome = serve(state, shutdown.clone()).await;
    shutdown.cancel();
    let _ = worker.await;
    sweep.abort();
    outcome
}
