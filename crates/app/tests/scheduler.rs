//! The scheduler against a real throwaway Postgres database. Time is injected, so a "tick on
//! Monday 08:05 in Kolkata" is a function call, not a wait.

#[allow(dead_code)]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use codoseo::scheduler::{SchedulerContext, TickReport, next_weekly, tick};
use codoseo_web::Mode;
use jiff::{SignedDuration, Timestamp};
use sqlx::PgPool;
use support::TestDb;
use uuid::Uuid;

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

/// A Monday, noon UTC.
fn noon() -> Timestamp {
    ts("2026-10-05T12:00:00Z")
}

fn ctx(db: &TestDb, mode: Mode) -> SchedulerContext {
    SchedulerContext {
        pool: db.pool.clone(),
        mode,
        base_url: url::Url::parse("https://codoseo.com").unwrap(),
        heartbeat_url: None,
        http: reqwest::Client::new(),
    }
}

async fn account(db: &TestDb, email: &str, plan: &str, tz: &str, age_days: i32) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO accounts (email, plan, timezone, created_at) \
         VALUES ($1, $2::plan, $3, $4::timestamptz - make_interval(days => $5)) RETURNING id",
    )
    .bind(email)
    .bind(plan)
    .bind(tz)
    .bind(noon().to_string())
    .bind(age_days)
    .fetch_one(&db.pool)
    .await
    .expect("insert account")
}

/// A monitored site whose slot was `due_minutes_ago` minutes before `noon()`.
async fn site(
    db: &TestDb,
    account: Uuid,
    domain: &str,
    schedule: &str,
    due_minutes_ago: i32,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO sites (account_id, domain, start_url, schedule, next_crawl_at) \
         VALUES ($1, $2, 'https://x/', $3, $4::timestamptz - make_interval(mins => $5)) RETURNING id",
    )
    .bind(account)
    .bind(domain)
    .bind(schedule)
    .bind(noon().to_string())
    .bind(due_minutes_ago)
    .fetch_one(&db.pool)
    .await
    .expect("insert site")
}

async fn crawls(pool: &PgPool, site: Uuid) -> Vec<(String, String, i16)> {
    sqlx::query_as("SELECT trigger::text, status::text, priority FROM crawls WHERE site_id = $1")
        .bind(site)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn next_crawl(pool: &PgPool, site: Uuid) -> Option<Timestamp> {
    let t: Option<time::OffsetDateTime> =
        sqlx::query_scalar("SELECT next_crawl_at FROM sites WHERE id = $1")
            .bind(site)
            .fetch_one(pool)
            .await
            .unwrap();
    t.map(|t| Timestamp::from_nanosecond(t.unix_timestamp_nanos()).unwrap())
}

async fn jobs(pool: &PgPool, kind: &str) -> Vec<serde_json::Value> {
    sqlx::query_scalar("SELECT payload FROM jobs WHERE kind = $1::job_kind ORDER BY created_at")
        .bind(kind)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn done_crawl_at(db: &TestDb, site: Uuid, domain: &str, finished: Timestamp) {
    sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, finished_at) \
         VALUES ($1, $2, 'first', 1, 'done', $3::timestamptz)",
    )
    .bind(site)
    .bind(domain)
    .bind(finished.to_string())
    .execute(&db.pool)
    .await
    .unwrap();
}

/// The mail worker's side: every queued `send_email` job gets delivered.
async fn deliver_mail(pool: &PgPool) {
    sqlx::query("UPDATE jobs SET status = 'done' WHERE kind = 'send_email'")
        .execute(pool)
        .await
        .unwrap();
}

fn assert_clean(report: &TickReport) {
    assert!(
        report.failures.is_empty(),
        "a step failed: {:?}",
        report.failures
    );
}

// ---- scheduled crawls -------------------------------------------------------------------

