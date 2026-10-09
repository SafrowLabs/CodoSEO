//! T7.4: alert planning and delivery against a real throwaway Postgres database and local HTTP
//! servers: grouped messages, per-channel retries, key-page filtering, plan limits, the
//! "couldn't reach your site" alert and the channel that gets switched off.

#[allow(dead_code)]
mod support;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::http::StatusCode;
use axum::routing::any;
use codoseo::alerts;
use codoseo::jobs::{JobContext, run_job};
use codoseo_core::change::{Change, ChangeKind};
use codoseo_core::check::IssueBits;
use codoseo_core::crawl::{AddressPolicy, SitemapSummary};
use codoseo_core::output::StopReason;
use codoseo_core::page::{Indexability, JsonLdStatus, OgTags, PageFields, PageRecord};
use codoseo_core::snapshot::Snapshot;
use codoseo_notify::{ChannelKey, ChannelTarget, Email, GuardedHttp, Mailer};
use codoseo_store::alert_rules;
use codoseo_store::channels;
use codoseo_store::crawl_queue::{CrawlQueue, CrawlTrigger};
use codoseo_store::jobs::{JobKind, JobQueue};
use codoseo_testkit::TestServer;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::TestDb;
use url::Url;
use uuid::Uuid;

// ---- world -------------------------------------------------------------------------------

struct World {
    db: TestDb,
    ctx: JobContext,
    mail: Arc<Mutex<Vec<Email>>>,
    queue: JobQueue,
    account: Uuid,
    site: Uuid,
    crawl: Uuid,
}

fn key() -> ChannelKey {
    ChannelKey::derive("test secret")
}

impl World {
    async fn new(plan: &str) -> World {
        let db = TestDb::new().await;
        let (mailer, mail) = Mailer::capture();
        let ctx = JobContext {
            pool: db.pool.clone(),
            mailer,
            channel_key: key(),
            base_url: Url::parse("https://codoseo.test").unwrap(),
            http: GuardedHttp::new(AddressPolicy::AllowPrivate).unwrap(),
            rankorg_url: None,
        };
        let account: Uuid = sqlx::query_scalar(
            "INSERT INTO accounts (email, email_canonical, plan) VALUES ('owner@example.com', 'owner@example.com', $1::plan) RETURNING id",
        )
        .bind(plan)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let site: Uuid = sqlx::query_scalar(
            "INSERT INTO sites (account_id, domain, start_url) VALUES ($1, 'example.com', 'https://example.com/') RETURNING id",
        )
        .bind(account)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let crawl = new_crawl(&db.pool, site, "schedule").await;
        World {
            queue: JobQueue::new(db.pool.clone()),
            db,
            ctx,
            mail,
            account,
            site,
            crawl,
        }
    }

    fn pool(&self) -> &PgPool {
        &self.db.pool
    }

    /// The default email channel with its default rules.
    async fn email_channel(&self) -> Uuid {
        let id = channels::ensure_default_email(self.pool(), &key(), self.account)
            .await
            .unwrap();
        alert_rules::create_defaults(self.pool(), self.site, id)
            .await
            .unwrap();
        id
    }

    /// A webhook channel (or Slack, to a local server) with default rules.
    async fn http_channel(&self, kind: &str, url: Url) -> Uuid {
        let target = match kind {
            "slack" => ChannelTarget::Slack { url },
            _ => ChannelTarget::Webhook {
                url,
                secret: "whsec_test".to_owned(),
            },
        };
        let id = channels::create(self.pool(), &key(), self.account, &target, None, false)
            .await
            .unwrap();
        alert_rules::create_defaults(self.pool(), self.site, id)
            .await
            .unwrap();
        id
    }

    async fn change(&self, kind: ChangeKind, url: Option<&str>) -> i64 {
        insert_change(self.pool(), self.crawl, self.site, kind, url).await
    }

    async fn page(&self, path: &str, depth: i32, inlinks: i32) {
        sqlx::query(
            "INSERT INTO pages (crawl_id, site_id, url, url_hash, status, indexability, depth, inlinks) \
             VALUES ($1, $2, $3, $4, 200, 'indexable', $5, $6)",
        )
        .bind(self.crawl)
        .bind(self.site)
        .bind(format!("https://example.com{path}"))
        .bind(codoseo_store::hash::to_db(codoseo_core::url::url_hash(
            &Url::parse(&format!("https://example.com{path}")).unwrap(),
        )))
        .bind(depth)
        .bind(inlinks)
        .execute(self.pool())
        .await
        .unwrap();
    }

