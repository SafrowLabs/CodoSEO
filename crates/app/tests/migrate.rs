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
    // api_usage from 0010, ai_reports and ai_incidents from 0011, ai_rechecks from 0014,
    // deploy_hooks from 0016, crawl_stats and crawled_websites from 0017, policy_history from
    // 0019, and sqlx's own _sqlx_migrations bookkeeping table.
    assert_eq!(count, 25);

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

    // 0014: the repair columns, the recheck table and the job kind that runs one.
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns \
         WHERE table_name = 'ai_incidents' AND column_name = ANY($1) ORDER BY column_name",
    )
    .bind(vec![
        "before",
        "accepted_at",
        "accepted_by",
        "snoozed_until",
        "recovered_at",
        "verified_at",
        "resolved_by",
        "snooze_escalated",
    ])
    .fetch_all(&mut check)
    .await
    .expect("read ai_incidents columns");
    assert_eq!(columns.len(), 8, "{columns:?}");
    let rechecks: Vec<String> = sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns \
         WHERE table_name = 'ai_rechecks' ORDER BY ordinal_position",
    )
    .fetch_all(&mut check)
    .await
    .expect("read ai_rechecks columns");
    assert_eq!(
        rechecks,
        [
            "id",
            "site_id",
            "incident_id",
            "origin",
            "requested_by",
            "status",
            "due_at",
            "started_at",
            "finished_at",
            "outcome",
            "failure",
            "observations",
            "created_at",
        ]
    );
    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT indexname::text FROM pg_indexes WHERE tablename = 'ai_rechecks' ORDER BY indexname",
    )
    .fetch_all(&mut check)
    .await
    .expect("read ai_rechecks indexes");
    assert_eq!(
        indexes,
        [
            "ai_rechecks_active_idx",
            "ai_rechecks_pkey",
            "ai_rechecks_site_idx"
        ]
    );
    let job: bool =
        sqlx::query_scalar("SELECT 'ai_recheck' = ANY(enum_range(NULL::job_kind)::text[])")
            .fetch_one(&mut check)
            .await
            .expect("read job_kind");
    assert!(job, "job_kind has ai_recheck");

    // 0015: the cloud plans are Free, Lite and Pro.
    let plans: Vec<String> = sqlx::query_scalar("SELECT unnest(enum_range(NULL::plan))::text")
        .fetch_all(&mut check)
        .await
        .expect("read plan");
    assert_eq!(plans, ["free", "lite", "pro", "self_hosted"]);
    drop(check);

    admin
        .execute(format!(r#"DROP DATABASE "{name}" WITH (FORCE)"#).as_str())
        .await
        .expect("drop test database");
}

/// Accounts made before 0015 keep what they pay for: the old $9 Pro becomes Lite and the old
/// Agency becomes Pro. An Agency account over the new Pro's 10 sites keeps its 10 oldest
/// monitored, and nothing is deleted.
#[tokio::test]
async fn the_plan_rename_maps_old_pro_to_lite_and_agency_to_pro() {
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
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(db_url.as_str())
        .await
        .expect("connect to test database");

    // Everything up to 0014, as on a server that hasn't deployed 0015 yet. (`migrations` is a
    // hidden field of sqlx's migrator, and the only way to stop short of the newest.)
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../store/migrations");
    let mut before = sqlx::migrate::Migrator::new(dir)
        .await
        .expect("read migrations");
    before.migrations = before
        .migrations
        .iter()
        .filter(|m| m.version < 15)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    before.run(&pool).await.expect("migrate to 0014");

    let mut ids = Vec::new();
    for (email, plan) in [
        ("free@example.test", "free"),
        ("pro@example.test", "pro"),
        ("agency@example.test", "agency"),
        ("self@example.test", "self_hosted"),
    ] {
        let id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO accounts (email, plan) VALUES ($1, $2::plan) RETURNING id",
        )
        .bind(email)
        .bind(plan)
        .fetch_one(&pool)
        .await
        .expect("insert account");
        ids.push(id);
    }
    let (old_pro, agency) = (ids[1], ids[2]);
    // 12 monitored sites on the Agency account and 5 on the old Pro, `s0` the oldest.
    for (account, count) in [(agency, 12), (old_pro, 5)] {
        for i in 0..count {
            sqlx::query(
                "INSERT INTO sites (account_id, domain, start_url, created_at) \
                 VALUES ($1, $2, 'https://x/', now() - make_interval(days => $3))",
            )
            .bind(account)
            .bind(format!("s{i}.test"))
            .bind(100 - i)
            .execute(&pool)
            .await
            .expect("insert site");
        }
    }

    codoseo_store::pool::migrate(&pool)
        .await
        .expect("apply 0015");

    let mut plans = Vec::new();
    for id in &ids {
        let plan: String = sqlx::query_scalar("SELECT plan::text FROM accounts WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("read plan");
        plans.push(plan);
    }
    assert_eq!(plans, ["free", "lite", "pro", "self_hosted"]);

    let monitored = |account: uuid::Uuid| {
        sqlx::query_scalar::<_, String>(
            "SELECT domain FROM sites WHERE account_id = $1 AND monitoring_active \
             ORDER BY created_at",
        )
        .bind(account)
        .fetch_all(&pool)
    };
    assert_eq!(
        monitored(agency).await.expect("sites"),
        (0..10).map(|i| format!("s{i}.test")).collect::<Vec<_>>(),
        "the 10 oldest stay monitored"
    );
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM sites WHERE account_id = $1")
        .bind(agency)
        .fetch_one(&pool)
        .await
        .expect("count sites");
    assert_eq!(total, 12, "nothing is deleted");
    assert_eq!(
        monitored(old_pro).await.expect("sites").len(),
        5,
        "Lite keeps the old Pro's 5 sites"
    );
    pool.close().await;

    admin
        .execute(format!(r#"DROP DATABASE "{name}" WITH (FORCE)"#).as_str())
        .await
        .expect("drop test database");
}

/// 0017 starts the crawl totals from the finished crawls still in the database: their stored page
/// counts, and each domain once. Queued and failed crawls count for nothing.
#[tokio::test]
async fn the_crawl_totals_start_from_the_finished_crawls() {
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
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(db_url.as_str())
        .await
        .expect("connect to test database");

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../store/migrations");
    let all = sqlx::migrate::Migrator::new(dir.clone())
        .await
        .expect("read migrations");
    let mut before = sqlx::migrate::Migrator::new(dir)
        .await
        .expect("read migrations");
    before.migrations = before
        .migrations
        .iter()
        .filter(|m| m.version < 17)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    before.run(&pool).await.expect("migrate to 0016");

    // Two finished crawls of one domain, one of another, a failed and a queued crawl.
    for (domain, status, pages) in [
        ("a.test", "done", Some(120)),
        ("a.test", "done", Some(80)),
        ("b.test", "done", Some(7)),
        ("c.test", "failed", None),
        ("d.test", "queued", None),
    ] {
        let site: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO sites (domain, start_url) VALUES ($1, 'https://' || $1 || '/') RETURNING id",
        )
        .bind(domain)
        .fetch_one(&pool)
        .await
        .expect("insert site");
        sqlx::query(
            "INSERT INTO crawls (site_id, domain, trigger, priority, status, summary) \
             VALUES ($1, $2, 'quick', 0, $3::crawl_status, $4)",
        )
        .bind(site)
        .bind(domain)
        .bind(status)
        .bind(pages.map(|p: i64| serde_json::json!({"report_summary": {"pages": p}})))
        .execute(&pool)
        .await
        .expect("insert crawl");
    }
    all.run(&pool).await.expect("migrate to the newest");

    let totals: (i64, i64, i64) =
        sqlx::query_as("SELECT pages_crawled, crawls, websites FROM crawl_stats")
            .fetch_one(&pool)
            .await
            .expect("read totals");
    assert_eq!(totals, (207, 3, 2));
    pool.close().await;

    admin
        .execute(format!(r#"DROP DATABASE "{name}" WITH (FORCE)"#).as_str())
        .await
        .expect("drop test database");
}