#[tokio::test]
async fn due_sites_get_a_crawl_in_the_lane_of_their_plan() {
    let db = TestDb::new().await;
    let free = account(&db, "free@example.test", "free", "UTC", 1).await;
    let pro = account(&db, "pro@example.test", "pro", "UTC", 1).await;
    let agency = account(&db, "agency@example.test", "agency", "UTC", 1).await;
    let selfhost = account(&db, "self@example.test", "self_hosted", "UTC", 1).await;
    let mut sites = Vec::new();
    for (acct, domain) in [
        (free, "f.test"),
        (pro, "p.test"),
        (agency, "a.test"),
        (selfhost, "s.test"),
    ] {
        sites.push(site(&db, acct, domain, "weekly", 5).await);
    }

    let report = tick(&ctx(&db, Mode::Cloud), noon()).await;
    assert_clean(&report);
    assert_eq!(report.crawls_queued, 4);
    let expected = [5, 3, 3, 3];
    for (s, priority) in sites.iter().zip(expected) {
        assert_eq!(
            crawls(&db.pool, *s).await,
            vec![("schedule".to_owned(), "queued".to_owned(), priority)]
        );
        assert!(next_crawl(&db.pool, *s).await.unwrap() > noon());
    }
}

#[tokio::test]
async fn a_site_three_weeks_overdue_gets_one_crawl_and_a_future_slot() {
    let db = TestDb::new().await;
    let pro = account(&db, "pro@example.test", "pro", "UTC", 90).await;
    let s = site(&db, pro, "late.test", "daily", 21 * 24 * 60).await;
    let c = ctx(&db, Mode::Cloud);

    assert_clean(&tick(&c, noon()).await);
    assert_eq!(crawls(&db.pool, s).await.len(), 1, "no back-fill");
    let next = next_crawl(&db.pool, s).await.unwrap();
    assert!(next > noon());
    assert!(next <= noon() + SignedDuration::from_hours(24));

    // The next ticks (the crawl is still queued, then not yet due) add nothing.
    tick(&c, noon() + SignedDuration::from_secs(60)).await;
    tick(&c, noon() + SignedDuration::from_secs(120)).await;
    assert_eq!(crawls(&db.pool, s).await.len(), 1);
}

#[tokio::test]
async fn a_free_site_marked_daily_runs_weekly() {
    let db = TestDb::new().await;
    let free = account(&db, "free@example.test", "free", "UTC", 1).await;
    let s = site(&db, free, "daily.test", "daily", 1).await;
    tick(&ctx(&db, Mode::Cloud), noon()).await;
    assert_eq!(
        next_crawl(&db.pool, s).await.unwrap(),
        next_weekly(s, noon())
    );
}

#[tokio::test]
async fn a_paid_daily_site_runs_at_its_hour_in_the_account_time_zone() {
    let db = TestDb::new().await;
    let pro = account(&db, "pro@example.test", "pro", "Europe/Berlin", 1).await;
    let s = site(&db, pro, "berlin.test", "daily", 1).await;
    sqlx::query("UPDATE sites SET scheduled_hour = 6 WHERE id = $1")
        .bind(s)
        .execute(&db.pool)
        .await
        .unwrap();
    tick(&ctx(&db, Mode::Cloud), noon()).await;
    // 06:00 CEST on Tuesday 2026-10-06.
    assert_eq!(
        next_crawl(&db.pool, s).await.unwrap(),
        ts("2026-10-06T04:00:00Z")
    );
}

#[tokio::test]
async fn a_site_with_a_queued_or_running_crawl_gets_no_second_one() {
    let db = TestDb::new().await;
    let pro = account(&db, "pro@example.test", "pro", "UTC", 1).await;
    let queued = site(&db, pro, "q.test", "daily", 5).await;
    let running = site(&db, pro, "r.test", "daily", 5).await;
    sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status) \
         VALUES ($1, 'q.test', 'manual', 2, 'queued'), ($2, 'r.test', 'manual', 2, 'running')",
    )
    .bind(queued)
    .bind(running)
    .execute(&db.pool)
    .await
    .unwrap();
    let report = tick(&ctx(&db, Mode::Cloud), noon()).await;
    assert_eq!(report.crawls_queued, 0);
    assert_eq!(crawls(&db.pool, queued).await.len(), 1);
    assert_eq!(crawls(&db.pool, running).await.len(), 1);
    assert!(next_crawl(&db.pool, queued).await.unwrap() > noon());
}