    /// Runs every job that is due, like the job loop does, until none is left.
    async fn drain(&self) {
        drain(&self.ctx, self.pool()).await;
    }

    async fn jobs(&self, kind: &str) -> Vec<Value> {
        sqlx::query_scalar(
            "SELECT payload FROM jobs WHERE kind = $1::job_kind ORDER BY created_at, id",
        )
        .bind(kind)
        .fetch_all(self.pool())
        .await
        .unwrap()
    }

    /// The delivery jobs (those naming a channel), as (channel, change count or unreachable).
    async fn deliveries(&self) -> Vec<Value> {
        self.jobs("send_alert")
            .await
            .into_iter()
            .filter(|p| p.get("channel_id").is_some())
            .collect()
    }

    async fn alerted(&self, id: i64) -> bool {
        sqlx::query_scalar("SELECT alerted_at IS NOT NULL FROM changes WHERE id = $1")
            .bind(id)
            .fetch_one(self.pool())
            .await
            .unwrap()
    }

    fn mail_to(&self, to: &str) -> Vec<Email> {
        self.mail
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.to == to)
            .cloned()
            .collect()
    }
}

async fn new_crawl(pool: &PgPool, site: Uuid, trigger: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, finished_at) \
         VALUES ($1, 'example.com', $2::crawl_trigger, 3, 'done', now()) RETURNING id",
    )
    .bind(site)
    .bind(trigger)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn insert_change(
    pool: &PgPool,
    crawl: Uuid,
    site: Uuid,
    kind: ChangeKind,
    url: Option<&str>,
) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO changes (crawl_id, site_id, kind, severity, url, before_value, after_value) \
         VALUES ($1, $2, $3::change_kind, 'warning', $4, 'before', 'after') RETURNING id",
    )
    .bind(crawl)
    .bind(site)
    .bind(kind.slug())
    .bind(url)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Claims and runs due jobs the way `job_loop` settles them: complete on `Ok`, retry on `Err`.
async fn drain(ctx: &JobContext, pool: &PgPool) {
    let queue = JobQueue::new(pool.clone());
    while let Some(job) = queue.claim("t").await.unwrap() {
        let id = job.id;
        match run_job(ctx, job).await {
            Ok(()) => queue.complete(id, "t").await.unwrap(),
            Err(e) => queue.retry(id, "t", &e).await.unwrap(),
        };
    }
}

/// Makes every queued job due now (retries back off for minutes).
async fn make_due(pool: &PgPool) {
    sqlx::query("UPDATE jobs SET run_after = now() WHERE status = 'queued'")
        .execute(pool)
        .await
        .unwrap();
}

// ---- local servers -----------------------------------------------------------------------

#[derive(Clone)]
struct Hook {
    status: Arc<AtomicU16>,
    bodies: Arc<Mutex<Vec<Value>>>,
}

impl Hook {
    async fn start(status: u16) -> (TestServer, Hook) {
        let hook = Hook {
            status: Arc::new(AtomicU16::new(status)),
            bodies: Arc::new(Mutex::new(Vec::new())),
        };
        let state = hook.clone();
        let app = Router::new().fallback(any(move |body: Bytes| {
            let state = state.clone();
            async move {
                if let Ok(v) = serde_json::from_slice::<Value>(&body) {
                    state.bodies.lock().unwrap().push(v);
                }
                StatusCode::from_u16(state.status.load(Ordering::SeqCst)).unwrap()
            }
        }));
        (TestServer::start(app).await, hook)
    }

    fn set_status(&self, status: u16) {
        self.status.store(status, Ordering::SeqCst);
    }

    fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().unwrap().clone()
    }
}

// ---- alert storms (review focus 4) --------------------------------------------------------

