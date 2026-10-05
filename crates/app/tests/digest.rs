//! T7.5: the weekly digest against a real throwaway Postgres database: the view built from
//! stored crawls and changes, the `send_digest` job, the cases that send nothing, and the
//! Monday 08:00 hand-off from the scheduler.

#[allow(dead_code)]
mod support;

use std::sync::{Arc, Mutex};

use codoseo::digest::{build_digest, send_digest_at};
use codoseo::jobs::{JobContext, run_job};
use codoseo::scheduler::{SchedulerContext, tick};
use codoseo_core::check::Severity;
use codoseo_core::crawl::AddressPolicy;
use codoseo_notify::{ChannelKey, Email, GuardedHttp, Mailer};
use codoseo_store::jobs::{JobKind, JobQueue};
use codoseo_web::Mode;
use jiff::Timestamp;
use serde_json::json;
use sqlx::PgPool;
use support::TestDb;
use time::macros::datetime;
use time::{Duration, OffsetDateTime};
use url::Url;
use uuid::Uuid;

/// Monday 2026-10-05 08:00 UTC.
fn now() -> OffsetDateTime {
    datetime!(2026-10-05 08:00 UTC)
}

struct World {
    db: TestDb,
    ctx: JobContext,
    mail: Arc<Mutex<Vec<Email>>>,
    account: Uuid,
}

impl World {
    async fn new() -> World {
        World::with(
            Some(Url::parse("https://rankorg.test/start").unwrap()),
            "UTC",
        )
        .await
    }

    async fn with(rankorg_url: Option<Url>, timezone: &str) -> World {
        let db = TestDb::new().await;
        let (mailer, mail) = Mailer::capture();
        let ctx = JobContext {
            pool: db.pool.clone(),
            mailer,
            channel_key: ChannelKey::derive("test secret"),
            base_url: Url::parse("https://codoseo.test").unwrap(),
            http: GuardedHttp::new(AddressPolicy::AllowPrivate).unwrap(),
            rankorg_url,
        };
        let account = sqlx::query_scalar(
            "INSERT INTO accounts (email, email_canonical, timezone) \
             VALUES ('owner@example.com', 'owner@example.com', $1) RETURNING id",
        )
        .bind(timezone)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        World {
            db,
            ctx,
            mail,
            account,
        }
    }

    fn pool(&self) -> &PgPool {
        &self.db.pool
    }

