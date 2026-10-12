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
    // 14 data tables from 0001, instance_settings from 0002, billing_events from 0008,
    // api_usage from 0010, ai_reports and ai_incidents from 0011, and sqlx's own
    // _sqlx_migrations bookkeeping table.
    assert_eq!(count, 20);

    // 0012: the per-site switch for synthetic agent fetch probes, on unless switched off.
    let (nullable, default): (String, Option<String>) = sqlx::query_as(
        "SELECT is_nullable, column_default FROM information_schema.columns \
         WHERE table_name = 'sites' AND column_name = 'ai_probes'",
    )
    .fetch_one(&mut check)
    .await
    .expect("sites.ai_probes exists");
    assert_eq!(nullable, "NO");
    assert_eq!(default.as_deref(), Some("true"));
    let kind: bool = sqlx::query_scalar(
        "SELECT 'ai_fetch_blocked' = ANY(enum_range(NULL::change_kind)::text[])",
    )
    .fetch_one(&mut check)
    .await
    .expect("read change_kind");
    assert!(kind, "change_kind has ai_fetch_blocked");

    // 0013: the content and llms.txt change kinds.
    for slug in ["ai_content_missing", "ai_llms_txt_unavailable"] {
        let has: bool =
            sqlx::query_scalar("SELECT $1 = ANY(enum_range(NULL::change_kind)::text[])")
                .bind(slug)
                .fetch_one(&mut check)
                .await
                .expect("read change_kind");
        assert!(has, "change_kind has {slug}");
    }
    drop(check);

    admin
        .execute(format!(r#"DROP DATABASE "{name}" WITH (FORCE)"#).as_str())
        .await
        .expect("drop test database");
}
