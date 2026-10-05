//! Connecting to Postgres and applying migrations. Spec section 11: "small pools (about 5 each)".

use sqlx::migrate::MigrateError;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Postgres, Transaction};

/// Opens a pool sized for the shared-Postgres constraints in the architecture spec.
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
}

/// Applies every migration in `crates/store/migrations` that hasn't run yet.
pub async fn migrate(pool: &PgPool) -> Result<(), MigrateError> {
    sqlx::migrate!().run(pool).await
}

/// The `statement_timeout` of a [`begin_long`] transaction. The web role is created with a short
/// one (5 s, see `deploy/postgres-role.sql`) so a stuck request query cannot hold a connection.
/// A few queries are legitimately slower on a busy server (a batch of a 50,000-page CSV export,
/// the admin funnel's scan of a month of events), so they lift the limit for their own
/// transaction only.
pub const LONG_STATEMENT_TIMEOUT: &str = "120s";

/// Starts a transaction whose statements may run up to [`LONG_STATEMENT_TIMEOUT`] (`SET
/// LOCAL`, so the pooled connection goes back with its usual limit). Meant for read-only work.
pub async fn begin_long(pool: &PgPool) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(&format!(
        "SET LOCAL statement_timeout = '{LONG_STATEMENT_TIMEOUT}'"
    ))
    .execute(&mut *tx)
    .await?;
    Ok(tx)
}
