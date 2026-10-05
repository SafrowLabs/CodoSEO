//! Logs and metrics for the server roles (`web`, `worker`, `all`, `migrate`); the CLI commands
//! never start any of this, so their output stays exactly what they print.
//!
//! Logs go to stderr through `tracing`: JSON lines when `CODOSEO_MODE=cloud` (what a log
//! shipper wants), readable text otherwise, `CODOSEO_LOG_FORMAT=json|text` overriding either.
//! The filter is `info,sqlx=warn` unless `RUST_LOG` says otherwise.
//!
//! Metrics are off unless `CODOSEO_METRICS_BIND` holds an address (for example
//! `0.0.0.0:9090`). They are served on that internal listener only, never on the public
//! router, so a reverse proxy in front of port 8080 cannot expose them. See
//! [`codoseo_web::metrics`] for what is recorded.

use std::io::IsTerminal;
use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use codoseo_web::metrics as m;
use metrics_exporter_prometheus::{
    Matcher, PrometheusBuilder, PrometheusHandle, PrometheusRecorder,
};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;

/// The log filter when `RUST_LOG` is unset or blank: info, with the query log of sqlx quieted.
pub const DEFAULT_FILTER: &str = "info,sqlx=warn";

/// Seconds a crawl may wait in the queue, in histogram buckets: from an idle queue to a day.
const WAIT_BUCKETS: [f64; 10] = [
    0.5, 2.0, 10.0, 30.0, 120.0, 600.0, 1800.0, 3600.0, 14_400.0, 86_400.0,
];

/// How often the pool gauges and the histogram buckets are refreshed.
const SAMPLE_EVERY: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Text,
}

/// The format for `CODOSEO_MODE` and `CODOSEO_LOG_FORMAT`: the override when set, else JSON in
/// the cloud and text everywhere else. An override that is neither `json` nor `text` is an
/// error naming the value.
pub fn log_format(mode: Option<&str>, over: Option<&str>) -> Result<LogFormat, String> {
    match over.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) if v.eq_ignore_ascii_case("json") => Ok(LogFormat::Json),
        Some(v) if v.eq_ignore_ascii_case("text") => Ok(LogFormat::Text),
        Some(v) => Err(format!(
            "CODOSEO_LOG_FORMAT must be json or text, got {v:?}"
        )),
        None if mode == Some("cloud") => Ok(LogFormat::Json),
        None => Ok(LogFormat::Text),
    }
}

pub fn default_filter() -> EnvFilter {
    EnvFilter::new(DEFAULT_FILTER)
}

/// The filter for the value of `RUST_LOG`: that value when it is set and parses, the default
/// otherwise. A blank value counts as unset: Compose's `RUST_LOG: ${RUST_LOG:-}` hands the
/// container an empty string, which would otherwise parse to no directives (errors only).
pub fn env_filter(rust_log: Option<&str>) -> EnvFilter {
    rust_log
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| EnvFilter::try_new(v).ok())
        .unwrap_or_else(default_filter)
}

/// A subscriber printing in `format` to `writer`, filtered by `filter`. Colour is left to the
/// caller's writer: text output is plain here (see [`init_logging`] for the terminal case).
pub fn subscriber<W>(
    format: LogFormat,
    filter: EnvFilter,
    writer: W,
) -> Box<dyn Subscriber + Send + Sync>
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    subscriber_with_ansi(format, filter, writer, false)
}

fn subscriber_with_ansi<W>(
    format: LogFormat,
    filter: EnvFilter,
    writer: W,
    ansi: bool,
) -> Box<dyn Subscriber + Send + Sync>
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    // The Sentry layer does nothing while no Sentry client is bound.
    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(sentry::integrations::tracing::layer());
    match format {
        LogFormat::Json => {
            Box::new(registry.with(tracing_subscriber::fmt::layer().json().with_writer(writer)))
        }
        LogFormat::Text => Box::new(
            registry.with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(ansi)
                    .with_writer(writer),
            ),
        ),
    }
}

/// What [`init_logging`] sets up and must outlive the role: dropping it flushes Sentry.
pub struct Telemetry {
    sentry: Option<sentry::ClientInitGuard>,
}

impl Telemetry {
    /// Whether errors are being reported to Sentry (`SENTRY_DSN` was set and valid).
    pub fn sentry_enabled(&self) -> bool {
        self.sentry.is_some()
    }
}

/// The Sentry DSN in `value` (`SENTRY_DSN`): `None` when unset or blank, an error when it
/// doesn't parse. Without one nothing is initialised and nothing leaves the process.
pub fn sentry_dsn(value: Option<&str>) -> Result<Option<sentry::types::Dsn>, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        // The error text of a bad DSN never echoes it: a DSN carries a secret key.
        Some(v) => v
            .parse()
            .map(Some)
            .map_err(|_| "SENTRY_DSN is not a valid Sentry DSN; error reporting is off".to_owned()),
    }
}

