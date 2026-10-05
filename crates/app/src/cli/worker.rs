//! `codoseo worker`: claims crawls from Postgres and runs them until `SIGTERM`.

use clap::Args;
use codoseo_store::crawl_queue::CrawlQueue;
use codoseo_store::jobs::JobQueue;
use tokio_util::sync::CancellationToken;

use crate::jobs::{JobContext, job_loop};
use crate::worker::{
    DEFAULT_MEMORY_BUDGET, address_policy_from_env, requeue_stale_sweep, worker_loop,
};

use super::web::{mailer_for, warn_if_dev_secret_key};
use super::{CliError, EXIT_OK, Outcome};

#[derive(Debug, Args)]
pub struct WorkerArgs {}

pub async fn run(_args: WorkerArgs) -> Outcome {
    let database_url =
        std::env::var("DATABASE_URL").map_err(|_| CliError::msg("DATABASE_URL is not set"))?;
    let pool = codoseo_store::pool::connect(&database_url)
        .await
        .map_err(|e| CliError::msg(format!("could not connect to Postgres: {e}")))?;

    // The same SMTP_URL, MAIL_FROM, SECRET_KEY, BASE_URL and CODOSEO_MODE the web role reads.
    // SECRET_KEY and BASE_URL are required in the cloud; elsewhere the dev defaults apply.
    let config = codoseo_web::Config::from_env().map_err(|e| CliError::msg(e.to_string()))?;
    warn_if_dev_secret_key();
    let jobs = JobContext::from_config(pool.clone(), mailer_for(&config)?, &config);

    let crawl_queue = CrawlQueue::new(pool.clone());
    let job_queue = JobQueue::new(pool.clone());
    let worker_id = format!("{}-{}", hostname(), std::process::id());

    let shutdown = CancellationToken::new();
    #[cfg(unix)]
    {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            let mut term =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(term) => term,
                    Err(_) => return,
                };
            term.recv().await;
            shutdown.cancel();
        });
    }

    println!("worker {worker_id} started");
    let sweep = tokio::spawn(requeue_stale_sweep(crawl_queue.clone(), shutdown.clone()));
    let job_runner = tokio::spawn(job_loop(
        jobs,
        job_queue.clone(),
        worker_id.clone(),
        shutdown.clone(),
    ));
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
    sweep.abort();
    // The crawl loop returns once shutdown is cancelled; the job runner finishes its current job.
    let _ = job_runner.await;
    Ok(EXIT_OK)
}

fn hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "codoseo-worker".to_owned())
}
