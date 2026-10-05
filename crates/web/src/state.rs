//! Shared state handed to every handler.

use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use codoseo_core::crawl::AddressPolicy;
use codoseo_notify::{ChannelKey, GuardedHttp};

use crate::auth::mailer::Mailer;
use crate::config::{Config, Mode};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub mailer: Mailer,
    /// For outgoing calls (GitHub OAuth).
    pub http: reqwest::Client,
    /// Encrypts and decrypts alert channel targets (derived from `SECRET_KEY`).
    pub channel_key: ChannelKey,
    /// For "Send test" and for checking a webhook address when it is saved: the same guarded
    /// client the job runner delivers with (the cloud refuses private and internal addresses,
    /// self-hosted allows them).
    pub notify_http: GuardedHttp,
}

impl AppState {
    pub fn new(pool: PgPool, config: Config, mailer: Mailer) -> AppState {
        let policy = match config.mode {
            Mode::Cloud => AddressPolicy::Public,
            Mode::SelfHost => AddressPolicy::AllowPrivate,
        };
        AppState {
            pool,
            channel_key: ChannelKey::derive(&config.secret_key),
            notify_http: GuardedHttp::new(policy).expect("http client builds"),
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