/// Starts Sentry when `SENTRY_DSN` is set. Panics are captured by its panic integration and
/// `tracing` errors become events (warnings and below only add breadcrumbs); personal data is
/// not attached and performance tracing is not enabled.
fn init_sentry(mode: Option<&str>) -> Result<Option<sentry::ClientInitGuard>, String> {
    let Some(dsn) = sentry_dsn(std::env::var("SENTRY_DSN").ok().as_deref())? else {
        return Ok(None);
    };
    let environment = std::env::var("SENTRY_ENVIRONMENT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| mode.unwrap_or("selfhost").to_owned());
    let mut options = sentry::ClientOptions::default();
    options.dsn = Some(dsn);
    options.release = Some(concat!("codoseo@", env!("CARGO_PKG_VERSION")).into());
    options.environment = Some(environment.into());
    options.send_default_pii = false;
    Ok(Some(sentry::init(options)))
}

/// Installs the process's logger on stderr (and Sentry, when configured). Safe to call twice
/// (the second call keeps the first logger), so tests that start a role in-process don't fight
/// over the global subscriber. Hold the result until the role ends.
pub fn init_logging() -> Telemetry {
    let mode = std::env::var("CODOSEO_MODE").ok();
    let over = std::env::var("CODOSEO_LOG_FORMAT").ok();
    let (format, bad_format) = match log_format(mode.as_deref(), over.as_deref()) {
        Ok(format) => (format, None),
        Err(e) => (
            log_format(mode.as_deref(), None).unwrap_or(LogFormat::Text),
            Some(e),
        ),
    };
    let (sentry, bad_sentry) = match init_sentry(mode.as_deref()) {
        Ok(guard) => (guard, None),
        Err(e) => (None, Some(e)),
    };
    let filter = env_filter(std::env::var("RUST_LOG").ok().as_deref());
    let ansi = format == LogFormat::Text && std::io::stderr().is_terminal();
    let subscriber = subscriber_with_ansi(format, filter, std::io::stderr, ansi);
    if tracing::subscriber::set_global_default(subscriber).is_ok() {
        if let Some(e) = bad_format {
            tracing::warn!("{e}; using the default");
        }
        if let Some(e) = bad_sentry {
            tracing::warn!("{e}");
        }
    }
    Telemetry { sentry }
}

/// The Prometheus recorder, with queue-wait buckets. Tests drive one through
/// `metrics::with_local_recorder`; the binary installs one globally ([`install_metrics`]).
pub fn build_recorder() -> PrometheusRecorder {
    PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(m::CRAWL_QUEUE_WAIT_SECONDS.to_owned()),
            &WAIT_BUCKETS,
        )
        .expect("buckets are not empty")
        .build_recorder()
}

/// The internal listener's router: `/metrics` and nothing else.
pub fn metrics_router(handle: PrometheusHandle) -> Router {
    Router::new().route("/metrics", get(move || async move { handle.render() }))
}

/// The address `CODOSEO_METRICS_BIND` names, `None` when unset or blank.
pub fn metrics_bind(value: Option<&str>) -> Result<Option<SocketAddr>, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => v.parse().map(Some).map_err(|e| {
            format!("CODOSEO_METRICS_BIND must be an address like 0.0.0.0:9090, got {v:?}: {e}")
        }),
    }
}

/// The handle of the process-wide recorder, set once by [`install_metrics`].
static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Starts the metrics listener when `CODOSEO_METRICS_BIND` is set: installs the recorder,
/// describes every metric and serves `/metrics` until `shutdown`. Returns the address it is
/// listening on, or `None` when metrics are off. A bad address or a port that is taken is an
/// error, so a typo shows up at startup rather than as a silent gap in the graphs.
pub async fn serve_metrics(shutdown: &CancellationToken) -> Result<Option<SocketAddr>, String> {
    let Some(bind) = metrics_bind(std::env::var("CODOSEO_METRICS_BIND").ok().as_deref())? else {
        return Ok(None);
    };
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| format!("cannot listen for metrics on {bind}: {e}"))?;
    let handle = HANDLE
        .get_or_init(|| {
            let recorder = build_recorder();
            let handle = recorder.handle();
            // A second recorder (in-process tests) keeps the first; its handle still renders.
            let _ = ::metrics::set_global_recorder(recorder);
            m::register();
            handle
        })
        .clone();
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let app = metrics_router(handle.clone());
    let stop = shutdown.clone();
    tokio::spawn(async move {
        let served = axum::serve(listener, app)
            .with_graceful_shutdown(async move { stop.cancelled().await })
            .await;
        if let Err(e) = served {
            tracing::error!(error = %e, "the metrics listener stopped");
        }
    });
    // Histograms keep their buckets tidy only when asked to.
    let stop = shutdown.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(SAMPLE_EVERY) => handle.run_upkeep(),
                _ = stop.cancelled() => break,
            }
        }
    });
    tracing::info!(%addr, "metrics are served on the internal listener");
    Ok(Some(addr))
}

/// Samples `pool` into the `db_pool_connections` gauges until `shutdown`.
pub fn sample_pool(pool: PgPool, shutdown: CancellationToken) {
    tokio::spawn(async move {
        loop {
            let size = pool.size();
            let idle = u32::try_from(pool.num_idle()).unwrap_or(u32::MAX).min(size);
            m::db_pool(idle, size - idle);
            tokio::select! {
                _ = tokio::time::sleep(SAMPLE_EVERY) => {}
                _ = shutdown.cancelled() => break,
            }
        }
    });
}
