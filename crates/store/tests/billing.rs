//! T7.6: billing events change the plan exactly once, in order, and never delete data.

mod support;

use codoseo_core::plan::Plan;
use codoseo_store::billing::{self, Outcome, SetMonitored, SubscriptionEvent};
use support::TestDb;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

async fn account(db: &TestDb, email: &str, plan: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO accounts (email, email_canonical, plan) VALUES ($1, lower($1), $2::plan) \
         RETURNING id",
    )
    .bind(email)
    .bind(plan)
    .fetch_one(&db.pool)
    .await
    .expect("insert account")
}

async fn site(db: &TestDb, account: Uuid, domain: &str, age_days: i32, schedule: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO sites (account_id, domain, start_url, schedule, next_crawl_at, created_at) \
         VALUES ($1, $2, 'https://x/', $3, now() + interval '1 day', now() - make_interval(days => $4)) \
         RETURNING id",
    )
    .bind(account)
    .bind(domain)
    .bind(schedule)
    .bind(age_days)
    .fetch_one(&db.pool)
    .await
    .expect("insert site")
}

async fn site_state(db: &TestDb, id: Uuid) -> (bool, Option<String>, Option<OffsetDateTime>) {
    sqlx::query_as("SELECT monitoring_active, schedule, next_crawl_at FROM sites WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .expect("site state")
}

async fn plan_of(db: &TestDb, id: Uuid) -> (String, Option<OffsetDateTime>) {
    sqlx::query_as("SELECT plan::text, plan_expires_at FROM accounts WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .expect("plan")
}

fn at(days: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap() + Duration::days(days)
}

fn event(id: &str, kind: &str, account: Uuid, when: OffsetDateTime) -> SubscriptionEvent {
    SubscriptionEvent {
        event_id: id.to_owned(),
        kind: kind.to_owned(),
        at: when,
        payload: serde_json::json!({ "type": kind }),
        account_hint: Some(account),
        subscription_id: Some("sub_1".to_owned()),
        customer_id: Some("cus_1".to_owned()),
        email_canonical: None,
        plan: Some(Plan::Pro),
        status: Some("active".to_owned()),
        next_billing_date: Some(when + Duration::days(30)),
    }
}

async fn handle(db: &TestDb, ev: &SubscriptionEvent) -> Outcome {
    billing::handle_event(&db.pool, ev, OffsetDateTime::now_utc())
        .await
        .expect("handle event")
}

#[tokio::test]
async fn an_active_subscription_sets_the_plan_expiry_ids_and_daily_schedules() {
    let db = TestDb::new().await;
    let id = account(&db, "ana@example.test", "free").await;
    sqlx::query("UPDATE accounts SET paused = true WHERE id = $1")
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    let weekly = site(&db, id, "a.test", 10, "weekly").await;
    let off = site(&db, id, "b.test", 5, "weekly").await;
    sqlx::query("UPDATE sites SET monitoring_active = false WHERE id = $1")
        .bind(off)
        .execute(&db.pool)
        .await
        .unwrap();

    let ev = event("evt_1", "subscription.active", id, at(0));
    assert_eq!(handle(&db, &ev).await, Outcome::Applied(Plan::Pro));

    let (plan, expires) = plan_of(&db, id).await;
    assert_eq!(plan, "pro");
    // next billing date plus three days of grace
    assert_eq!(expires, Some(at(33)));
    let info = billing::account_billing(&db.pool, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.customer_id.as_deref(), Some("cus_1"));
    assert_eq!(info.subscription_id.as_deref(), Some("sub_1"));
    assert_eq!(info.updated_at, Some(at(0)));
    let paused: bool = sqlx::query_scalar("SELECT paused FROM accounts WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(!paused, "paying wakes a paused account");

    let (active, schedule, next) = site_state(&db, weekly).await;
    assert!(active);
    assert_eq!(schedule.as_deref(), Some("daily"));
    assert_eq!(next, None, "the scheduler picks the first daily slot");
    let (active, schedule, _) = site_state(&db, off).await;
    assert!(!active, "an inactive site stays inactive on upgrade");
    assert_eq!(
        schedule.as_deref(),
        Some("weekly"),
        "and keeps what it had until the owner turns it back on"
    );
}

#[tokio::test]
async fn a_renewal_moves_the_expiry_without_resetting_the_crawl_slot() {
    let db = TestDb::new().await;
    let id = account(&db, "ana@example.test", "free").await;
    let s = site(&db, id, "a.test", 10, "weekly").await;
    handle(&db, &event("evt_1", "subscription.active", id, at(0))).await;
    // The scheduler gives the site its slot.
    sqlx::query("UPDATE sites SET next_crawl_at = $2 WHERE id = $1")
        .bind(s)
        .bind(at(1))
        .execute(&db.pool)
        .await
        .unwrap();

    let outcome = handle(&db, &event("evt_2", "subscription.renewed", id, at(30))).await;
    assert_eq!(outcome, Outcome::Applied(Plan::Pro));
    assert_eq!(plan_of(&db, id).await.1, Some(at(63)));
    assert_eq!(site_state(&db, s).await.2, Some(at(1)));
}

#[tokio::test]
async fn a_replayed_event_changes_nothing() {
    let db = TestDb::new().await;
    let id = account(&db, "ana@example.test", "free").await;
    let ev = event("evt_1", "subscription.active", id, at(0));
    assert_eq!(handle(&db, &ev).await, Outcome::Applied(Plan::Pro));

    // Something else changed the account since; the replay must not undo it.
    sqlx::query("UPDATE accounts SET plan = 'agency' WHERE id = $1")
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(handle(&db, &ev).await, Outcome::Duplicate);
    assert_eq!(plan_of(&db, id).await.0, "agency");
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM billing_events")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(stored, 1);
}

#[tokio::test]
async fn an_older_active_after_an_applied_expired_does_not_resurrect_the_plan() {
    let db = TestDb::new().await;
    let id = account(&db, "ana@example.test", "free").await;
    handle(&db, &event("evt_1", "subscription.active", id, at(0))).await;
    handle(&db, &event("evt_3", "subscription.expired", id, at(40))).await;
    assert_eq!(plan_of(&db, id).await, ("free".to_owned(), None));

    // Delivered late: it happened before the expiry.
    let late = event("evt_2", "subscription.renewed", id, at(30));
    assert_eq!(handle(&db, &late).await, Outcome::Stale);
    assert_eq!(plan_of(&db, id).await, ("free".to_owned(), None));
}

#[tokio::test]
async fn cancelled_keeps_the_plan_and_its_expiry() {
    let db = TestDb::new().await;
    let id = account(&db, "ana@example.test", "free").await;
    handle(&db, &event("evt_1", "subscription.active", id, at(0))).await;
    let before = plan_of(&db, id).await;

    let outcome = handle(&db, &event("evt_2", "subscription.cancelled", id, at(5))).await;
    assert!(matches!(outcome, Outcome::Unchanged(_)), "{outcome:?}");
    assert_eq!(plan_of(&db, id).await, before);
    assert_eq!(before.0, "pro");
}

#[tokio::test]
async fn on_hold_and_past_due_change_nothing_and_unknown_events_are_stored() {
    let db = TestDb::new().await;
    let id = account(&db, "ana@example.test", "free").await;
    handle(&db, &event("evt_1", "subscription.active", id, at(0))).await;
    let before = plan_of(&db, id).await;
    for (n, kind) in ["subscription.on_hold", "subscription.past_due"]
        .iter()
        .enumerate()
    {
        let ev = event(&format!("evt_h{n}"), kind, id, at(2 + n as i64));
        assert!(matches!(handle(&db, &ev).await, Outcome::Unchanged(_)));
    }
    let weird = event("evt_w", "payment.succeeded", id, at(4));
    assert!(matches!(handle(&db, &weird).await, Outcome::Unchanged(_)));
    assert_eq!(plan_of(&db, id).await, before);
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM billing_events")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(stored, 4);
}

#[tokio::test]
async fn expired_downgrades_and_keeps_only_the_oldest_site_monitored() {
    let db = TestDb::new().await;
    let id = account(&db, "ana@example.test", "free").await;
    let oldest = site(&db, id, "a.test", 30, "weekly").await;
    let middle = site(&db, id, "b.test", 20, "weekly").await;
    let newest = site(&db, id, "c.test", 10, "weekly").await;
    handle(&db, &event("evt_1", "subscription.active", id, at(0))).await;
    assert_eq!(site_state(&db, middle).await.1.as_deref(), Some("daily"));

    let outcome = handle(&db, &event("evt_2", "subscription.expired", id, at(31))).await;
    assert_eq!(outcome, Outcome::Downgraded);
    assert_eq!(plan_of(&db, id).await, ("free".to_owned(), None));
    let (active, schedule, _) = site_state(&db, oldest).await;
    assert!(active);
    assert_eq!(schedule.as_deref(), Some("weekly"));
    assert!(!site_state(&db, middle).await.0);
    assert!(!site_state(&db, newest).await.0);

    // `failed` does the same.
    let id2 = account(&db, "bo@example.test", "free").await;
    let mut ev = event("evt_3", "subscription.active", id2, at(0));
    ev.subscription_id = Some("sub_2".to_owned());
    handle(&db, &ev).await;
    let mut failed = event("evt_4", "subscription.failed", id2, at(2));
    failed.subscription_id = Some("sub_2".to_owned());
    assert_eq!(handle(&db, &failed).await, Outcome::Downgraded);
    assert_eq!(plan_of(&db, id2).await.0, "free");
}

#[tokio::test]
async fn the_end_of_an_old_subscription_does_not_downgrade_a_newer_one() {
    let db = TestDb::new().await;
    let id = account(&db, "ana@example.test", "free").await;
    handle(&db, &event("evt_1", "subscription.active", id, at(0))).await;
    let mut new = event("evt_2", "subscription.active", id, at(10));
    new.subscription_id = Some("sub_2".to_owned());
    handle(&db, &new).await;

    let mut old_ends = event("evt_3", "subscription.expired", id, at(11));
    old_ends.subscription_id = Some("sub_1".to_owned());
    assert!(matches!(
        handle(&db, &old_ends).await,
        Outcome::Unchanged(_)
    ));
    assert_eq!(plan_of(&db, id).await.0, "pro");
}

#[tokio::test]
async fn the_account_is_found_by_metadata_then_subscription_then_customer_then_email() {
    let db = TestDb::new().await;
    let by_hint = account(&db, "hint@example.test", "free").await;
    let by_sub = account(&db, "sub@example.test", "free").await;
    let by_customer = account(&db, "cus@example.test", "free").await;
    let by_email = account(&db, "mail@example.test", "free").await;
    sqlx::query("UPDATE accounts SET dodo_subscription_id = 'sub_x' WHERE id = $1")
        .bind(by_sub)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE accounts SET dodo_customer_id = 'cus_y' WHERE id = $1")
        .bind(by_customer)
        .execute(&db.pool)
        .await
        .unwrap();

    let mut ev = event("e1", "subscription.active", by_hint, at(0));
    ev.subscription_id = Some("sub_x".to_owned());
    handle(&db, &ev).await;
    assert_eq!(plan_of(&db, by_hint).await.0, "pro", "the hint wins");
    assert_eq!(plan_of(&db, by_sub).await.0, "free");

    let mut ev = event("e2", "subscription.active", by_hint, at(0));
    ev.account_hint = None;
    ev.subscription_id = Some("sub_x".to_owned());
    handle(&db, &ev).await;
    assert_eq!(plan_of(&db, by_sub).await.0, "pro");

    let mut ev = event("e3", "subscription.active", by_hint, at(0));
    ev.account_hint = None;
    ev.subscription_id = Some("sub_new".to_owned());
    ev.customer_id = Some("cus_y".to_owned());
    handle(&db, &ev).await;
    assert_eq!(plan_of(&db, by_customer).await.0, "pro");

    let mut ev = event("e4", "subscription.active", by_hint, at(0));
    ev.account_hint = None;
    ev.subscription_id = Some("sub_other".to_owned());
    ev.customer_id = Some("cus_z".to_owned());
    ev.email_canonical = Some("mail@example.test".to_owned());
    handle(&db, &ev).await;
    assert_eq!(plan_of(&db, by_email).await.0, "pro");
}

#[tokio::test]
async fn an_event_for_nobody_is_stored_and_changes_nothing() {
    let db = TestDb::new().await;
    let id = account(&db, "ana@example.test", "free").await;
    let mut ev = event("evt_1", "subscription.active", id, at(0));
    ev.account_hint = Some(Uuid::new_v4());
    ev.subscription_id = Some("sub_unknown".to_owned());
    ev.customer_id = None;
    assert_eq!(handle(&db, &ev).await, Outcome::NoAccount);
    assert_eq!(plan_of(&db, id).await.0, "free");
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM billing_events")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(stored, 1);
}

#[tokio::test]
async fn an_unknown_product_or_an_inactive_status_does_not_grant_a_plan() {
    let db = TestDb::new().await;
    let id = account(&db, "ana@example.test", "free").await;
    let mut ev = event("evt_1", "subscription.active", id, at(0));
    ev.plan = None;
    assert!(matches!(handle(&db, &ev).await, Outcome::Unchanged(_)));
    let mut ev = event("evt_2", "subscription.updated", id, at(1));
    ev.status = Some("on_hold".to_owned());
    assert!(matches!(handle(&db, &ev).await, Outcome::Unchanged(_)));
    assert_eq!(plan_of(&db, id).await.0, "free");
}

#[tokio::test]
async fn a_plan_change_to_a_smaller_plan_trims_the_sites() {
    let db = TestDb::new().await;
    let id = account(&db, "ana@example.test", "free").await;
    let mut sites = Vec::new();
    for n in 0..7 {
        sites.push(site(&db, id, &format!("s{n}.test"), 30 - n, "weekly").await);
    }
    let mut agency = event("evt_1", "subscription.active", id, at(0));
    agency.plan = Some(Plan::Agency);
    handle(&db, &agency).await;
    assert!(site_state(&db, sites[6]).await.0);

    let mut pro = event("evt_2", "subscription.plan_changed", id, at(10));
    pro.plan = Some(Plan::Pro);
    assert_eq!(handle(&db, &pro).await, Outcome::Applied(Plan::Pro));
    for (n, s) in sites.iter().enumerate() {
        assert_eq!(site_state(&db, *s).await.0, n < 5, "site {n}");
    }
}

#[tokio::test]
async fn the_owner_picks_which_sites_stay_monitored() {
    let db = TestDb::new().await;
    let id = account(&db, "ana@example.test", "free").await;
    let a = site(&db, id, "a.test", 30, "weekly").await;
    let b = site(&db, id, "b.test", 20, "weekly").await;
    let c = site(&db, id, "c.test", 10, "weekly").await;
    sqlx::query("UPDATE sites SET monitoring_active = false WHERE id = ANY($1)")
        .bind(vec![b, c])
        .execute(&db.pool)
        .await
        .unwrap();

    // The free plan keeps one.
    let outcome = billing::set_monitored(&db.pool, id, &[c]).await.unwrap();
    assert_eq!(outcome, SetMonitored::Saved);
    assert!(!site_state(&db, a).await.0);
    assert!(!site_state(&db, b).await.0);
    let (active, schedule, next) = site_state(&db, c).await;
    assert!(active);
    assert_eq!(schedule.as_deref(), Some("weekly"));
    assert_eq!(next, None);

    let outcome = billing::set_monitored(&db.pool, id, &[a, b]).await.unwrap();
    assert_eq!(outcome, SetMonitored::TooMany { max: 1 });
    assert!(
        site_state(&db, c).await.0,
        "a refused change changes nothing"
    );

    let other = account(&db, "bo@example.test", "free").await;
    let theirs = site(&db, other, "theirs.test", 5, "weekly").await;
    let outcome = billing::set_monitored(&db.pool, id, &[theirs])
        .await
        .unwrap();
    assert_eq!(outcome, SetMonitored::UnknownSite);
    assert!(site_state(&db, theirs).await.0);
}
