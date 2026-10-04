//! A throwaway Postgres database per test, so tests never collide or leave state behind.
//! Same pattern as `crates/store/tests/support/mod.rs` (duplicated rather than shared, since
//! `crates/app` and `crates/store` don't otherwise share a test-utility dependency).

use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, Executor, PgConnection, PgPool};

fn admin_url() -> String {
    std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://127.0.0.1/postgres".to_string())
}

/// Owns a database created for one test and drops it when the guard is dropped.
pub struct TestDb {
    name: String,
    pub pool: PgPool,
}

impl TestDb {
    pub async fn new() -> TestDb {
        let name = format!("codoseo_test_{}", uuid::Uuid::new_v4().simple());
        let mut admin = PgConnection::connect(&admin_url())
            .await
            .expect("connect to admin database");
        admin
            .execute(format!(r#"CREATE DATABASE "{name}""#).as_str())
            .await
            .expect("create test database");

        let base = url::Url::parse(&admin_url()).expect("valid admin url");
        let mut db_url = base.clone();
        db_url.set_path(&format!("/{name}"));

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(db_url.as_str())
            .await
            .expect("connect to test database");

        codoseo_store::pool::migrate(&pool)
            .await
            .expect("run migrations");

        TestDb { name, pool }
    }

    /// Inserts a minimal `sites` row (no account) and returns its id.
    pub async fn seed_site(&self, domain: &str, start_url: &str) -> uuid::Uuid {
        sqlx::query_scalar("INSERT INTO sites (domain, start_url) VALUES ($1, $2) RETURNING id")
            .bind(domain)
            .bind(start_url)
            .fetch_one(&self.pool)
            .await
            .expect("insert site")
    }

    /// Inserts a `sites` row whose `crawl_settings` includes extra JSON keys (e.g. the
    /// test-only panic seam).
    pub async fn seed_site_with_settings(
        &self,
        domain: &str,
        start_url: &str,
        settings: serde_json::Value,
    ) -> uuid::Uuid {
        sqlx::query_scalar(
            "INSERT INTO sites (domain, start_url, crawl_settings) VALUES ($1, $2, $3) \
             RETURNING id",
        )
        .bind(domain)
        .bind(start_url)
        .bind(settings)
        .fetch_one(&self.pool)
        .await
        .expect("insert site")
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let name = self.name.clone();
        // Dropping a pool is async-unfriendly in a sync Drop; block on a fresh runtime just
        // for the cleanup connection, same pattern sqlx's own test helpers use.
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("runtime for cleanup");
            rt.block_on(async move {
                if let Ok(mut admin) = PgConnection::connect(&admin_url()).await {
                    let _ = admin
                        .execute(
                            format!(r#"DROP DATABASE IF EXISTS "{name}" WITH (FORCE)"#).as_str(),
                        )
                        .await;
                }
            });
        })
        .join();
    }
}
