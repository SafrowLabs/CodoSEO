//! `codoseo all`: the self-hosted single process. Applies migrations, then runs the web app and
//! a crawl worker, a job runner and the scheduler side by side until SIGTERM or Ctrl-C.

use std::net::SocketAddr;

use clap::Args;
use codoseo_store::crawl_queue::CrawlQueue;
use codoseo_store::jobs::JobQueue;
use tokio_util::sync::CancellationToken;

use super::web::{cancel_on_signal, prepare, serve};
use super::{CliError, Outcome};
use crate::jobs::{JobContext, job_loop};
use crate::worker::{
    DEFAULT_MEMORY_BUDGET, address_policy_from_env, requeue_stale_sweep, worker_loop,
};

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
    let jobs = JobContext::from_config(pool.clone(), state.mailer.clone(), &state.config);
    let job_runner = tokio::spawn(job_loop(
        jobs,
        job_queue.clone(),
        worker_id.clone(),
        shutdown.clone(),
    ));
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
                address_policy_from_env(),
                shutdown,
            )
            .await;
        })
    };
    let scheduler = crate::scheduler::spawn(&state.pool, &state.config, &shutdown);
    let outcome = serve(state, shutdown.clone()).await;
    shutdown.cancel();
    if let Some(scheduler) = scheduler {
        let _ = scheduler.await;
    }
    let _ = worker.await;
    let _ = job_runner.await;
    sweep.abort();
    outcome
}
