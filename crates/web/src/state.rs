//! Shared state handed to every handler.

use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::auth::mailer::Mailer;
use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub mailer: Mailer,
    /// For outgoing calls (GitHub OAuth).
    pub http: reqwest::Client,
}

impl AppState {
    pub fn new(pool: PgPool, config: Config, mailer: Mailer) -> AppState {
        AppState {
            pool,
            config: Arc::new(config),
            mailer,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .user_agent(concat!("CodoSEO/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("http client builds"),
        }
    }
}

/// A pool that connects on first use with a short acquire timeout, so the web role starts and
/// serves its 503 page while Postgres is down instead of refusing to boot.
pub fn lazy_pool(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(3))
        .connect_lazy(database_url)
}