    async fn site(&self, domain: &str) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO sites (account_id, domain, start_url) VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(self.account)
        .bind(domain)
        .bind(format!("https://{domain}/"))
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    /// A done crawl finished `hours_ago` hours before `at`.
    async fn crawl_at(
        &self,
        at: OffsetDateTime,
        site: Uuid,
        hours_ago: i64,
        score: i16,
        passed: i16,
        counts: &[(&str, u32)],
    ) -> Uuid {
        let finished = at - Duration::hours(hours_ago);
        let summary = json!({
            "stop_reason": {"kind": "completed"},
            "report_summary": {},
            "counts": counts.iter().map(|(s, n)| json!([s, n])).collect::<Vec<_>>(),
        });
        sqlx::query_scalar(
            "INSERT INTO crawls (site_id, domain, status, trigger, priority, queued_at, started_at, \
                                 finished_at, health_score, checks_passed, checks_total, summary) \
             VALUES ($1, 'x', 'done', 'schedule', 3, $2, $2, $2, $3, $4, 40, $5) RETURNING id",
        )
        .bind(site)
        .bind(finished)
        .bind(score)
        .bind(passed)
        .bind(summary)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    async fn crawl(
        &self,
        site: Uuid,
        hours_ago: i64,
        score: i16,
        passed: i16,
        counts: &[(&str, u32)],
    ) -> Uuid {
        self.crawl_at(now(), site, hours_ago, score, passed, counts)
            .await
    }

    async fn change(&self, crawl: Uuid, site: Uuid, severity: &str, url: &str) {
        sqlx::query(
            "INSERT INTO changes (crawl_id, site_id, kind, severity, url, before_value, after_value, created_at) \
             VALUES ($1, $2, 'status_changed', $3::severity, $4, '200', '404', $5)",
        )
        .bind(crawl)
        .bind(site)
        .bind(severity)
        .bind(url)
        .bind(now() - Duration::hours(2))
        .execute(self.pool())
        .await
        .unwrap();
    }

    /// The usual site: 9 days ago 89/40 passing with two failing checks, now 92 with other ones.
    async fn example(&self) -> Uuid {
        let s = self.site("example.com").await;
        self.crawl(
            s,
            9 * 24,
            89,
            35,
            &[("title_too_long", 9), ("description_missing", 6)],
        )
        .await;
        let c = self
            .crawl(s, 5, 92, 37, &[("title_too_long", 11), ("http_5xx", 2)])
            .await;
        self.change(c, s, "critical", "https://example.com/pricing")
            .await;
        self.change(c, s, "notice", "https://example.com/blog")
            .await;
        s
    }

    fn sent(&self) -> Vec<Email> {
        self.mail.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn the_digest_for_one_site_has_the_headline_the_delta_the_issues_and_the_changes() {
    let w = World::new().await;
    let site = w.example().await;

    assert!(send_digest_at(&w.ctx, w.account, now()).await.unwrap());

    let mails = w.sent();
    assert_eq!(mails.len(), 1);
    let mail = &mails[0];
    assert_eq!(mail.to, "owner@example.com");
    assert_eq!(mail.subject, "CodoSEO weekly: example.com 92 (+3)");
    let html = mail.html.as_deref().expect("an HTML part");
    for body in [mail.text.as_str(), html] {
        assert!(
            body.contains("Your site passed 37/40 checks. 1 new issue."),
            "{body}"
        );
        // Titles come from the checks registry.
        assert!(body.contains("Page returns a 5xx error"));
        assert!(body.contains("Meta description is missing"), "resolved");
        // A check failing on both dates is neither new nor resolved.
        assert!(!body.contains("Title is over 60 characters"));
        assert!(body.contains("Sep 28 to Oct 5, 2026"));
        assert!(body.contains(&format!("https://codoseo.test/s/{site}/audit")));
        assert!(body.contains("https://example.com/pricing"));
        assert!(body.contains("https://codoseo.test/settings/alerts"));
    }
    assert!(
        mail.text
            .contains("Changes this week: 1 critical, 1 notice.")
    );
}

#[tokio::test]
async fn a_changes_summary_ignores_whether_an_instant_alert_was_routed() {
    let w = World::new().await;
    let site = w.example().await;
    sqlx::query("UPDATE changes SET alerted_at = now() WHERE severity = 'critical'")
        .execute(w.pool())
        .await
        .unwrap();
    let view = build_digest(&w.ctx, w.account, now())
        .await
        .unwrap()
        .unwrap();
    let digest = &view.sites[0];
    assert_eq!(digest.changes_by_severity.critical, 1);
    assert_eq!(digest.top_changes[0].severity, Severity::Critical);
    assert_eq!(
        digest.dashboard_url,
        format!("https://codoseo.test/s/{site}/audit")
    );
}

#[tokio::test]
async fn the_view_carries_the_delta_in_each_state() {
    let w = World::new().await;
    let up = w.site("up.test").await;
    w.crawl(up, 8 * 24, 80, 30, &[]).await;
    w.crawl(up, 1, 85, 32, &[]).await;
    let down = w.site("down.test").await;
    w.crawl(down, 8 * 24, 80, 30, &[]).await;
    w.crawl(down, 1, 77, 29, &[]).await;
    let first = w.site("first.test").await;
    w.crawl(first, 1, 60, 25, &[("http_5xx", 1)]).await;

    let view = build_digest(&w.ctx, w.account, now())
        .await
        .unwrap()
        .unwrap();
    let by = |d: &str| view.sites.iter().find(|s| s.domain == d).unwrap();
    assert_eq!(by("up.test").score_delta, Some(5));
    assert_eq!(by("down.test").score_delta, Some(-3));
    assert_eq!(by("first.test").score_delta, None);
    // A first week has nothing to compare, so nothing is "new".
    assert!(by("first.test").new_issues.is_empty());
    assert_eq!(view.subject(), "CodoSEO weekly: 3 sites");
}

#[tokio::test]
async fn new_issues_are_listed_most_severe_first() {
    let w = World::new().await;
    let s = w.site("example.com").await;
    w.crawl(s, 8 * 24, 80, 30, &[]).await;
    w.crawl(
        s,
        1,
        70,
        28,
        &[
            ("images_missing_alt", 40),
            ("http_5xx", 2),
            ("title_too_long", 7),
            ("http_4xx", 5),
            ("not_a_check_from_the_future", 3),
        ],
    )
    .await;
    let view = build_digest(&w.ctx, w.account, now())
        .await
        .unwrap()
        .unwrap();
    let titles: Vec<&str> = view.sites[0]
        .new_issues
        .iter()
        .map(|(t, _)| t.as_str())
        .collect();
    // Critical first (more pages first), then warnings, then notices; unknown checks skipped.
    assert_eq!(
        titles[..2],
        ["Page returns a 4xx error", "Page returns a 5xx error"]
    );
    assert_eq!(titles.len(), 4, "{titles:?}");
    assert_eq!(view.sites[0].new_issues[0].1, 5);
}

#[tokio::test]
async fn an_account_without_a_finished_crawl_sends_nothing() {
    let w = World::new().await;
    let s = w.site("example.com").await;
    sqlx::query(
        "INSERT INTO crawls (site_id, domain, status, trigger, priority) VALUES ($1, 'x', 'queued', 'first', 1)",
    )
    .bind(s)
    .execute(w.pool())
    .await
    .unwrap();
    assert!(!send_digest_at(&w.ctx, w.account, now()).await.unwrap());
    assert!(w.sent().is_empty());

    // No sites at all, and an account that no longer exists: also fine.
    let empty = World::new().await;
    assert!(
        !send_digest_at(&empty.ctx, empty.account, now())
            .await
            .unwrap()
    );
    assert!(
        !send_digest_at(&empty.ctx, Uuid::new_v4(), now())
            .await
            .unwrap()
    );
    assert!(empty.sent().is_empty());
}

#[tokio::test]
async fn sites_that_are_not_monitored_or_have_no_crawl_are_left_out() {
    let w = World::new().await;
    w.example().await;
    let paused = w.site("paused.test").await;
    w.crawl(paused, 1, 50, 20, &[]).await;
    sqlx::query("UPDATE sites SET monitoring_active = false WHERE id = $1")
        .bind(paused)
        .execute(w.pool())
        .await
        .unwrap();
    w.site("never-crawled.test").await;

    send_digest_at(&w.ctx, w.account, now()).await.unwrap();
    let mails = w.sent();
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].subject, "CodoSEO weekly: example.com 92 (+3)");
    assert!(!mails[0].text.contains("paused.test"));
    assert!(!mails[0].text.contains("never-crawled.test"));
}

#[tokio::test]
async fn a_paused_account_gets_no_digest() {
    let w = World::new().await;
    w.example().await;
    sqlx::query("UPDATE accounts SET paused = true WHERE id = $1")
        .bind(w.account)
        .execute(w.pool())
        .await
        .unwrap();
    assert!(!send_digest_at(&w.ctx, w.account, now()).await.unwrap());
    assert!(w.sent().is_empty());
}

#[tokio::test]
async fn the_rankorg_line_is_cloud_only() {
    let cloud = World::new().await;
    let site = cloud.example().await;
    let top = cloud.crawl(site, 1, 92, 37, &[]).await;
    sqlx::query(
        "INSERT INTO pages (crawl_id, site_id, url, url_hash, status, indexability, depth, inlinks) \
         VALUES ($1, $2, 'https://example.com/best', 1, 200, 'indexable', 1, 9)",
    )
    .bind(top)
    .bind(site)
    .execute(cloud.pool())
    .await
    .unwrap();
    send_digest_at(&cloud.ctx, cloud.account, now())
        .await
        .unwrap();
    let mail = &cloud.sent()[0];
    assert!(
        mail.text
            .contains("https://rankorg.test/start?domain=example.com"),
        "{}",
        mail.text
    );
    assert!(mail.text.contains("page=https%3A%2F%2Fexample.com%2Fbest"));
    assert!(mail.text.contains("utm_medium=digest"));

    let selfhost = World::with(None, "UTC").await;
    selfhost.example().await;
    send_digest_at(&selfhost.ctx, selfhost.account, now())
        .await
        .unwrap();
    let mail = &selfhost.sent()[0];
    assert!(!mail.text.to_lowercase().contains("rankorg"));
    assert!(
        !mail
            .html
            .as_deref()
            .unwrap()
            .to_lowercase()
            .contains("rankorg")
    );
}

#[tokio::test]
async fn the_week_label_uses_the_accounts_time_zone() {
    // 23:30 UTC on Monday is already Tuesday the 6th in Auckland.
    let w = World::with(None, "Pacific/Auckland").await;
    w.example().await;
    let late = datetime!(2026-10-05 23:30 UTC);
    let view = build_digest(&w.ctx, w.account, late)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(view.week_label, "Sep 29 to Oct 6, 2026");
}

// ---- the job ------------------------------------------------------------------------------

#[tokio::test]
async fn the_send_digest_job_sends_one_email_with_both_parts() {
    let w = World::new().await;
    let site = w.site("example.com").await;
    let real_now = OffsetDateTime::now_utc();
    w.crawl_at(real_now, site, 9 * 24, 89, 35, &[("title_too_long", 9)])
        .await;
    w.crawl_at(real_now, site, 5, 92, 37, &[]).await;

    let queue = JobQueue::new(w.pool().clone());
    queue
        .enqueue(JobKind::SendDigest, json!({ "account_id": w.account }))
        .await
        .unwrap();
    let job = queue.claim("t").await.unwrap().unwrap();
    run_job(&w.ctx, job).await.unwrap();

    let mails = w.sent();
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].subject, "CodoSEO weekly: example.com 92 (+3)");
    assert!(mails[0].html.is_some() && !mails[0].text.is_empty());
}

