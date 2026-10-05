//! M8 T1: API keys (hashed, live until revoked, capped per account) and the daily call quota.

mod support;

use codoseo_store::api_keys::{self, ApiKey, Charge, CreateKeyOutcome};
use sqlx::PgPool;
use support::TestDb;
use uuid::Uuid;

async fn account(pool: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO accounts (email, email_canonical) VALUES ($1, $1) RETURNING id")
        .bind(email)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn hash(n: u32) -> Vec<u8> {
    format!("hash-{n}").into_bytes()
}

async fn key(pool: &PgPool, account: Uuid, name: &str, n: u32) -> ApiKey {
    match api_keys::create(pool, account, name, &hash(n), "cdo_Ab3dE5gH", 20)
        .await
        .unwrap()
    {
        CreateKeyOutcome::Created(k) => k,
        CreateKeyOutcome::LimitReached => panic!("not at the cap"),
    }
}

#[tokio::test]
async fn keys_are_created_listed_newest_first_and_revoked() {
    let db = TestDb::new().await;
    let ana = account(&db.pool, "ana@example.com").await;
    let bob = account(&db.pool, "bob@example.com").await;

    let first = key(&db.pool, ana, "CI", 1).await;
    let second = key(&db.pool, ana, "Claude Code", 2).await;
    key(&db.pool, bob, "Bob's", 3).await;
    assert_eq!(first.name, "CI");
    assert_eq!(first.prefix, "cdo_Ab3dE5gH");
    assert!(first.last_used_at.is_none());

    let listed = api_keys::list_for_account(&db.pool, ana).await.unwrap();
    assert_eq!(
        listed.iter().map(|k| k.id).collect::<Vec<_>>(),
        vec![second.id, first.id]
    );

    // Another account's revoke does nothing; the owner's works once.
    assert!(!api_keys::revoke(&db.pool, bob, first.id).await.unwrap());
    assert!(api_keys::revoke(&db.pool, ana, first.id).await.unwrap());
    assert!(!api_keys::revoke(&db.pool, ana, first.id).await.unwrap());
    assert!(
        !api_keys::revoke(&db.pool, ana, Uuid::new_v4())
            .await
            .unwrap()
    );
    let listed = api_keys::list_for_account(&db.pool, ana).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, second.id);
}

#[tokio::test]
async fn only_live_keys_authenticate() {
    let db = TestDb::new().await;
    let ana = account(&db.pool, "ana@example.com").await;
    let k = key(&db.pool, ana, "CI", 1).await;

    let (id, who) = api_keys::authenticate(&db.pool, &hash(1))
        .await
        .unwrap()
        .expect("live key");
    assert_eq!(id, k.id);
    assert_eq!(who.id, ana);
    assert_eq!(who.email, "ana@example.com");

    assert!(
        api_keys::authenticate(&db.pool, &hash(2))
            .await
            .unwrap()
            .is_none(),
        "unknown"
    );
    api_keys::revoke(&db.pool, ana, k.id).await.unwrap();
    assert!(
        api_keys::authenticate(&db.pool, &hash(1))
            .await
            .unwrap()
            .is_none(),
        "revoked"
    );
}

#[tokio::test]
async fn an_account_holds_at_most_the_cap_of_live_keys() {
    let db = TestDb::new().await;
    let ana = account(&db.pool, "ana@example.com").await;
    let mut first = None;
    for n in 0..20 {
        let k = key(&db.pool, ana, &format!("k{n}"), n).await;
        first.get_or_insert(k.id);
    }
    assert_eq!(
        api_keys::create(&db.pool, ana, "one too many", &hash(99), "cdo_xxxxxxxx", 20)
            .await
            .unwrap(),
        CreateKeyOutcome::LimitReached
    );
    // Another account is unaffected, and a revoked key frees a slot.
    let bob = account(&db.pool, "bob@example.com").await;
    key(&db.pool, bob, "ok", 100).await;
    api_keys::revoke(&db.pool, ana, first.unwrap())
        .await
        .unwrap();
    key(&db.pool, ana, "fits now", 101).await;
}

