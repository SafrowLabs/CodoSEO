//! A throwaway Postgres database per test, so tests never collide or leave state behind.
//!
//! `TEST_DATABASE_URL` (falling back to a local default) must point at a server with
//! permission to `CREATE DATABASE`; each test connects there only to create/drop its own
//! database, then does its real work against that database.

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
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let name = self.name.clone();
        // Dropping a pool is async-unfriendly in a sync Drop; block on a fresh runtime
        // just for the cleanup connection, same pattern sqlx's own test helpers use.
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
