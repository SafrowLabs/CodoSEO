//! Migration 0002's `email_canonical` backfill uses the web app's canonical rules and never
//! fails on accounts that collapse to the same key.

use std::borrow::Cow;

use sqlx::migrate::Migrator;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, Executor, PgConnection};

fn admin_url() -> String {
    std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://127.0.0.1/postgres".to_string())
}

#[tokio::test]
async fn backfill_matches_the_canonical_rules_and_skips_collisions() {
    let name = format!("codoseo_test_{}", uuid::Uuid::new_v4().simple());
    let mut admin = PgConnection::connect(&admin_url()).await.unwrap();
    admin
        .execute(format!(r#"CREATE DATABASE "{name}""#).as_str())
        .await
        .unwrap();
    let mut url = url::Url::parse(&admin_url()).unwrap();
    url.set_path(&format!("/{name}"));
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(url.as_str())
        .await
        .unwrap();

    // Only 0001: accounts as M4 left them, with no canonical column yet.
    let all: Migrator = sqlx::migrate!();
    let first = Migrator {
        migrations: Cow::Owned(all.migrations[..1].to_vec()),
        ..sqlx::migrate!()
    };
    first.run(&pool).await.unwrap();
    for (email, age) in [
        ("A.Na@Gmail.com", 3),
        ("ana+seo@gmail.com", 2),
        ("First.Last+news@Example.com", 1),
    ] {
        sqlx::query(
            "INSERT INTO accounts (email, created_at) VALUES ($1, now() - make_interval(days => $2))",
        )
        .bind(email)
        .bind(age)
        .execute(&pool)
        .await
        .unwrap();
    }

    all.run(&pool)
        .await
        .expect("0002 applies over existing accounts");

    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT email, email_canonical FROM accounts ORDER BY created_at")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![
            ("A.Na@Gmail.com".into(), Some("ana@gmail.com".into())),
            // Same person, newer account: left without a key, found by email at sign-in.
            ("ana+seo@gmail.com".into(), None),
            (
                "First.Last+news@Example.com".into(),
                Some("first.last@example.com".into())
            ),
        ]
    );

    pool.close().await;
    admin
        .execute(format!(r#"DROP DATABASE "{name}" WITH (FORCE)"#).as_str())
        .await
        .unwrap();
}