#[tokio::test]
async fn concurrent_creates_cannot_pass_the_cap() {
    let db = TestDb::new().await;
    let ana = account(&db.pool, "ana@example.com").await;
    let mut tasks = Vec::new();
    for n in 0..12u32 {
        let pool = db.pool.clone();
        tasks.push(tokio::spawn(async move {
            api_keys::create(&pool, ana, "k", &hash(n), "cdo_xxxxxxxx", 5)
                .await
                .unwrap()
        }));
    }
    let mut created = 0;
    for t in tasks {
        if matches!(t.await.unwrap(), CreateKeyOutcome::Created(_)) {
            created += 1;
        }
    }
    assert_eq!(created, 5);
    assert_eq!(
        api_keys::list_for_account(&db.pool, ana)
            .await
            .unwrap()
            .len(),
        5
    );
}

#[tokio::test]
async fn last_used_is_touched_at_most_once_a_minute() {
    let db = TestDb::new().await;
    let ana = account(&db.pool, "ana@example.com").await;
    let k = key(&db.pool, ana, "CI", 1).await;
    let last =
        || async { api_keys::list_for_account(&db.pool, ana).await.unwrap()[0].last_used_at };

    api_keys::authenticate(&db.pool, &hash(1)).await.unwrap();
    let first = last().await.expect("set on first use");

    // Used again right away: left alone.
    api_keys::authenticate(&db.pool, &hash(1)).await.unwrap();
    assert_eq!(last().await, Some(first));

    // A minute and a bit later it moves.
    sqlx::query("UPDATE api_keys SET last_used_at = now() - interval '61 seconds' WHERE id = $1")
        .bind(k.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let old = last().await.unwrap();
    api_keys::authenticate(&db.pool, &hash(1)).await.unwrap();
    assert!(last().await.unwrap() > old);
}

#[tokio::test]
async fn charge_counts_up_to_the_limit_and_a_refusal_consumes_nothing() {
    let db = TestDb::new().await;
    let ana = account(&db.pool, "ana@example.com").await;
    for n in 1..=3 {
        assert_eq!(
            api_keys::charge(&db.pool, ana, Some(3)).await.unwrap(),
            Charge::Ok {
                used: n,
                limit: Some(3)
            }
        );
    }
    for _ in 0..4 {
        assert_eq!(
            api_keys::charge(&db.pool, ana, Some(3)).await.unwrap(),
            Charge::OverQuota { limit: 3 }
        );
    }
    assert_eq!(api_keys::usage_today(&db.pool, ana).await.unwrap(), 3);

    // A bigger limit later the same day picks up where the counter stands.
    assert_eq!(
        api_keys::charge(&db.pool, ana, Some(5)).await.unwrap(),
        Charge::Ok {
            used: 4,
            limit: Some(5)
        }
    );
    // Another account has its own counter.
    let bob = account(&db.pool, "bob@example.com").await;
    assert_eq!(api_keys::usage_today(&db.pool, bob).await.unwrap(), 0);
}

#[tokio::test]
async fn a_limit_of_zero_refuses_the_first_call_too() {
    let db = TestDb::new().await;
    let ana = account(&db.pool, "ana@example.com").await;
    assert_eq!(
        api_keys::charge(&db.pool, ana, Some(0)).await.unwrap(),
        Charge::OverQuota { limit: 0 }
    );
    assert_eq!(api_keys::usage_today(&db.pool, ana).await.unwrap(), 0);
}

#[tokio::test]
async fn fifty_concurrent_charges_with_ten_left_succeed_exactly_ten_times() {
    let db = TestDb::new().await;
    let ana = account(&db.pool, "ana@example.com").await;
    // 90 of 100 already used today.
    for _ in 0..90 {
        api_keys::charge(&db.pool, ana, Some(100)).await.unwrap();
    }
    let mut tasks = Vec::new();
    for _ in 0..50 {
        let pool = db.pool.clone();
        tasks.push(tokio::spawn(async move {
            api_keys::charge(&pool, ana, Some(100)).await.unwrap()
        }));
    }
    let mut ok = 0;
    let mut refused = 0;
    for t in tasks {
        match t.await.unwrap() {
            Charge::Ok { .. } => ok += 1,
            Charge::OverQuota { .. } => refused += 1,
        }
    }
    assert_eq!((ok, refused), (10, 40));
    assert_eq!(api_keys::usage_today(&db.pool, ana).await.unwrap(), 100);
}

#[tokio::test]
async fn concurrent_first_calls_of_the_day_do_not_overshoot() {
    let db = TestDb::new().await;
    let ana = account(&db.pool, "ana@example.com").await;
    let mut tasks = Vec::new();
    for _ in 0..30 {
        let pool = db.pool.clone();
        tasks.push(tokio::spawn(async move {
            api_keys::charge(&pool, ana, Some(10)).await.unwrap()
        }));
    }
    let mut ok = 0;
    for t in tasks {
        if matches!(t.await.unwrap(), Charge::Ok { .. }) {
            ok += 1;
        }
    }
    assert_eq!(ok, 10);
}

#[tokio::test]
async fn unlimited_still_counts() {
    let db = TestDb::new().await;
    let ana = account(&db.pool, "ana@example.com").await;
    for n in 1..=5 {
        assert_eq!(
            api_keys::charge(&db.pool, ana, None).await.unwrap(),
            Charge::Ok {
                used: n,
                limit: None
            }
        );
    }
    assert_eq!(api_keys::usage_today(&db.pool, ana).await.unwrap(), 5);
}

#[tokio::test]
async fn a_new_utc_day_starts_at_zero() {
    let db = TestDb::new().await;
    let ana = account(&db.pool, "ana@example.com").await;
    // Yesterday's allowance was all used.
    sqlx::query(
        "INSERT INTO api_usage (account_id, day, calls) \
         VALUES ($1, (now() AT TIME ZONE 'utc')::date - 1, 100)",
    )
    .bind(ana)
    .execute(&db.pool)
    .await
    .unwrap();
    assert_eq!(api_keys::usage_today(&db.pool, ana).await.unwrap(), 0);
    assert_eq!(
        api_keys::charge(&db.pool, ana, Some(100)).await.unwrap(),
        Charge::Ok {
            used: 1,
            limit: Some(100)
        }
    );
    // Yesterday's row is untouched.
    let yesterday: i32 = sqlx::query_scalar(
        "SELECT calls FROM api_usage WHERE day = (now() AT TIME ZONE 'utc')::date - 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(yesterday, 100);
}

#[tokio::test]
async fn retention_deletes_usage_older_than_35_days() {
    let db = TestDb::new().await;
    let ana = account(&db.pool, "ana@example.com").await;
    for age in [0, 34, 35, 36, 200] {
        sqlx::query(
            "INSERT INTO api_usage (account_id, day, calls) \
             VALUES ($1, (now() AT TIME ZONE 'utc')::date - $2::int, 7)",
        )
        .bind(ana)
        .bind(age)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    let report = codoseo_store::retention::run(&db.pool, None).await.unwrap();
    assert_eq!(report.api_usage_deleted, 2);
    let left: Vec<i32> = sqlx::query_scalar(
        "SELECT ((now() AT TIME ZONE 'utc')::date - day) FROM api_usage ORDER BY day DESC",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(left, vec![0, 34, 35]);
}

#[tokio::test]
async fn deleting_an_account_removes_its_keys_and_usage() {
    let db = TestDb::new().await;
    let ana = account(&db.pool, "ana@example.com").await;
    key(&db.pool, ana, "CI", 1).await;
    api_keys::charge(&db.pool, ana, None).await.unwrap();
    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(ana)
        .execute(&db.pool)
        .await
        .unwrap();
    for table in ["api_keys", "api_usage"] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(n, 0, "{table}");
    }
}

#[tokio::test]
async fn retention_deletes_keys_revoked_over_90_days_ago() {
    let db = TestDb::new().await;
    let ana = account(&db.pool, "ana@example.com").await;
    let live = key(&db.pool, ana, "live", 1).await;
    let mut ids = Vec::new();
    for (n, days) in [(2, 89), (3, 91), (4, 400)] {
        let k = key(&db.pool, ana, &format!("revoked {days}"), n).await;
        api_keys::revoke(&db.pool, ana, k.id).await.unwrap();
        sqlx::query(
            "UPDATE api_keys SET revoked_at = now() - ($2::int * interval '1 day') WHERE id = $1",
        )
        .bind(k.id)
        .bind(days)
        .execute(&db.pool)
        .await
        .unwrap();
        ids.push((days, k.id));
    }
    let report = codoseo_store::retention::run(&db.pool, None).await.unwrap();
    assert_eq!(report.revoked_keys_deleted, 2);
    let left: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM api_keys")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(left.len(), 2);
    assert!(left.contains(&live.id), "a live key stays");
    assert!(left.contains(&ids[0].1), "89 days stays");
}
