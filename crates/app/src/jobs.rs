//! The job runner: drains the `jobs` table (`send_email`, `cleanup`, and later `send_alert` and
//! `send_digest`) beside the crawl loop. One job at a time; a failure or a panic fails only that
//! job, which then retries with the queue's backoff.

use std::time::Duration;

use codoseo_core::crawl::AddressPolicy;
use codoseo_notify::{ChannelKey, Email, Mailer};
use codoseo_store::jobs::{ClaimedJob, JobKind, JobQueue};
use codoseo_web::Config;
use codoseo_web::Mode;
use serde::Deserialize;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use url::Url;

/// How long an idle [`job_loop`] waits between `claim()` polls.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Everything a job handler may need.
#[derive(Clone)]
pub struct JobContext {
    pub pool: PgPool,
    pub mailer: Mailer,
    pub channel_key: ChannelKey,
    /// The public address of the app, for links in messages.
    pub base_url: Url,
    /// The cloud refuses private and internal addresses for user-supplied URLs.
    pub policy: AddressPolicy,
    /// For outgoing deliveries (Slack, Discord, webhooks). Never follows redirects.
    pub http: reqwest::Client,
}

impl JobContext {
    /// The context for a process configured by `config` (the same `SECRET_KEY`, `BASE_URL`,
    /// `CODOSEO_MODE` the web role reads), sending mail through `mailer`.
    pub fn from_config(pool: PgPool, mailer: Mailer, config: &Config) -> JobContext {
        JobContext {
            pool,
            mailer,
            channel_key: ChannelKey::derive(&config.secret_key),
            base_url: config.base_url.clone(),
            policy: match config.mode {
                Mode::Cloud => AddressPolicy::Public,
                Mode::SelfHost => AddressPolicy::AllowPrivate,
            },
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("CodoSEO/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("http client builds"),
        }
    }
}

/// Claims and runs jobs until `shutdown` is cancelled. A claimed job always runs to completion
/// first; an idle loop polls every [`POLL_INTERVAL`].
pub async fn job_loop(
    ctx: JobContext,
    queue: JobQueue,
    worker_id: String,
    shutdown: CancellationToken,
) {
    while !shutdown.is_cancelled() {
        match queue.claim(&worker_id).await {
            Ok(Some(job)) => settle(&ctx, &queue, job).await,
            Ok(None) => idle(&shutdown).await,
            Err(e) => {
                tracing::warn!(error = %e, "could not claim a job");
                idle(&shutdown).await;
            }
        }
    }
}

async fn idle(shutdown: &CancellationToken) {
    tokio::select! {
        _ = tokio::time::sleep(POLL_INTERVAL) => {}
        _ = shutdown.cancelled() => {}
    }
}

/// Runs one claimed job in its own task, so a panic fails that job and nothing else, then
/// records the outcome.
async fn settle(ctx: &JobContext, queue: &JobQueue, job: ClaimedJob) {
    let id = job.id;
    let kind = job.kind;
    let task_ctx = ctx.clone();
    let outcome = match tokio::spawn(async move { run_job(&task_ctx, job).await }).await {
        Ok(result) => result,
        Err(join) if join.is_panic() => Err("the job handler panicked".to_owned()),
        Err(_) => Err("the job was cancelled".to_owned()),
    };
    let recorded = match outcome {
        Ok(()) => queue.complete(id).await,
        Err(error) => {
            tracing::warn!(job = %id, kind = kind.slug(), %error, "job failed");
            queue.retry(id, &error).await
        }
    };
    if let Err(e) = recorded {
        tracing::error!(job = %id, error = %e, "could not record the job outcome");
    }
}

/// Dispatches a claimed job to its handler. The error text lands in `jobs.last_error`.
pub async fn run_job(ctx: &JobContext, job: ClaimedJob) -> Result<(), String> {
    // Test-only panic seam, like the crawl worker's: proves a panicking handler is isolated to
    // its own job. Never set by production code.
    if job
        .payload
        .get("test_panic_before_job")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        panic!("test_panic_before_job");
    }
    match job.kind {
        JobKind::SendEmail => send_email(ctx, &job.payload).await,
        JobKind::Cleanup => cleanup(ctx).await,
        JobKind::SendAlert => Err("send_alert is not implemented".to_owned()),
        JobKind::SendDigest => Err("send_digest is not implemented".to_owned()),
    }
}

#[derive(Deserialize)]
struct SendEmailPayload {
    to: String,
    subject: String,
    text: String,
    #[serde(default)]
    html: Option<String>,
}

async fn send_email(ctx: &JobContext, payload: &serde_json::Value) -> Result<(), String> {
    let p: SendEmailPayload = serde_json::from_value(payload.clone())
        .map_err(|e| format!("send_email payload is invalid: {e}"))?;
    ctx.mailer
        .send(Email {
            to: p.to,
            subject: p.subject,
            text: p.text,
            html: p.html,
        })
        .await
        .map_err(|e| e.to_string())
}

/// The daily cleanup: retention for crawl history, unclaimed audits, tokens, sessions and old
/// failed jobs. `RETENTION_DAYS_SELFHOST` overrides the self-hosted history window.
async fn cleanup(ctx: &JobContext) -> Result<(), String> {
    let days = self_host_history_days(std::env::var("RETENTION_DAYS_SELFHOST").ok().as_deref())?;
    let report = codoseo_store::retention::run(&ctx.pool, days)
        .await
        .map_err(|e| format!("cleanup failed: {e}"))?;
    tracing::info!(?report, "cleanup finished");
    Ok(())
}

fn self_host_history_days(value: Option<&str>) -> Result<Option<u32>, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => v.parse().map(Some).map_err(|_| {
            format!("RETENTION_DAYS_SELFHOST must be a whole number of days, got {v:?}")
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_days_come_from_a_whole_number() {
        assert_eq!(self_host_history_days(None), Ok(None));
        assert_eq!(self_host_history_days(Some(" ")), Ok(None));
        assert_eq!(self_host_history_days(Some("90")), Ok(Some(90)));
        assert!(self_host_history_days(Some("a year")).is_err());
    }
}
