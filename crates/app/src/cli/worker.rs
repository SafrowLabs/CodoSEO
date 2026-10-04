//! `codoseo worker`: claims crawls from Postgres and runs them until `SIGTERM`.

use clap::Args;
use codoseo_store::crawl_queue::CrawlQueue;
use codoseo_store::jobs::JobQueue;
use tokio_util::sync::CancellationToken;

use crate::worker::worker_loop;

use super::{CliError, EXIT_OK, Outcome};

#[derive(Debug, Args)]
pub struct WorkerArgs {}

pub async fn run(_args: WorkerArgs) -> Outcome {
    let database_url =
        std::env::var("DATABASE_URL").map_err(|_| CliError::msg("DATABASE_URL is not set"))?;
    let pool = codoseo_store::pool::connect(&database_url)
        .await
        .map_err(|e| CliError::msg(format!("could not connect to Postgres: {e}")))?;

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
    worker_loop(&pool, &crawl_queue, &job_queue, &worker_id, shutdown).await;
    Ok(EXIT_OK)
}

fn hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "codoseo-worker".to_owned())
}