#[tokio::test]
async fn two_schedulers_ticking_at_once_queue_each_crawl_once() {
    let db = TestDb::new().await;
    let pro = account(&db, "pro@example.test", "pro", "UTC", 1).await;
    let mut sites = Vec::new();
    for i in 0..30 {
        sites.push(site(&db, pro, &format!("s{i}.test"), "daily", 5).await);
    }
    let (a, b) = (ctx(&db, Mode::Cloud), ctx(&db, Mode::Cloud));
    let (ra, rb) = tokio::join!(tick(&a, noon()), tick(&b, noon()));
    assert_clean(&ra);
    assert_clean(&rb);
    assert_eq!(ra.crawls_queued + rb.crawls_queued, 30);
    for s in sites {
        assert_eq!(crawls(&db.pool, s).await.len(), 1);
    }
}

#[tokio::test]
async fn paused_accounts_and_stopped_sites_are_not_scheduled() {
    let db = TestDb::new().await;
    let paused = account(&db, "paused@example.test", "free", "UTC", 40).await;
    sqlx::query("UPDATE accounts SET paused = true WHERE id = $1")
        .bind(paused)
        .execute(&db.pool)
        .await
        .unwrap();
    let ps = site(&db, paused, "paused.test", "weekly", 5).await;
    let pro = account(&db, "pro@example.test", "pro", "UTC", 1).await;
    let stopped = site(&db, pro, "stopped.test", "daily", 5).await;
    sqlx::query("UPDATE sites SET monitoring_active = false WHERE id = $1")
        .bind(stopped)
        .execute(&db.pool)
        .await
        .unwrap();
    let report = tick(&ctx(&db, Mode::Cloud), noon()).await;
    assert_eq!(report.crawls_queued, 0);
    assert!(crawls(&db.pool, ps).await.is_empty());
    assert!(crawls(&db.pool, stopped).await.is_empty());
}

#[tokio::test]
async fn a_site_without_a_slot_gets_one_but_no_crawl() {
    let db = TestDb::new().await;
    let free = account(&db, "free@example.test", "free", "UTC", 1).await;
    let s = site(&db, free, "fresh.test", "weekly", 0).await;
    sqlx::query("UPDATE sites SET next_crawl_at = NULL WHERE id = $1")
        .bind(s)
        .execute(&db.pool)
        .await
        .unwrap();
    let report = tick(&ctx(&db, Mode::Cloud), noon()).await;
    assert_eq!((report.crawls_queued, report.sites_initialised), (0, 1));
    assert!(crawls(&db.pool, s).await.is_empty());
    assert_eq!(
        next_crawl(&db.pool, s).await.unwrap(),
        next_weekly(s, noon())
    );
}

// ---- plan expiry -------------------------------------------------------------------------

#[tokio::test]
async fn an_expired_plan_is_downgraded_by_the_tick() {
    let db = TestDb::new().await;
    let pro = account(&db, "pro@example.test", "pro", "UTC", 100).await;
    sqlx::query("UPDATE accounts SET plan_expires_at = $2::timestamptz WHERE id = $1")
        .bind(pro)
        .bind((noon() - SignedDuration::from_hours(1)).to_string())
        .execute(&db.pool)
        .await
        .unwrap();
    let a = site(&db, pro, "a.test", "daily", -600).await;
    let b = site(&db, pro, "b.test", "daily", -600).await;
    let report = tick(&ctx(&db, Mode::Cloud), noon()).await;
    assert_eq!(report.downgraded, 1);
    let plan: String = sqlx::query_scalar("SELECT plan::text FROM accounts WHERE id = $1")
        .bind(pro)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(plan, "free");
    let active: Vec<bool> = sqlx::query_scalar(
        "SELECT monitoring_active FROM sites WHERE id = ANY($1) ORDER BY created_at, id",
    )
    .bind(vec![a, b])
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(active.iter().filter(|a| **a).count(), 1);
}

