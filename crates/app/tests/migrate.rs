//! `codoseo migrate` against a real, throwaway Postgres database.

use assert_cmd::cargo::cargo_bin_cmd;
use sqlx::{Connection, Executor, PgConnection, Row};

fn admin_url() -> String {
    std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://127.0.0.1/postgres".into())
}

#[tokio::test]
async fn migrate_creates_every_table() {
    let name = format!("codoseo_cli_test_{}", uuid::Uuid::new_v4().simple());
    let mut admin = PgConnection::connect(&admin_url())
        .await
        .expect("connect to admin database");
    admin
        .execute(format!(r#"CREATE DATABASE "{name}""#).as_str())
        .await
        .expect("create test database");

    let mut db_url = url::Url::parse(&admin_url()).expect("valid admin url");
    db_url.set_path(&format!("/{name}"));

    cargo_bin_cmd!("codoseo")
        .arg("migrate")
        .env("DATABASE_URL", db_url.as_str())
        .assert()
        .success()
        .stdout(predicates::str::contains("migrations applied"));

    let mut check = PgConnection::connect(db_url.as_str())
        .await
        .expect("connect to migrated database");
    let count: i64 =
        sqlx::query("SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'")
            .fetch_one(&mut check)
            .await
            .expect("count tables")
            .get(0);
    // 14 data tables + sqlx's own _sqlx_migrations bookkeeping table.
    assert_eq!(count, 15);
    drop(check);

    admin
        .execute(format!(r#"DROP DATABASE "{name}" WITH (FORCE)"#).as_str())
        .await
        .expect("drop test database");
}