#[tokio::test]
async fn four_hundred_changes_on_two_channels_make_two_messages_of_twenty_and_380_more() {
    let w = World::new("pro").await;
    let (server, hook) = Hook::start(200).await;
    let email = w.email_channel().await;
    let webhook = w.http_channel("webhook", server.url("/hook")).await;
    for channel in [email, webhook] {
        alert_rules::set(w.pool(), w.site, ChangeKind::NewUrl, channel, true)
            .await
            .unwrap();
    }
    for i in 0..400 {
        w.change(
            ChangeKind::NewUrl,
            Some(&format!("https://example.com/p{i}")),
        )
        .await;
    }
    w.queue
        .enqueue(JobKind::SendAlert, json!({ "crawl_id": w.crawl }))
        .await
        .unwrap();

    w.drain().await;

    // Exactly two delivery jobs, one per channel, each carrying all 400 changes.
    let deliveries = w.deliveries().await;
    assert_eq!(deliveries.len(), 2, "{deliveries:?}");
    let channels: BTreeSet<String> = deliveries
        .iter()
        .map(|d| d["channel_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        channels,
        [email, webhook].iter().map(|c| c.to_string()).collect()
    );
    for d in &deliveries {
        assert_eq!(d["change_ids"].as_array().unwrap().len(), 400);
    }
    let unalerted: i64 =
        sqlx::query_scalar("SELECT count(*) FROM changes WHERE alerted_at IS NULL")
            .fetch_one(w.pool())
            .await
            .unwrap();
    assert_eq!(unalerted, 0);

    // One message each, listing 20 and "and 380 more".
    let bodies = hook.bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["event"], "changes");
    assert_eq!(bodies[0]["changes"].as_array().unwrap().len(), 20);
    assert_eq!(bodies[0]["more"], 380);
    let mails = w.mail_to("owner@example.com");
    assert_eq!(mails.len(), 1);
    assert!(mails[0].text.contains("and 380 more"), "{}", mails[0].text);
    assert_eq!(mails[0].text.matches("[Warning]").count(), 20);
}

#[tokio::test]
async fn a_retried_delivery_does_not_resend_to_the_channel_that_already_got_it() {
    let w = World::new("pro").await;
    let (server, hook) = Hook::start(500).await;
    w.email_channel().await;
    let webhook = w.http_channel("webhook", server.url("/hook")).await;
    w.change(ChangeKind::ErrorSpike, None).await;
    w.queue
        .enqueue(JobKind::SendAlert, json!({ "crawl_id": w.crawl }))
        .await
        .unwrap();

    w.drain().await;
    assert_eq!(w.mail_to("owner@example.com").len(), 1, "email went out");
    assert_eq!(hook.bodies().len(), 1, "the webhook was tried and failed");
    let (status, attempt, error): (String, i16, Option<String>) = sqlx::query_as(
        "SELECT status::text, attempt, last_error FROM jobs \
         WHERE kind = 'send_alert' AND payload->>'channel_id' = $1",
    )
    .bind(webhook.to_string())
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!((status.as_str(), attempt), ("queued", 1));
    assert!(error.unwrap().contains("500"));
    let listed = channels::list_for_account(w.pool(), &key(), w.account)
        .await
        .unwrap();
    let failing = listed.iter().find(|c| c.id == webhook).unwrap();
    assert_eq!(failing.consecutive_failures, 1);
    assert!(failing.enabled, "one failure does not switch it off");

    // The webhook recovers; only its job runs again.
    hook.set_status(200);
    make_due(w.pool()).await;
    w.drain().await;
    assert_eq!(hook.bodies().len(), 2);
    assert_eq!(w.mail_to("owner@example.com").len(), 1, "no second email");
    let listed = channels::list_for_account(w.pool(), &key(), w.account)
        .await
        .unwrap();
    let recovered = listed.iter().find(|c| c.id == webhook).unwrap();
    assert_eq!(recovered.consecutive_failures, 0);
    assert_eq!(recovered.last_error, None);
}

// ---- key pages -----------------------------------------------------------------------------

