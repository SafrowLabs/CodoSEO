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

/// Reads the web configuration and opens a lazy pool, so the web role starts (and serves its
/// 503 page) even while Postgres is down.
pub fn prepare(bind: Option<SocketAddr>) -> Result<AppState, CliError> {
    let mut config = Config::from_env().map_err(|e| CliError::msg(e.to_string()))?;
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
    println!(
        "CodoSEO web ({mode}) listening on {} → {}",
        state.config.bind, state.config.base_url
    );
    codoseo_web::serve(state, listener, async move { shutdown.cancelled().await })
        .await
        .map_err(|e| CliError::msg(format!("web server failed: {e}")))?;
    Ok(EXIT_OK)
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
    // The scheduler runs in the web role; `CODOSEO_SCHEDULER=off` leaves it to another container.
    let scheduler = crate::scheduler::spawn(&state.pool, &state.config, &shutdown);
    let outcome = serve(state, shutdown.clone()).await;
    shutdown.cancel();
    if let Some(scheduler) = scheduler {
        let _ = scheduler.await;
    }
    outcome
}
