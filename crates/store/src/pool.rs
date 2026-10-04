//! Connecting to Postgres and applying migrations. Spec section 11: "small pools (about 5 each)".

use sqlx::PgPool;
use sqlx::migrate::MigrateError;
use sqlx::postgres::PgPoolOptions;

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