#[tokio::test]
async fn a_bad_payload_fails_the_job() {
    let w = World::new().await;
    let queue = JobQueue::new(w.pool().clone());
    queue.enqueue(JobKind::SendDigest, json!({})).await.unwrap();
    let job = queue.claim("t").await.unwrap().unwrap();
    let error = run_job(&w.ctx, job).await.unwrap_err();
    assert!(error.contains("send_digest payload"), "{error}");
}

// ---- Monday 08:00 local ---------------------------------------------------------------------

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

#[tokio::test]
async fn a_digest_goes_out_on_monday_at_eight_in_the_accounts_time_zone() {
    let w = World::with(None, "Asia/Kolkata").await;
    let site = w.site("example.com").await;
    let real_now = OffsetDateTime::now_utc();
    w.crawl_at(real_now, site, 9 * 24, 89, 35, &[]).await;
    w.crawl_at(real_now, site, 5, 92, 37, &[]).await;
    let sched = SchedulerContext {
        pool: w.pool().clone(),
        mode: Mode::Cloud,
        base_url: Url::parse("https://codoseo.test").unwrap(),
        heartbeat_url: None,
        http: reqwest::Client::new(),
    };
    let queue = JobQueue::new(w.pool().clone());

    // Monday 07:55 in Kolkata: not yet.
    tick(&sched, ts("2026-10-05T02:25:00Z")).await;
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'send_digest'")
        .fetch_one(w.pool())
        .await
        .unwrap();
    assert_eq!(queued, 0);
    assert!(w.sent().is_empty());

    // 08:05 in Kolkata: the scheduler queues the job and the handler sends the email.
    let report = tick(&sched, ts("2026-10-05T02:35:00Z")).await;
    assert_eq!(report.digests_queued, 1);
    let mut ran = 0;
    while let Some(job) = queue.claim("t").await.unwrap() {
        let id = job.id;
        if job.kind == JobKind::SendDigest {
            ran += 1;
        }
        run_job(&w.ctx, job).await.unwrap();
        queue.complete(id, "t").await.unwrap();
    }
    assert_eq!(ran, 1);
    let digests: Vec<Email> = w
        .sent()
        .into_iter()
        .filter(|m| m.subject.starts_with("CodoSEO weekly"))
        .collect();
    assert_eq!(digests.len(), 1);

    // Later the same morning nothing more is queued.
    assert_eq!(
        tick(&sched, ts("2026-10-05T03:35:00Z"))
            .await
            .digests_queued,
        0
    );
}