// ---- stuck jobs --------------------------------------------------------------------------

#[tokio::test]
async fn the_tick_puts_stuck_jobs_back_in_the_queue() {
    let db = TestDb::new().await;
    sqlx::query(
        "INSERT INTO jobs (kind, payload, status, claimed_by, claimed_at) \
         VALUES ('send_email', '{}', 'running', 'dead', now() - interval '11 minutes')",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let report = tick(&ctx(&db, Mode::SelfHost), noon()).await;
    assert_eq!(report.jobs_requeued, 1);
    let status: String =
        sqlx::query_scalar("SELECT status::text FROM jobs WHERE kind = 'send_email'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(status, "queued");
}

// ---- digest and cleanup ------------------------------------------------------------------

#[tokio::test]
async fn the_digest_is_queued_once_on_monday_morning_local_time() {
    let db = TestDb::new().await;
    let acct = account(&db, "ana@example.test", "pro", "Asia/Kolkata", 60).await;
    let s = site(&db, acct, "ana.test", "weekly", -600).await;
    done_crawl_at(&db, s, "ana.test", noon() - SignedDuration::from_hours(80)).await;
    let utc_acct = account(&db, "utc@example.test", "pro", "UTC", 60).await;
    let s2 = site(&db, utc_acct, "utc.test", "weekly", -600).await;
    done_crawl_at(&db, s2, "utc.test", noon() - SignedDuration::from_hours(80)).await;
    let c = ctx(&db, Mode::Cloud);

    // Monday 2026-10-05 07:55 IST: not yet.
    assert_eq!(tick(&c, ts("2026-10-05T02:25:00Z")).await.digests_queued, 0);
    // 08:05 IST: queued for Kolkata (UTC is still asleep at 02:35).
    assert_eq!(tick(&c, ts("2026-10-05T02:35:00Z")).await.digests_queued, 1);
    // 09:05 IST: already sent.
    assert_eq!(tick(&c, ts("2026-10-05T03:35:00Z")).await.digests_queued, 0);
    assert_eq!(
        jobs(&db.pool, "send_digest").await,
        vec![serde_json::json!({"account_id": acct})]
    );
    // 08:05 UTC: the UTC account's turn.
    assert_eq!(tick(&c, ts("2026-10-05T08:05:00Z")).await.digests_queued, 1);
    // Tuesday: nothing.
    assert_eq!(tick(&c, ts("2026-10-06T09:00:00Z")).await.digests_queued, 0);
    assert_eq!(jobs(&db.pool, "send_digest").await.len(), 2);
}

#[tokio::test]
async fn cleanup_is_queued_once_per_day_across_many_ticks() {
    let db = TestDb::new().await;
    let c = ctx(&db, Mode::SelfHost);
    let day1 = ts("2026-10-05T00:01:00Z");
    for minute in 0..30 {
        tick(&c, day1 + SignedDuration::from_mins(minute * 45)).await;
    }
    // 30 ticks span 22 h: still the 5th.
    assert_eq!(jobs(&db.pool, "cleanup").await.len(), 1);
    tick(&c, ts("2026-10-06T00:00:30Z")).await;
    tick(&c, ts("2026-10-06T00:01:30Z")).await;
    assert_eq!(jobs(&db.pool, "cleanup").await.len(), 2);
}

// ---- inactivity --------------------------------------------------------------------------

/// A Free account that last signed in `inactive_days` before `noon()`, with a site.
async fn quiet_free_account(db: &TestDb, email: &str, inactive_days: i32) -> Uuid {
    let id = account(db, email, "free", "UTC", inactive_days + 5).await;
    sqlx::query("UPDATE accounts SET last_login_at = $2::timestamptz - make_interval(days => $3) WHERE id = $1")
        .bind(id)
        .bind(noon().to_string())
        .bind(inactive_days)
        .execute(&db.pool)
        .await
        .unwrap();
    site(db, id, &format!("{}.test", id.simple()), "weekly", -10_000).await;
    id
}

#[tokio::test]
async fn a_free_account_inactive_for_31_days_is_asked_once_and_paused_a_week_later() {
    let db = TestDb::new().await;
    let id = quiet_free_account(&db, "quiet@example.test", 31).await;
    let c = ctx(&db, Mode::Cloud);

    let report = tick(&c, noon()).await;
    assert_clean(&report);
    assert_eq!(report.warned, 1);
    // Many more ticks in the next days: still one email.
    for day in 1..7 {
        assert_eq!(
            tick(&c, noon() + SignedDuration::from_hours(day * 24))
                .await
                .warned,
            0
        );
    }
    let mail = jobs(&db.pool, "send_email").await;
    assert_eq!(mail.len(), 1);
    assert_eq!(mail[0]["to"], "quiet@example.test");
    assert!(
        mail[0]["subject"]
            .as_str()
            .unwrap()
            .contains("Keep monitoring")
    );
    let text = mail[0]["text"].as_str().unwrap();
    let link = text
        .lines()
        .find(|l| l.starts_with("https://codoseo.com/monitoring/resume/"))
        .expect("a resume link");
    let token = link.rsplit('/').next().unwrap();
    let stored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM login_tokens WHERE purpose = 'resume_monitoring' \
           AND account_id = $1 AND token_hash = $2 AND expires_at > now() + interval '6 days'",
    )
    .bind(id)
    .bind(codoseo_web::auth::session::hash(token))
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        stored, 1,
        "the link's token is stored hashed, with a 7 day expiry"
    );
    let paused: bool = sqlx::query_scalar("SELECT paused FROM accounts WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(!paused, "still inside the 7 days");

    // Day 38 with no click, and the email was delivered: paused.
    deliver_mail(&db.pool).await;
    let report = tick(&c, noon() + SignedDuration::from_hours(7 * 24)).await;
    assert_eq!(report.paused, 1);
    let paused: bool = sqlx::query_scalar("SELECT paused FROM accounts WHERE id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(paused);
    assert_eq!(
        jobs(&db.pool, "send_email").await.len(),
        1,
        "no second email"
    );
}

#[tokio::test]
async fn a_warning_that_was_not_delivered_does_not_start_the_pause_clock() {
    let db = TestDb::new().await;
    let id = quiet_free_account(&db, "quiet@example.test", 31).await;
    let c = ctx(&db, Mode::Cloud);
    assert_eq!(tick(&c, noon()).await.warned, 1);
    let paused = |db: &TestDb| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, bool>("SELECT paused FROM accounts WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };

    // A week on, the email is still waiting (a mail outage): not paused.
    let week = noon() + SignedDuration::from_hours(7 * 24);
    let report = tick(&c, week).await;
    assert_eq!((report.paused, report.warnings_reset), (0, 0));
    assert!(!paused(&db).await);
    // Still queued after two weeks: still not paused.
    assert_eq!(
        tick(&c, week + SignedDuration::from_hours(7 * 24))
            .await
            .paused,
        0
    );

    // It finally goes out; the week counts from the warning, so the next tick pauses.
    deliver_mail(&db.pool).await;
    assert_eq!(
        tick(&c, week + SignedDuration::from_hours(7 * 24 + 1))
            .await
            .paused,
        1
    );
    assert!(paused(&db).await);
}

#[tokio::test]
async fn a_warning_email_that_failed_for_good_is_sent_again() {
    let db = TestDb::new().await;
    let id = quiet_free_account(&db, "quiet@example.test", 31).await;
    let c = ctx(&db, Mode::Cloud);
    assert_eq!(tick(&c, noon()).await.warned, 1);
    sqlx::query("UPDATE jobs SET status = 'failed', attempt = 5 WHERE kind = 'send_email'")
        .execute(&db.pool)
        .await
        .unwrap();

    let later = noon() + SignedDuration::from_hours(8 * 24);
    let report = tick(&c, later).await;
    assert_clean(&report);
    assert_eq!(
        (report.warnings_reset, report.warned, report.paused),
        (1, 1, 0)
    );
    let statuses: Vec<String> = sqlx::query_scalar(
        "SELECT status::text FROM jobs WHERE kind = 'send_email' ORDER BY created_at",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(statuses, vec!["failed", "queued"]);
    let sent: Option<time::OffsetDateTime> =
        sqlx::query_scalar("SELECT keep_monitoring_sent_at FROM accounts WHERE id = $1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        sent.unwrap().unix_timestamp(),
        later.as_second(),
        "the clock restarts"
    );
    // Delivered this time: paused only a week after the new warning.
    deliver_mail(&db.pool).await;
    assert_eq!(
        tick(&c, later + SignedDuration::from_hours(6 * 24))
            .await
            .paused,
        0
    );
    assert_eq!(
        tick(&c, later + SignedDuration::from_hours(7 * 24))
            .await
            .paused,
        1
    );
}

#[tokio::test]
async fn using_the_app_counts_as_activity() {
    let db = TestDb::new().await;
    // No sign-in for 40 days, but a session seen yesterday.
    let id = quiet_free_account(&db, "busy@example.test", 40).await;
    sqlx::query(
        "INSERT INTO sessions (account_id, session_hash, expires_at, last_seen_at) \
         VALUES ($1, 'h1', $2::timestamptz + interval '1 day', $2::timestamptz - interval '1 day')",
    )
    .bind(id)
    .bind(noon().to_string())
    .execute(&db.pool)
    .await
    .unwrap();
    let c = ctx(&db, Mode::Cloud);
    assert_eq!(
        tick(&c, noon()).await.warned,
        0,
        "seen yesterday, so not inactive"
    );

    // Thirty-one days after that visit, they are.
    let later = noon() + SignedDuration::from_hours(31 * 24);
    assert_eq!(tick(&c, later).await.warned, 1);
    // A visit after the warning stops the pause.
    deliver_mail(&db.pool).await;
    sqlx::query("UPDATE sessions SET last_seen_at = $1::timestamptz WHERE account_id = $2")
        .bind((later + SignedDuration::from_hours(48)).to_string())
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    let report = tick(&c, later + SignedDuration::from_hours(7 * 24)).await;
    assert_eq!(report.paused, 0);
}

#[tokio::test]
async fn a_click_inside_the_week_keeps_the_account_running() {
    let db = TestDb::new().await;
    let id = quiet_free_account(&db, "quiet@example.test", 31).await;
    let c = ctx(&db, Mode::Cloud);
    tick(&c, noon()).await;
    sqlx::query("UPDATE accounts SET last_email_click_at = $2::timestamptz WHERE id = $1")
        .bind(id)
        .bind((noon() + SignedDuration::from_hours(48)).to_string())
        .execute(&db.pool)
        .await
        .unwrap();
    let report = tick(&c, noon() + SignedDuration::from_hours(8 * 24)).await;
    assert_eq!((report.paused, report.warned), (0, 0));
}

#[tokio::test]
async fn paid_and_self_hosted_accounts_are_never_warned_or_paused() {
    let db = TestDb::new().await;
    let pro = account(&db, "pro@example.test", "pro", "UTC", 120).await;
    site(&db, pro, "pro.test", "daily", -10_000).await;
    let selfhost = account(&db, "self@example.test", "self_hosted", "UTC", 120).await;
    site(&db, selfhost, "self.test", "daily", -10_000).await;
    let free = quiet_free_account(&db, "free@example.test", 90).await;

    let cloud = ctx(&db, Mode::Cloud);
    let report = tick(&cloud, noon()).await;
    assert_eq!(report.warned, 1, "only the Free account");
    deliver_mail(&db.pool).await;
    tick(&cloud, noon() + SignedDuration::from_hours(8 * 24)).await;
    let paused: Vec<(Uuid, bool)> = sqlx::query_as("SELECT id, paused FROM accounts")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    for (id, p) in paused {
        assert_eq!(p, id == free, "only the Free account is paused");
    }

    // The same data in a self-hosted instance: nobody is warned.
    let db = TestDb::new().await;
    quiet_free_account(&db, "free@example.test", 90).await;
    let report = tick(&ctx(&db, Mode::SelfHost), noon()).await;
    assert_eq!((report.warned, report.paused), (0, 0));
    assert!(jobs(&db.pool, "send_email").await.is_empty());
}

#[tokio::test]
async fn a_free_account_with_nothing_monitored_is_not_asked_to_keep_monitoring() {
    let db = TestDb::new().await;
    account(&db, "empty@example.test", "free", "UTC", 60).await;
    let report = tick(&ctx(&db, Mode::Cloud), noon()).await;
    assert_eq!(report.warned, 0);
}

// ---- funnel and heartbeat ----------------------------------------------------------------

#[tokio::test]
async fn active_after_4_weeks_is_recorded_once_per_account() {
    let db = TestDb::new().await;
    let veteran = account(&db, "vet@example.test", "pro", "UTC", 29).await;
    let s = site(&db, veteran, "vet.test", "daily", -600).await;
    done_crawl_at(&db, s, "vet.test", noon() - SignedDuration::from_hours(24)).await;
    let newcomer = account(&db, "new@example.test", "pro", "UTC", 10).await;
    let s2 = site(&db, newcomer, "new.test", "daily", -600).await;
    done_crawl_at(&db, s2, "new.test", noon() - SignedDuration::from_hours(24)).await;
    let stale = account(&db, "stale@example.test", "pro", "UTC", 90).await;
    let s3 = site(&db, stale, "stale.test", "daily", -600).await;
    done_crawl_at(
        &db,
        s3,
        "stale.test",
        noon() - SignedDuration::from_hours(24 * 9),
    )
    .await;

    let c = ctx(&db, Mode::Cloud);
    assert_eq!(tick(&c, noon()).await.active_events, 1);
    assert_eq!(
        tick(&c, noon() + SignedDuration::from_secs(60))
            .await
            .active_events,
        0
    );
    let rows: Vec<(Option<Uuid>, String)> =
        sqlx::query_as("SELECT account_id, kind FROM events WHERE kind = 'active_after_4_weeks'")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![(Some(veteran), "active_after_4_weeks".to_owned())]
    );
}

#[tokio::test]
async fn active_after_4_weeks_of_an_agent_account_is_an_agent_funnel_event() {
    let db = TestDb::new().await;
    let finished = noon() - SignedDuration::from_hours(24);
    // Its first site's first crawl came from start_monitoring.
    let agent = account(&db, "agent@example.test", "free", "UTC", 40).await;
    let s = site(&db, agent, "agent.test", "weekly", -600).await;
    done_crawl_at(&db, s, "agent.test", finished).await;
    sqlx::query("UPDATE crawls SET source = 'agent' WHERE site_id = $1")
        .bind(s)
        .execute(&db.pool)
        .await
        .unwrap();
    // A website account, and one whose second site was an agent's: the first site decides.
    let web = account(&db, "web@example.test", "free", "UTC", 40).await;
    let w = site(&db, web, "web.test", "weekly", -600).await;
    done_crawl_at(&db, w, "web.test", finished).await;
    let mixed = account(&db, "mixed@example.test", "free", "UTC", 40).await;
    let m1 = site(&db, mixed, "first.test", "weekly", -600).await;
    done_crawl_at(&db, m1, "first.test", finished).await;
    let m2 = site(&db, mixed, "second.test", "weekly", -600).await;
    done_crawl_at(&db, m2, "second.test", finished).await;
    sqlx::query("UPDATE crawls SET source = 'agent' WHERE site_id = $1")
        .bind(m2)
        .execute(&db.pool)
        .await
        .unwrap();

    assert_eq!(tick(&ctx(&db, Mode::Cloud), noon()).await.active_events, 3);
    let source = |who: Uuid| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT payload->>'source' FROM events \
                 WHERE kind = 'active_after_4_weeks' AND account_id = $1",
            )
            .bind(who)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(source(agent).await.as_deref(), Some("agent"));
    assert_eq!(source(web).await, None);
    assert_eq!(source(mixed).await, None);

    let by = |f: &[codoseo_store::events::FunnelCount]| {
        f.iter()
            .find(|c| c.kind == codoseo_store::events::EventKind::ActiveAfter4Weeks)
            .unwrap()
            .events
    };
    let agents = codoseo_store::events::agent_funnel_counts(&db.pool, 30)
        .await
        .unwrap();
    let website = codoseo_store::events::funnel_counts(&db.pool, 30)
        .await
        .unwrap();
    assert_eq!((by(&agents), by(&website)), (1, 2));
}

#[tokio::test]
async fn every_tick_records_a_heartbeat() {
    let db = TestDb::new().await;
    assert!(
        codoseo_store::schedule::last_heartbeat(&db.pool)
            .await
            .unwrap()
            .is_none()
    );
    tick(&ctx(&db, Mode::SelfHost), noon()).await;
    let beat = codoseo_store::schedule::last_heartbeat(&db.pool)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(beat.unix_timestamp(), noon().as_second());
    let later = noon() + SignedDuration::from_secs(60);
    tick(&ctx(&db, Mode::SelfHost), later).await;
    let beat = codoseo_store::schedule::last_heartbeat(&db.pool)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(beat.unix_timestamp(), later.as_second());
}

/// A server that counts the requests it gets and answers `status`.
async fn counting_server(status: u16) -> (url::Url, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = url::Url::parse(&format!("http://{}/ping", listener.local_addr().unwrap())).unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                if buf[..n].starts_with(b"GET /ping") {
                    seen.fetch_add(1, Ordering::SeqCst);
                }
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    (url, hits)
}

#[tokio::test]
async fn the_heartbeat_url_is_pinged_and_its_failure_is_never_fatal() {
    let db = TestDb::new().await;
    let (ok, hits) = counting_server(200).await;
    let mut c = ctx(&db, Mode::SelfHost);
    c.heartbeat_url = Some(ok);
    assert_clean(&tick(&c, noon()).await);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let (broken, hits) = counting_server(500).await;
    c.heartbeat_url = Some(broken);
    assert_clean(&tick(&c, noon() + SignedDuration::from_secs(60)).await);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // Nothing is listening at all.
    c.heartbeat_url = Some(url::Url::parse("http://127.0.0.1:9/ping").unwrap());
    assert_clean(&tick(&c, noon() + SignedDuration::from_secs(120)).await);
}

#[tokio::test]
async fn a_failing_step_does_not_stop_the_others() {
    let db = TestDb::new().await;
    sqlx::query("DROP TABLE events")
        .execute(&db.pool)
        .await
        .unwrap();
    let report = tick(&ctx(&db, Mode::SelfHost), noon()).await;
    assert!(
        report
            .failures
            .iter()
            .any(|(step, _)| *step == "active_after_4_weeks")
    );
    assert!(
        codoseo_store::schedule::last_heartbeat(&db.pool)
            .await
            .unwrap()
            .is_some()
    );
}
