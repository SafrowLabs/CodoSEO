//! T4.1: migrations apply cleanly, and a smoke insert works for every table, in FK order.

mod support;

use sqlx::Row;
use support::TestDb;

#[tokio::test]
async fn migrations_apply_and_every_table_accepts_a_row() {
    let db = TestDb::new().await;
    let pool = &db.pool;

    let account_id: uuid::Uuid =
        sqlx::query("INSERT INTO accounts (email) VALUES ($1) RETURNING id")
            .bind("ana@example.com")
            .fetch_one(pool)
            .await
            .expect("insert account")
            .get(0);

    let site_id: uuid::Uuid = sqlx::query(
        "INSERT INTO sites (account_id, domain, start_url) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(account_id)
    .bind("example.com")
    .bind("https://example.com/")
    .fetch_one(pool)
    .await
    .expect("insert site")
    .get(0);

    let crawl_id: uuid::Uuid = sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority) VALUES ($1, $2, 'manual', 2) RETURNING id",
    )
    .bind(site_id)
    .bind("example.com")
    .fetch_one(pool)
    .await
    .expect("insert crawl")
    .get(0);

    sqlx::query(
        "INSERT INTO pages (crawl_id, site_id, url, url_hash, status, indexability) \
         VALUES ($1, $2, $3, $4, 200, 'indexable')",
    )
    .bind(crawl_id)
    .bind(site_id)
    .bind("https://example.com/")
    .bind(codoseo_store::hash::to_db(42))
    .execute(pool)
    .await
    .expect("insert page");

    sqlx::query("INSERT INTO inlinks (crawl_id, target_url_hash, from_url) VALUES ($1, $2, $3)")
        .bind(crawl_id)
        .bind(codoseo_store::hash::to_db(42))
        .bind("https://example.com/other")
        .execute(pool)
        .await
        .expect("insert inlink");

    sqlx::query("INSERT INTO site_files (crawl_id, site_id, robots_status) VALUES ($1, $2, 200)")
        .bind(crawl_id)
        .bind(site_id)
        .execute(pool)
        .await
        .expect("insert site_file");

    sqlx::query(
        "INSERT INTO changes (crawl_id, site_id, kind, severity, before_value, after_value) \
         VALUES ($1, $2, 'new_url', 'notice', '', 'https://example.com/')",
    )
    .bind(crawl_id)
    .bind(site_id)
    .execute(pool)
    .await
    .expect("insert change");

    let channel_id: uuid::Uuid = sqlx::query(
        "INSERT INTO alert_channels (account_id, kind, target_encrypted) VALUES ($1, 'email', $2) RETURNING id",
    )
    .bind(account_id)
    .bind(b"encrypted".as_slice())
    .fetch_one(pool)
    .await
    .expect("insert alert_channel")
    .get(0);

    sqlx::query("INSERT INTO alert_rules (site_id, channel_id, mode) VALUES ($1, $2, 'instant')")
        .bind(site_id)
        .bind(channel_id)
        .execute(pool)
        .await
        .expect("insert alert_rule");

    sqlx::query("INSERT INTO jobs (kind, payload) VALUES ('send_alert', '{}'::jsonb)")
        .execute(pool)
        .await
        .expect("insert job");

    sqlx::query("INSERT INTO events (account_id, site_id, kind) VALUES ($1, $2, 'audit_started')")
        .bind(account_id)
        .bind(site_id)
        .execute(pool)
        .await
        .expect("insert event");

    sqlx::query(
        "INSERT INTO login_tokens (account_id, purpose, token_hash, expires_at) \
         VALUES ($1, 'magic_link', $2, now() + interval '15 minutes')",
    )
    .bind(account_id)
    .bind(b"tokenhash".as_slice())
    .execute(pool)
    .await
    .expect("insert login_token");

    sqlx::query(
        "INSERT INTO sessions (account_id, session_hash, expires_at) \
         VALUES ($1, $2, now() + interval '30 days')",
    )
    .bind(account_id)
    .bind(b"sessionhash".as_slice())
    .execute(pool)
    .await
    .expect("insert session");

    sqlx::query("INSERT INTO api_keys (account_id, name, key_hash) VALUES ($1, 'ci', $2)")
        .bind(account_id)
        .bind(b"keyhash".as_slice())
        .execute(pool)
        .await
        .expect("insert api_key");
}