#[tokio::test]
async fn became_noindex_is_instant_only_on_key_pages() {
    let w = World::new("free").await;
    w.email_channel().await;
    // The homepage, 30 pages with rising inlinks (the top 20 are p10..p29) and a starred page.
    w.page("/", 0, 0).await;
    for i in 0..30 {
        w.page(&format!("/p{i}"), 1, 50 + i).await;
    }
    w.page("/starred", 1, 0).await;
    w.page("/boring", 1, 0).await;
    let starred = codoseo_core::url::url_hash(&Url::parse("https://example.com/starred").unwrap());
    sqlx::query("UPDATE sites SET key_pages = $2 WHERE id = $1")
        .bind(w.site)
        .bind(vec![codoseo_store::hash::to_db(starred)])
        .execute(w.pool())
        .await
        .unwrap();

    let mut instant = Vec::new();
    let mut digest = Vec::new();
    for (path, goes_instant) in [
        ("/", true),
        ("/p29", true),
        ("/p10", true),
        ("/starred", true),
        ("/p9", false),
        ("/p0", false),
        ("/boring", false),
    ] {
        let id = w
            .change(
                ChangeKind::BecameNoindex,
                Some(&format!("https://example.com{path}")),
            )
            .await;
        if goes_instant {
            &mut instant
        } else {
            &mut digest
        }
        .push(id);
    }
    w.queue
        .enqueue(JobKind::SendAlert, json!({ "crawl_id": w.crawl }))
        .await
        .unwrap();
    w.drain().await;

    let deliveries = w.deliveries().await;
    assert_eq!(deliveries.len(), 1);
    let routed: BTreeSet<i64> = deliveries[0]["change_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    assert_eq!(routed, instant.iter().copied().collect());
    for id in &instant {
        assert!(w.alerted(*id).await);
    }
    for id in &digest {
        assert!(!w.alerted(*id).await, "left for the digest");
    }
}

// ---- the diff's spike threshold, end to end -------------------------------------------------

fn record(path: &str, status: u16) -> PageRecord {
    let url = Url::parse(&format!("https://example.com{path}")).unwrap();
    let mut p = PageRecord {
        url_hash: codoseo_core::url::url_hash(&url),
        url,
        status,
        redirect_chain: Vec::new(),
        response_ms: 10,
        size_bytes: 100,
        content_type: Some("text/html".to_owned()),
        depth: Some(1),
        in_sitemap: true,
        indexability: if status >= 400 {
            Indexability::ClientError
        } else {
            Indexability::Indexable
        },
        fields: PageFields {
            title: Some(format!("Title {path}")),
            title_count: 1,
            meta_description: None,
            meta_robots: None,
            x_robots_tag: None,
            canonical: None,
            hreflang: Vec::new(),
            h1: vec!["h".to_owned()],
            h2: Vec::new(),
            word_count: 100,
            content_hash: 1,
            images_missing_alt: 0,
            og: OgTags::default(),
            jsonld: JsonLdStatus::default(),
            mixed_content: 0,
            ai: Default::default(),
        },
        inlinks: 1,
        outlinks_internal: 0,
        outlinks_external: 0,
        issues: IssueBits::default(),
        key_hash: 0,
        redirect_target: None,
        outlinks_nofollow: 0,
        error: None,
    };
    p.key_hash = p.compute_key_hash();
    p
}

fn snapshot(failing: usize) -> Snapshot {
    // 200 pages (the home page and 199 others); the first `failing` of the others are 404.
    let mut pages = vec![record("/", 200)];
    for i in 0..199 {
        pages.push(record(
            &format!("/p{i}"),
            if i < failing { 404 } else { 200 },
        ));
    }
    Snapshot {
        origin: Url::parse("https://example.com/").unwrap(),
        stop: StopReason::Completed,
        pages,
        robots: None,
        sitemap: SitemapSummary::default(),
    }
}

async fn alert_for(failing: usize) -> (World, Vec<Change>) {
    let w = World::new("free").await;
    w.email_channel().await;
    let changes = codoseo_diff::diff(&snapshot(0), &snapshot(failing), &Default::default());
    for c in &changes {
        insert_change(
            w.pool(),
            w.crawl,
            w.site,
            c.kind,
            c.url.as_ref().map(Url::as_str),
        )
        .await;
    }
    w.queue
        .enqueue(JobKind::SendAlert, json!({ "crawl_id": w.crawl }))
        .await
        .unwrap();
    w.drain().await;
    (w, changes)
}

#[tokio::test]
async fn four_new_404s_on_a_200_page_site_alert_as_a_spike() {
    let (w, changes) = alert_for(4).await;
    assert!(changes.iter().any(|c| c.kind == ChangeKind::ErrorSpike));
    let mails = w.mail_to("owner@example.com");
    assert_eq!(mails.len(), 1);
    assert!(mails[0].text.contains("Error spike"), "{}", mails[0].text);
    // Only the spike was instant: the four status changes wait for the digest.
    let deliveries = w.deliveries().await;
    assert_eq!(deliveries[0]["change_ids"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn two_new_404s_on_a_200_page_site_are_no_spike_and_nothing_is_sent() {
    let (w, changes) = alert_for(2).await;
    assert!(!changes.iter().any(|c| c.kind == ChangeKind::ErrorSpike));
    assert!(!changes.is_empty());
    assert!(w.deliveries().await.is_empty());
    assert!(w.mail.lock().unwrap().is_empty());
}

// ---- plans and channel state -----------------------------------------------------------------

#[tokio::test]
async fn a_free_accounts_slack_channel_is_skipped_and_its_email_alerted() {
    let w = World::new("free").await;
    let (server, hook) = Hook::start(200).await;
    w.email_channel().await;
    let slack = w.http_channel("slack", server.url("/slack")).await;
    w.change(ChangeKind::ErrorSpike, None).await;
    w.queue
        .enqueue(JobKind::SendAlert, json!({ "crawl_id": w.crawl }))
        .await
        .unwrap();
    w.drain().await;

    assert_eq!(w.deliveries().await.len(), 1, "only the email channel");
    assert_eq!(w.mail_to("owner@example.com").len(), 1);
    assert!(hook.bodies().is_empty());

    // A job planned before a downgrade is dropped at delivery too.
    w.queue
        .enqueue(
            JobKind::SendAlert,
            json!({ "crawl_id": w.crawl, "channel_id": slack, "unreachable": true }),
        )
        .await
        .unwrap();
    w.drain().await;
    assert!(hook.bodies().is_empty());
}

#[tokio::test]
async fn a_pro_accounts_slack_channel_is_alerted() {
    let w = World::new("pro").await;
    let (server, hook) = Hook::start(200).await;
    w.email_channel().await;
    w.http_channel("slack", server.url("/slack")).await;
    w.change(ChangeKind::ErrorSpike, None).await;
    w.queue
        .enqueue(JobKind::SendAlert, json!({ "crawl_id": w.crawl }))
        .await
        .unwrap();
    w.drain().await;
    assert_eq!(hook.bodies().len(), 1);
    assert!(
        hook.bodies()[0]["text"]
            .as_str()
            .unwrap()
            .contains("example.com")
    );
}

#[tokio::test]
async fn muted_and_disabled_channels_get_nothing_and_their_changes_wait_for_the_digest() {
    let w = World::new("pro").await;
    let email = w.email_channel().await;
    channels::set_muted(w.pool(), w.account, email, true)
        .await
        .unwrap();
    let id = w.change(ChangeKind::ErrorSpike, None).await;
    w.queue
        .enqueue(JobKind::SendAlert, json!({ "crawl_id": w.crawl }))
        .await
        .unwrap();
    w.drain().await;
    assert!(w.deliveries().await.is_empty());
    assert!(!w.alerted(id).await);
    assert!(w.mail.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_delivery_to_a_channel_that_is_gone_muted_or_off_is_dropped_quietly() {
    let w = World::new("pro").await;
    let (server, hook) = Hook::start(200).await;
    let muted = w.http_channel("webhook", server.url("/a")).await;
    let off = w.http_channel("webhook", server.url("/b")).await;
    let gone = w.http_channel("webhook", server.url("/c")).await;
    channels::set_muted(w.pool(), w.account, muted, true)
        .await
        .unwrap();
    channels::disable(w.pool(), off, "boom").await.unwrap();
    channels::delete(w.pool(), w.account, gone).await.unwrap();
    for channel in [muted, off, gone] {
        w.queue
            .enqueue(
                JobKind::SendAlert,
                json!({ "crawl_id": w.crawl, "channel_id": channel, "unreachable": true }),
            )
            .await
            .unwrap();
    }
    w.drain().await;
    assert!(hook.bodies().is_empty());
    let failed: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE status <> 'done'")
        .fetch_one(w.pool())
        .await
        .unwrap();
    assert_eq!(failed, 0);
}

#[tokio::test]
async fn a_site_with_no_rules_or_email_channel_yet_still_gets_the_default_alert() {
    let w = World::new("free").await;
    w.change(ChangeKind::SiteMoved, None).await;
    w.queue
        .enqueue(JobKind::SendAlert, json!({ "crawl_id": w.crawl }))
        .await
        .unwrap();
    w.drain().await;
    assert_eq!(w.mail_to("owner@example.com").len(), 1);
}

#[tokio::test]
async fn planning_covers_a_slack_channel_added_before_the_site_had_rules() {
    let w = World::new("pro").await;
    let (server, hook) = Hook::start(200).await;
    // A channel with no rules on this site (it was added before the site was).
    let target = ChannelTarget::Slack {
        url: server.url("/slack"),
    };
    let slack = channels::create(w.pool(), &key(), w.account, &target, None, false)
        .await
        .unwrap();
    w.change(ChangeKind::SiteMoved, None).await;
    w.queue
        .enqueue(JobKind::SendAlert, json!({ "crawl_id": w.crawl }))
        .await
        .unwrap();
    w.drain().await;
    assert_eq!(hook.bodies().len(), 1, "the slack channel got the alert");
    assert_eq!(w.mail_to("owner@example.com").len(), 1);
    let on = alert_rules::instant_channels_for(w.pool(), w.site, ChangeKind::SiteMoved)
        .await
        .unwrap();
    assert!(on.contains(&slack));
}

#[tokio::test]
async fn planning_twice_does_not_queue_a_second_delivery() {
    let w = World::new("pro").await;
    w.email_channel().await;
    w.change(ChangeKind::ErrorSpike, None).await;
    alerts::plan_alert(&w.ctx, w.crawl).await.unwrap();
    alerts::plan_alert(&w.ctx, w.crawl).await.unwrap();
    assert_eq!(w.deliveries().await.len(), 1);
}

#[tokio::test]
async fn a_quick_audit_or_an_unowned_site_never_alerts() {
    let w = World::new("pro").await;
    w.email_channel().await;
    let quick = new_crawl(w.pool(), w.site, "quick").await;
    insert_change(w.pool(), quick, w.site, ChangeKind::ErrorSpike, None).await;
    alerts::plan_alert(&w.ctx, quick).await.unwrap();
    assert!(w.deliveries().await.is_empty());

    let orphan: Uuid = sqlx::query_scalar(
        "INSERT INTO sites (domain, start_url) VALUES ('orphan.example', 'https://orphan.example/') RETURNING id",
    )
    .fetch_one(w.pool())
    .await
    .unwrap();
    let crawl = new_crawl(w.pool(), orphan, "schedule").await;
    insert_change(w.pool(), crawl, orphan, ChangeKind::ErrorSpike, None).await;
    alerts::plan_alert(&w.ctx, crawl).await.unwrap();
    assert!(w.deliveries().await.is_empty());
}

// ---- couldn't reach your site -------------------------------------------------------------

#[tokio::test]
async fn the_second_failed_crawl_alerts_every_allowed_enabled_channel() {
    let w = World::new("pro").await;
    let (server, hook) = Hook::start(200).await;
    let email = w.email_channel().await;
    let webhook = w.http_channel("webhook", server.url("/hook")).await;
    let muted = w.http_channel("webhook", server.url("/muted")).await;
    channels::set_muted(w.pool(), w.account, muted, true)
        .await
        .unwrap();
    // Turn the instant rules off: the unreachable alert doesn't depend on them.
    for kind in alert_rules::DEFAULT_INSTANT {
        for channel in [email, webhook] {
            alert_rules::set(w.pool(), w.site, kind, channel, false)
                .await
                .unwrap();
        }
    }

    let crawls = CrawlQueue::new(w.pool().clone());
    let id = crawls
        .enqueue(w.site, "example.com", CrawlTrigger::Schedule, 3, None, None)
        .await
        .unwrap();
    for _ in 0..2 {
        sqlx::query("UPDATE crawls SET queued_at = now() - interval '1 minute' WHERE id = $1")
            .bind(id)
            .execute(w.pool())
            .await
            .unwrap();
        crawls.claim("w").await.unwrap().expect("claimable");
        crawls
            .finish_failed(id, "site unreachable: connection refused to 10.0.0.5", "w")
            .await
            .unwrap();
    }
    w.drain().await;

    let deliveries = w.deliveries().await;
    assert_eq!(deliveries.len(), 2, "{deliveries:?}");
    assert!(deliveries.iter().all(|d| d["unreachable"] == true));
    let bodies = hook.bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["event"], "unreachable");
    let mails = w.mail_to("owner@example.com");
    assert_eq!(mails.len(), 1);
    assert!(
        mails[0].subject.contains("couldn't reach example.com"),
        "{}",
        mails[0].subject
    );
    assert!(
        mails[0].text.contains("didn't answer"),
        "our own wording is in the message: {}",
        mails[0].text
    );
    assert!(
        !mails[0].text.contains("connection refused") && !mails[0].text.contains("10.0.0.5"),
        "the raw reason stays out of the message: {}",
        mails[0].text
    );
}

#[tokio::test]
async fn the_unreachable_alert_on_a_free_plan_goes_to_email_only() {
    let w = World::new("free").await;
    let (server, hook) = Hook::start(200).await;
    w.email_channel().await;
    w.http_channel("slack", server.url("/slack")).await;
    w.queue
        .enqueue(
            JobKind::SendAlert,
            json!({ "crawl_id": w.crawl, "unreachable": true }),
        )
        .await
        .unwrap();
    w.drain().await;
    assert_eq!(w.mail_to("owner@example.com").len(), 1);
    assert!(hook.bodies().is_empty());
}

// ---- a channel that keeps failing ----------------------------------------------------------

#[tokio::test]
async fn a_delivery_that_fails_five_times_turns_the_channel_off_and_emails_once() {
    let w = World::new("pro").await;
    let (server, hook) = Hook::start(500).await;
    let channel = w.http_channel("slack", server.url("/slack")).await;
    w.change(ChangeKind::ErrorSpike, None).await;
    w.queue
        .enqueue(JobKind::SendAlert, json!({ "crawl_id": w.crawl }))
        .await
        .unwrap();

    for attempt in 1..=5 {
        w.drain().await;
        make_due(w.pool()).await;
        let state = channels::state(w.pool(), channel).await.unwrap().unwrap();
        assert_eq!(hook.bodies().len(), attempt, "attempt {attempt}");
        assert_eq!(
            state.enabled,
            attempt < 5,
            "switched off only by the last attempt (attempt {attempt})"
        );
        if attempt < 5 {
            assert!(notices(&w).is_empty());
        }
    }
    w.drain().await;

    let status: String = sqlx::query_scalar(
        "SELECT status::text FROM jobs WHERE kind = 'send_alert' AND payload->>'channel_id' = $1",
    )
    .bind(channel.to_string())
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(status, "failed");
    let listed = channels::list_for_account(w.pool(), &key(), w.account)
        .await
        .unwrap();
    let slack = listed.iter().find(|c| c.id == channel).unwrap();
    assert!(!slack.enabled);
    assert!(slack.last_error.as_deref().unwrap().contains("500"));
    assert!(
        listed.iter().filter(|c| c.id != channel).all(|c| c.enabled),
        "the email channel is untouched"
    );

    // (The default email channel also got the alert itself; the notice is the other mail.)
    let mails = notices(&w);
    assert_eq!(mails.len(), 1, "exactly one notice");
    assert_eq!(
        mails[0].subject,
        "We turned off your Slack alerts for example.com"
    );
    assert!(mails[0].text.contains("500"));
    assert!(
        mails[0]
            .text
            .contains("https://codoseo.test/settings/alerts")
    );

    // A later delivery job for the switched-off channel is dropped, and no second notice.
    w.queue
        .enqueue(
            JobKind::SendAlert,
            json!({ "crawl_id": w.crawl, "channel_id": channel, "unreachable": true }),
        )
        .await
        .unwrap();
    w.drain().await;
    assert_eq!(hook.bodies().len(), 5);
    assert_eq!(notices(&w).len(), 1);
}

fn notices(w: &World) -> Vec<Email> {
    w.mail_to("owner@example.com")
        .into_iter()
        .filter(|m| m.subject.starts_with("We turned off"))
        .collect()
}

#[tokio::test]
async fn a_target_that_cannot_be_decrypted_is_ours_to_fix_not_the_channels_fault() {
    let mut w = World::new("pro").await;
    let (server, hook) = Hook::start(200).await;
    let channel = w.http_channel("slack", server.url("/slack")).await;
    let change = w.change(ChangeKind::ErrorSpike, None).await;
    // The operator changed SECRET_KEY: what was stored can't be read any more.
    w.ctx.channel_key = ChannelKey::derive("a different secret");
    let delivery = alerts::Delivery {
        crawl_id: w.crawl,
        channel_id: channel,
        change_ids: vec![change],
        unreachable: false,
    };

    // Even the last attempt neither switches the channel off nor emails the user.
    for last_attempt in [false, true] {
        let outcome = alerts::deliver_alert(&w.ctx, &delivery, last_attempt).await;
        assert!(outcome.is_err(), "the job still fails: {outcome:?}");
    }
    let state = channels::state(w.pool(), channel).await.unwrap().unwrap();
    assert!(state.enabled, "still on");
    let (failures, error): (i16, Option<String>) =
        sqlx::query_as("SELECT consecutive_failures, last_error FROM alert_channels WHERE id = $1")
            .bind(channel)
            .fetch_one(w.pool())
            .await
            .unwrap();
    assert_eq!(
        (failures, error),
        (0, None),
        "not counted against the channel"
    );
    assert!(w.mail.lock().unwrap().is_empty(), "the user is not emailed");
    assert!(hook.bodies().is_empty());
}

#[tokio::test]
async fn the_default_email_channel_is_never_switched_off_by_failures() {
    let mut w = World::new("pro").await;
    // Nothing listens on port 9, so every send fails, like an SMTP outage.
    w.ctx.mailer =
        Mailer::from_config(Some("smtp://127.0.0.1:9"), "CodoSEO <hello@codoseo.test>").unwrap();
    let email = w.email_channel().await;
    w.change(ChangeKind::ErrorSpike, None).await;
    w.queue
        .enqueue(JobKind::SendAlert, json!({ "crawl_id": w.crawl }))
        .await
        .unwrap();

    for _ in 0..5 {
        w.drain().await;
        make_due(w.pool()).await;
    }
    w.drain().await;

    let status: String = sqlx::query_scalar(
        "SELECT status::text FROM jobs WHERE kind = 'send_alert' AND payload->>'channel_id' = $1",
    )
    .bind(email.to_string())
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(status, "failed", "the job itself ran out of attempts");
    let listed = channels::list_for_account(w.pool(), &key(), w.account)
        .await
        .unwrap();
    assert!(listed[0].enabled, "still on");
    assert!(listed[0].last_error.is_some());
    assert_eq!(listed[0].consecutive_failures, 5);
    let notices: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'send_email'")
        .fetch_one(w.pool())
        .await
        .unwrap();
    assert_eq!(notices, 0, "no notice either");
}

#[tokio::test]
async fn an_ai_access_change_on_a_failed_crawl_is_planned_to_the_instant_channel() {
    let w = World::new("pro").await;
    let email = w.email_channel().await;
    // The crawl failed (robots.txt returned 503): its changes are still the owner's to hear about.
    sqlx::query("UPDATE crawls SET status = 'failed', failure_reason = 'site blocked: robots' WHERE id = $1")
        .bind(w.crawl)
        .execute(w.pool())
        .await
        .unwrap();
    let blocked = w.change(ChangeKind::AiBotBlocked, None).await;
    let not_applied = w.change(ChangeKind::AiBlockNotApplied, None).await;

    let crawl = alert_rules::alert_crawl(w.pool(), w.crawl)
        .await
        .unwrap()
        .unwrap();
    assert!(!crawl.quick);
    assert_eq!(
        alert_rules::unalerted_changes(w.pool(), w.crawl)
            .await
            .unwrap()
            .len(),
        2
    );
    alerts::plan_alert(&w.ctx, w.crawl).await.unwrap();

    let deliveries = w.deliveries().await;
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0]["channel_id"], json!(email));
    assert_eq!(deliveries[0]["change_ids"], json!([blocked]));
    assert!(w.alerted(blocked).await);
    assert!(
        !w.alerted(not_applied).await,
        "ai_block_not_applied waits for the digest"
    );
    w.drain().await;
    assert_eq!(w.mail_to("owner@example.com").len(), 1);
}
