//! `codoseo web`: serves the web app until SIGTERM or Ctrl-C.

use std::net::SocketAddr;

use clap::Args;
use codoseo_web::auth::mailer::Mailer;
use codoseo_web::{AppState, Config, Mode};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use super::{CliError, EXIT_OK, Outcome};

#[derive(Debug, Args)]
pub struct WebArgs {
    /// Address to listen on (overrides CODOSEO_BIND, default 0.0.0.0:8080)
    #[arg(long)]
    pub bind: Option<SocketAddr>,
}

/// The warning for a process running on the built-in development `SECRET_KEY`, if it is.
/// Every role that stores or reads channel secrets prints it at startup.
pub fn secret_key_warning(secret_key_set: bool) -> Option<&'static str> {
    (!secret_key_set).then_some(
        "SECRET_KEY is not set; using the built-in development key. Set it so stored \
         channel secrets are protected.",
    )
}

/// Logs [`secret_key_warning`] for this process's environment.
pub fn warn_if_dev_secret_key() {
    if let Some(warning) = secret_key_warning(std::env::var("SECRET_KEY").is_ok()) {
        tracing::warn!("{warning}");
    }
}

/// Reads the web configuration and opens a lazy pool, so the web role starts (and serves its
/// 503 page) even while Postgres is down.
pub fn prepare(bind: Option<SocketAddr>) -> Result<AppState, CliError> {
    let mut config = Config::from_env().map_err(|e| CliError::msg(e.to_string()))?;
    warn_if_dev_secret_key();
    if let Some(bind) = bind {
        config.bind = bind;
        if std::env::var("BASE_URL").is_err() {
            config.base_url = url::Url::parse(&format!("http://localhost:{}", bind.port()))
                .expect("localhost url is valid");
        }
    }
    let database_url =
        std::env::var("DATABASE_URL").map_err(|_| CliError::msg("DATABASE_URL is not set"))?;
    let pool = codoseo_web::state::lazy_pool(&database_url)
        .map_err(|e| CliError::msg(format!("invalid DATABASE_URL: {e}")))?;
    let mailer = mailer_for(&config)?;
    Ok(AppState::new(pool, config, mailer))
}

/// The mailer for `SMTP_URL` and `MAIL_FROM`; a bad value stops the process at startup.
pub fn mailer_for(config: &Config) -> Result<Mailer, CliError> {
    Mailer::from_config(config.smtp_url.as_deref(), &config.mail_from)
        .map_err(|e| CliError::msg(e.to_string()))
}

pub async fn serve(state: AppState, shutdown: CancellationToken) -> Outcome {
    let listener = TcpListener::bind(state.config.bind)
        .await
        .map_err(|e| CliError::msg(format!("cannot listen on {}: {e}", state.config.bind)))?;
    let mode = match state.config.mode {
        Mode::SelfHost => "self-hosted",
        Mode::Cloud => "cloud",
    };
    tracing::info!(
        bind = %state.config.bind,
        base_url = %state.config.base_url,
        "CodoSEO web ({mode}) listening"
    );
    codoseo_web::serve(state, listener, async move { shutdown.cancelled().await })
        .await
        .map_err(|e| CliError::msg(format!("web server failed: {e}")))?;
    Ok(EXIT_OK)
}

/// Starts what the metrics need in a server role: the internal `/metrics` listener (when
/// `CODOSEO_METRICS_BIND` is set) and the pool gauges. A worker also publishes its memory budget.
pub async fn start_metrics(
    pool: &sqlx::PgPool,
    shutdown: &CancellationToken,
    worker_budget: Option<u64>,
) -> Result<(), CliError> {
    let listening = crate::telemetry::serve_metrics(shutdown)
        .await
        .map_err(CliError::msg)?;
    if listening.is_some() {
        crate::telemetry::sample_pool(pool.clone(), shutdown.clone());
        if let Some(budget) = worker_budget {
            codoseo_web::metrics::worker_memory_budget(budget);
        }
    }
    Ok(())
}

/// Cancels `token` on SIGTERM or Ctrl-C.
pub fn cancel_on_signal(token: CancellationToken) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut term =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(t) => t,
                    Err(_) => return,
                };
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        token.cancel();
    });
}

pub async fn run(args: WebArgs) -> Outcome {
    let state = prepare(args.bind)?;
    let shutdown = CancellationToken::new();
    cancel_on_signal(shutdown.clone());
    start_metrics(&state.pool, &shutdown, None).await?;
    // The scheduler runs in the web role; `CODOSEO_SCHEDULER=off` leaves it to another container.
    let scheduler = crate::scheduler::spawn(&state.pool, &state.config, &shutdown);
    let outcome = serve(state, shutdown.clone()).await;
    shutdown.cancel();
    if let Some(scheduler) = scheduler {
        let _ = scheduler.await;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_development_key_is_warned_about_only_when_secret_key_is_unset() {
        assert!(
            secret_key_warning(false)
                .unwrap()
                .contains("SECRET_KEY is not set")
        );
        assert_eq!(secret_key_warning(true), None);
    }
}
