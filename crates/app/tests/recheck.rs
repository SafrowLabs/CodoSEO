//! The `ai_recheck` job end to end: real crawls open the incidents, a fake site is fixed (or
//! not), and the job fetches only what the recheck plans, as CodoSEObot, and applies it.

#[allow(dead_code)]
mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use codoseo::jobs::{JobContext, run_job};
use codoseo::recheck::RecheckSettings;
use codoseo::worker::{
    DEFAULT_MEMORY_BUDGET, ProbeSettings, WorkerSettings, worker_loop_once_with,
};
use codoseo_core::crawl::{AddressPolicy, Politeness};
use codoseo_core::plan::{Plan, PlanLimits};
use codoseo_geo::findings::FindingKind;
use codoseo_notify::{ChannelKey, GuardedHttp, Mailer};
use codoseo_store::crawl_queue::{CrawlQueue, CrawlTrigger};
use codoseo_store::geo::{self, RecheckError, RecheckOrigin, RecheckOutcome, RecheckStatus};
use codoseo_store::jobs::{ClaimedJob, JobKind, JobQueue};
use support::TestDb;
use time::{Duration as TimeDuration, OffsetDateTime};
use uuid::Uuid;

// ---- the fake site -----------------------------------------------------------------------

const ALLOW_ALL: &str = "User-agent: *\nAllow: /\n";

#[derive(Clone)]
struct Knobs {
    robots_status: u16,
    robots_body: String,
    /// Answer robots.txt after this long.
    robots_delay: Option<Duration>,
    /// Paths served as a client-side shell.
    shells: Vec<&'static str>,
    llms_status: u16,
    /// User agents (substrings) the WAF answers 403.
    blocked: Vec<&'static str>,
}

/// One request the site saw.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    ua: String,
    from: Option<String>,
}

struct Site {
    server: codoseo_testkit::TestServer,
    knobs: Arc<Mutex<Knobs>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Site {
    fn set(&self, f: impl FnOnce(&mut Knobs)) {
        f(&mut self.knobs.lock().unwrap());
    }

    fn mark(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    /// The requests since `mark`.
    fn since(&self, mark: usize) -> Vec<Seen> {
        self.seen.lock().unwrap()[mark..].to_vec()
    }
}

/// `/`, `/a`, `/b` (linked from the home page), `/private`, robots.txt and `/llms.txt`, all
/// changeable between crawls and rechecks, every request logged.
async fn site() -> Site {
    use axum::http::{HeaderMap, StatusCode, Uri, header};
    use axum::response::IntoResponse;

    let knobs = Arc::new(Mutex::new(Knobs {
        robots_status: 200,
        robots_body: ALLOW_ALL.to_owned(),
        robots_delay: None,
        shells: Vec::new(),
        llms_status: 404,
        blocked: Vec::new(),
    }));
    let seen: Arc<Mutex<Vec<Seen>>> = Default::default();
    let (state, log) = (knobs.clone(), seen.clone());
    let router =
        axum::Router::new().fallback(axum::routing::any(move |uri: Uri, headers: HeaderMap| {
            let k = state.lock().unwrap().clone();
            let ua = headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let from = headers
                .get(header::FROM)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            log.lock().unwrap().push(Seen {
                path: uri.path().to_owned(),
                ua: ua.clone(),
                from,
            });
            async move {
                if k.blocked.iter().any(|b| ua.contains(b)) {
                    return (
                        StatusCode::FORBIDDEN,
                        [(header::CONTENT_TYPE, "text/html")],
                        "<html><head><title>Forbidden</title></head><body>No.</body></html>",
                    )
                        .into_response();
                }
                match uri.path() {
                    "/robots.txt" => {
                        if let Some(d) = k.robots_delay {
                            tokio::time::sleep(d).await;
                        }
                        (
                            StatusCode::from_u16(k.robots_status).unwrap(),
                            [(header::CONTENT_TYPE, "text/plain")],
                            k.robots_body,
                        )
                            .into_response()
                    }
                    "/llms.txt" => match k.llms_status {
                        200 => (
                            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                            "# Example\n\n> A small site.\n\n## Pages\n\n- [Home](/): home\n",
                        )
                            .into_response(),
                        s => StatusCode::from_u16(s).unwrap().into_response(),
                    },
                    p @ ("/" | "/a" | "/b" | "/private") => {
                        let links = if p == "/" {
                            "<a href=\"/a\">A</a><a href=\"/b\">B</a><a href=\"/private\">P</a>"
                        } else {
                            "<a href=\"/\">Home</a>"
                        };
                        let body = if k.shells.contains(&p) {
                            format!(
                                "<div id=\"root\"></div>{links}\
                                 <noscript>You need to enable JavaScript to run this app.</noscript>\
                                 <script src=\"/static/js/main.js\"></script>"
                            )
                        } else {
                            format!(
                                "<h1>Page {p}</h1><p>{}</p>{links}",
                                "Plenty of server rendered words for a crawler to read. "
                                    .repeat(12)
                            )
                        };
                        (
                            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                            format!(
                                "<!doctype html><html lang=\"en\"><head><title>Page {p}</title>\
                                 <meta name=\"description\" content=\"A page.\"></head>\
                                 <body>{body}</body></html>"
                            ),
                        )
                            .into_response()
                    }
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            }
        }));
    Site {
        server: codoseo_testkit::TestServer::start(router).await,
        knobs,
        seen,
    }
}

// ---- the world ---------------------------------------------------------------------------

struct World {
    db: TestDb,
    site: Site,
    site_id: Uuid,
    account: Uuid,
    ctx: JobContext,
}

fn fast_probes() -> ProbeSettings {
    ProbeSettings {
        min_gap: Duration::from_millis(5),
        request_timeout: Duration::from_secs(5),
        ..ProbeSettings::default()
    }
}

fn fast_rechecks() -> RecheckSettings {
    RecheckSettings {
        probes: fast_probes(),
        politeness: Politeness {
            requests_per_sec: 1000.0,
            ..Politeness::default()
        },
        request_timeout: Duration::from_secs(5),
        ..RecheckSettings::new(AddressPolicy::AllowPrivate)
    }
}

impl World {
    /// A site owned by an account on `plan`.
    async fn new(plan: &str) -> World {
        let db = TestDb::new().await;
        let site = site().await;
        let account: Uuid = sqlx::query_scalar(
            "INSERT INTO accounts (email, email_canonical, plan) VALUES ($1, $1, $2::plan) \
             RETURNING id",
        )
        .bind(format!("{}@example.com", Uuid::new_v4().simple()))
        .bind(plan)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let site_id: Uuid = sqlx::query_scalar(
            "INSERT INTO sites (account_id, domain, start_url) VALUES ($1, 'example.test', $2) \
             RETURNING id",
        )
        .bind(account)
        .bind(site.server.url("/").as_str())
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let (mailer, _) = Mailer::capture();
        let ctx = JobContext {
            pool: db.pool.clone(),
            mailer,
            channel_key: ChannelKey::derive("test secret"),
            base_url: url::Url::parse("http://localhost:8080").unwrap(),
            http: GuardedHttp::new(AddressPolicy::AllowPrivate).unwrap(),
            rankorg_url: None,
            recheck: fast_rechecks(),
        };
        World {
            db,
            site,
            site_id,
            account,
            ctx,
        }
    }

    fn pool(&self) -> &sqlx::PgPool {
        &self.db.pool
    }

    async fn crawl(&self) -> Uuid {
        let crawl_queue = CrawlQueue::new(self.db.pool.clone());
        let job_queue = JobQueue::new(self.db.pool.clone());
        let id = crawl_queue
            .enqueue(
                self.site_id,
                "example.test",
                CrawlTrigger::Manual,
                2,
                None,
                None,
            )
            .await
            .unwrap();
        let settings = WorkerSettings {
            probes: fast_probes(),
            ..WorkerSettings::default()
        };
        assert!(
            worker_loop_once_with(
                &self.db.pool,
                &crawl_queue,
                &job_queue,
                "test-worker",
                DEFAULT_MEMORY_BUDGET,
                AddressPolicy::AllowPrivate,
                &settings,
            )
            .await
            .unwrap()
        );
        let status: String = sqlx::query_scalar("SELECT status::text FROM crawls WHERE id = $1")
            .bind(id)
            .fetch_one(&self.db.pool)
            .await
            .unwrap();
        assert_eq!(status, "done");
        id
    }

    /// The one open incident of `kind`.
    async fn incident(&self, kind: FindingKind) -> geo::Incident {
        let open: Vec<_> = geo::open_incidents(self.pool(), self.site_id)
            .await
            .unwrap()
            .into_iter()
            .filter(|i| i.kind == kind)
            .collect();
        assert_eq!(open.len(), 1, "{open:?}");
        open.into_iter().next().unwrap()
    }

    async fn reload(&self, id: i64) -> geo::Incident {
        geo::incident(self.pool(), self.site_id, id)
            .await
            .unwrap()
            .unwrap()
    }

    /// Asks for a manual recheck of `incident`.
    async fn request(&self, incident: i64) -> Result<i64, RecheckError> {
        let limits = PlanLimits {
            ai_rechecks_per_day: None,
            ..PlanLimits::for_plan(Plan::Lite)
        };
        geo::request_recheck(self.pool(), self.site_id, incident, self.account, &limits)
            .await
            .map(|q| q.id)
    }

    /// Claims the `ai_recheck` job of `recheck` (due or not) and runs it through `run_job`.
    async fn run(&self, recheck: i64) {
        let job: ClaimedJob = sqlx::query_as(
            "UPDATE jobs SET status = 'running', claimed_by = 'test', claimed_at = now() \
             WHERE kind = 'ai_recheck' AND status = 'queued' \
               AND payload->>'recheck_id' = $1::text \
             RETURNING id, kind, payload, attempt, max_attempts",
        )
        .bind(recheck)
        .fetch_one(self.pool())
        .await
        .expect("the recheck has a queued job");
        assert_eq!(job.kind, JobKind::AiRecheck);
        run_job(&self.ctx, job)
            .await
            .expect("a recheck job never fails");
    }

    async fn recheck(&self, id: i64) -> geo::Recheck {
        geo::recheck(self.pool(), self.site_id, id)
            .await
            .unwrap()
            .unwrap()
    }

    async fn rechecks(&self, incident: i64) -> Vec<geo::Recheck> {
        let ids: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM ai_rechecks WHERE incident_id = $1 ORDER BY id")
                .bind(incident)
                .fetch_all(self.pool())
                .await
                .unwrap();
        let mut out = Vec::new();
        for id in ids {
            out.push(self.recheck(id).await);
        }
        out
    }

    /// The recheck's job's `run_after`.
    async fn job_due(&self, recheck: i64) -> OffsetDateTime {
        sqlx::query_scalar(
            "SELECT run_after FROM jobs WHERE kind = 'ai_recheck' \
             AND payload->>'recheck_id' = $1::text",
        )
        .bind(recheck)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    /// Makes the follow-up `recheck` due now.
    async fn make_due(&self, recheck: i64) {
        sqlx::query("UPDATE ai_rechecks SET due_at = now() WHERE id = $1")
            .bind(recheck)
            .execute(self.pool())
            .await
            .unwrap();
        sqlx::query(
            "UPDATE jobs SET run_after = now() WHERE kind = 'ai_recheck' \
             AND payload->>'recheck_id' = $1::text",
        )
        .bind(recheck)
        .execute(self.pool())
        .await
        .unwrap();
    }

    /// Lets another manual recheck through the cooldown.
    async fn cool_down(&self) {
        sqlx::query("UPDATE ai_rechecks SET created_at = now() - interval '1 hour'")
            .execute(self.pool())
            .await
            .unwrap();
    }
}

fn assert_about(at: OffsetDateTime, hours: i64) {
    let wait = at - OffsetDateTime::now_utc();
    assert!(
        wait > TimeDuration::hours(hours) - TimeDuration::minutes(5)
            && wait <= TimeDuration::hours(hours),
        "{wait} is not about {hours} h"
    );
}

/// A non-quiet `BotsBlocked` incident for `token`: a clean first crawl, then robots.txt shuts
/// the bot out.
async fn blocked_bot(w: &World, token: &str) -> geo::Incident {
    w.crawl().await;
    w.site.set(|k| {
        k.robots_body = format!("User-agent: *\nAllow: /\n\nUser-agent: {token}\nDisallow: /\n");
    });
    w.crawl().await;
    let i = w.incident(FindingKind::BotsBlocked).await;
    assert!(!i.quiet);
    i
}

async fn alert_jobs(w: &World, crawl: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = 'send_alert' AND payload->>'crawl_id' = $1",
    )
    .bind(crawl.to_string())
    .fetch_one(w.pool())
    .await
    .unwrap()
}

async fn changes_of(w: &World, crawl: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT kind::text FROM changes WHERE crawl_id = $1 AND kind::text LIKE 'ai_%' \
         ORDER BY id",
    )
    .bind(crawl)
    .fetch_all(w.pool())
    .await
    .unwrap()
}

// ---- robots.txt --------------------------------------------------------------------------

#[tokio::test]
async fn a_fixed_robots_txt_recovers_then_the_follow_up_resolves_it() {
    let w = World::new("lite").await;
    let incident = blocked_bot(&w, "OAI-SearchBot").await;
    let last_seen: Uuid =
        sqlx::query_scalar("SELECT last_seen_crawl_id FROM ai_incidents WHERE id = $1")
            .bind(incident.id)
            .fetch_one(w.pool())
            .await
            .unwrap();
    let alerts_before = alert_jobs(&w, last_seen).await;

    // (a) The site fixes robots.txt; the owner asks for a recheck.
    w.site.set(|k| k.robots_body = ALLOW_ALL.to_owned());
    let manual = w.request(incident.id).await.unwrap();
    let mark = w.site.mark();
    w.run(manual).await;

    let r = w.recheck(manual).await;
    assert_eq!(r.status, RecheckStatus::Done);
    assert_eq!(r.outcome, Some(RecheckOutcome::Clean));
    let i = w.reload(incident.id).await;
    assert!(i.recovered_at.is_some());
    assert!(i.resolved_at.is_none());
    // (d) Only robots.txt was asked for, as CodoSEObot.
    let seen = w.site.since(mark);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].path, "/robots.txt");
    assert!(seen[0].ua.contains("CodoSEObot"));
    // What it saw is kept, small.
    let obs = r.observations.expect("observations are kept");
    assert_eq!(obs["robots"]["status"], 200);
    assert_eq!(obs["bots"][0]["token"], "OAI-SearchBot");
    assert_eq!(obs["bots"][0]["urls_blocked"], 0);
    assert!(obs.to_string().len() < 2000, "{obs}");

    // A follow-up waits for OpenAI's 24 hours, with its job.
    let all = w.rechecks(incident.id).await;
    assert_eq!(all.len(), 2);
    let followup = &all[1];
    assert_eq!(followup.origin, RecheckOrigin::Followup);
    assert_eq!(followup.status, RecheckStatus::Queued);
    assert_about(followup.due_at, 24);
    assert_about(w.job_due(followup.id).await, 24);
    assert_eq!(changes_of(&w, last_seen).await, vec!["ai_bot_blocked"]);

    // (b) A day on, the follow-up still sees it fixed: resolved by the recheck, and announced.
    w.make_due(followup.id).await;
    w.run(followup.id).await;
    let f = w.recheck(followup.id).await;
    assert_eq!(
        (f.status, f.outcome),
        (RecheckStatus::Done, Some(RecheckOutcome::Clean))
    );
    let i = w.reload(incident.id).await;
    assert!(i.resolved_at.is_some());
    assert!(i.verified_at.is_some());
    assert_eq!(i.resolved_by.as_deref(), Some("recheck"));
    assert_eq!(
        changes_of(&w, last_seen).await,
        vec!["ai_bot_blocked", "ai_issue_resolved"]
    );
    assert_eq!(alert_jobs(&w, last_seen).await, alerts_before + 1);
    assert!(
        geo::open_incidents(w.pool(), w.site_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_duckassistbot_fix_is_followed_up_after_72_hours() {
    let w = World::new("lite").await;
    let incident = blocked_bot(&w, "DuckAssistBot").await;
    w.site.set(|k| k.robots_body = ALLOW_ALL.to_owned());
    let manual = w.request(incident.id).await.unwrap();
    w.run(manual).await;
    assert_eq!(w.recheck(manual).await.outcome, Some(RecheckOutcome::Clean));
    let followup = w.rechecks(incident.id).await.pop().unwrap();
    assert_eq!(followup.origin, RecheckOrigin::Followup);
    assert_about(followup.due_at, 72);
    assert_about(w.job_due(followup.id).await, 72);
}

#[tokio::test]
async fn a_robots_txt_that_still_blocks_is_still_failing() {
    let w = World::new("lite").await;
    let incident = blocked_bot(&w, "OAI-SearchBot").await;
    let manual = w.request(incident.id).await.unwrap();
    w.run(manual).await;
    let r = w.recheck(manual).await;
    assert_eq!(r.outcome, Some(RecheckOutcome::StillFailing));
    let obs = r.observations.unwrap();
    assert_eq!(obs["bots"][0]["home_allowed"], false);
    assert_eq!(obs["bots"][0]["home_rule"]["pattern"], "/");
    let i = w.reload(incident.id).await;
    assert_eq!(i.recovered_at, None);
    assert!(i.resolved_at.is_none());
    assert!(i.last_seen_at > incident.last_seen_at);
    assert_eq!(w.rechecks(incident.id).await.len(), 1, "no follow-up");
}

#[tokio::test]
async fn a_recheck_reads_robots_txt_where_the_crawl_did_after_a_redirect() {
    use axum::http::{StatusCode, Uri, header};
    use axum::response::IntoResponse;

    let w = World::new("lite").await;
    // The site's address is an "apex" with a robots.txt of its own that redirects every page to
    // the "www" host the crawl ends up on.
    let www = w.site.server.base();
    let apex_seen: Arc<Mutex<Vec<String>>> = Default::default();
    let log = apex_seen.clone();
    let apex = codoseo_testkit::TestServer::start(axum::Router::new().fallback(
        axum::routing::any(move |uri: Uri| {
            log.lock().unwrap().push(uri.path().to_owned());
            let www = www.clone();
            async move {
                if uri.path() == "/robots.txt" {
                    ([(header::CONTENT_TYPE, "text/plain")], ALLOW_ALL).into_response()
                } else {
                    (
                        StatusCode::MOVED_PERMANENTLY,
                        [(header::LOCATION, format!("{www}{}", uri.path()))],
                    )
                        .into_response()
                }
            }
        }),
    ))
    .await;
    sqlx::query("UPDATE sites SET start_url = $2 WHERE id = $1")
        .bind(w.site_id)
        .bind(apex.localhost_url("/").as_str())
        .execute(w.pool())
        .await
        .unwrap();
    let incident = blocked_bot(&w, "OAI-SearchBot").await;

    // Nothing was fixed on www: the recheck must read www's robots.txt, not the apex's.
    let mark = w.site.mark();
    let apex_mark = apex_seen.lock().unwrap().len();
    let manual = w.request(incident.id).await.unwrap();
    w.run(manual).await;
    let r = w.recheck(manual).await;
    assert_eq!(r.outcome, Some(RecheckOutcome::StillFailing));
    let seen = w.site.since(mark);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].path, "/robots.txt");
    assert!(
        apex_seen.lock().unwrap()[apex_mark..].is_empty(),
        "the apex was not asked"
    );
}

#[tokio::test]
async fn an_inconclusive_follow_up_tries_again_six_hours_on_at_most_three_times() {
    let w = World::new("lite").await;
    let incident = blocked_bot(&w, "OAI-SearchBot").await;
    w.site.set(|k| k.robots_body = ALLOW_ALL.to_owned());
    let manual = w.request(incident.id).await.unwrap();
    w.run(manual).await;

    // robots.txt now fails: it can say nothing about the bots.
    w.site.set(|k| k.robots_status = 503);
    for round in 1..=geo::MAX_INCONCLUSIVE_FOLLOWUPS {
        let followup = w.rechecks(incident.id).await.pop().unwrap();
        assert_eq!(followup.origin, RecheckOrigin::Followup);
        assert_eq!(followup.status, RecheckStatus::Queued, "round {round}");
        w.make_due(followup.id).await;
        w.run(followup.id).await;
        assert_eq!(
            w.recheck(followup.id).await.outcome,
            Some(RecheckOutcome::Inconclusive)
        );
        let next = w.rechecks(incident.id).await.pop().unwrap();
        if round < geo::MAX_INCONCLUSIVE_FOLLOWUPS {
            assert_ne!(next.id, followup.id);
            assert_about(next.due_at, 6);
            assert_about(w.job_due(next.id).await, 6);
        } else {
            assert_eq!(next.id, followup.id, "left to the next crawl");
        }
    }
    let i = w.reload(incident.id).await;
    assert!(i.recovered_at.is_some());
    assert!(i.resolved_at.is_none());
}

// ---- runs that do not get to judge -------------------------------------------------------

#[tokio::test]
async fn a_crawl_under_way_supersedes_the_recheck() {
    let w = World::new("lite").await;
    let incident = blocked_bot(&w, "OAI-SearchBot").await;
    let manual = w.request(incident.id).await.unwrap();
    CrawlQueue::new(w.db.pool.clone())
        .enqueue(
            w.site_id,
            "example.test",
            CrawlTrigger::Manual,
            2,
            None,
            None,
        )
        .await
        .unwrap();
    let mark = w.site.mark();
    w.run(manual).await;
    let r = w.recheck(manual).await;
    assert_eq!(r.status, RecheckStatus::Superseded);
    assert_eq!(r.outcome, None);
    assert!(r.finished_at.is_some());
    assert!(w.site.since(mark).is_empty(), "nothing fetched");
    // Run again: nothing happens.
    sqlx::query("UPDATE jobs SET status = 'queued' WHERE kind = 'ai_recheck'")
        .execute(w.pool())
        .await
        .unwrap();
    w.run(manual).await;
    assert_eq!(w.recheck(manual).await.status, RecheckStatus::Superseded);
}

async fn failure_of(w: &World, recheck: i64) -> Option<String> {
    sqlx::query_scalar("SELECT failure FROM ai_rechecks WHERE id = $1")
        .bind(recheck)
        .fetch_one(w.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_site_that_does_not_answer_fails_the_recheck_and_frees_the_incident() {
    let w = World::new("lite").await;
    let incident = blocked_bot(&w, "OAI-SearchBot").await;

    // The site no longer answers in time: robots.txt hangs past the request limit.
    w.site
        .set(|k| k.robots_delay = Some(Duration::from_secs(3)));
    let manual = w.request(incident.id).await.unwrap();
    let mut slow = w.ctx.clone();
    slow.recheck.request_timeout = Duration::from_millis(300);
    let job: ClaimedJob = sqlx::query_as(
        "UPDATE jobs SET status = 'running', claimed_at = now() \
         WHERE kind = 'ai_recheck' AND payload->>'recheck_id' = $1::text \
         RETURNING id, kind, payload, attempt, max_attempts",
    )
    .bind(manual)
    .fetch_one(w.pool())
    .await
    .unwrap();
    run_job(&slow, job).await.unwrap();
    let r = w.recheck(manual).await;
    assert_eq!(r.status, RecheckStatus::Failed);
    assert!(r.finished_at.is_some());
    assert_eq!(failure_of(&w, manual).await.as_deref(), Some("site"));
    let job: String = sqlx::query_scalar(
        "SELECT status::text FROM jobs WHERE kind = 'ai_recheck' \
         AND payload->>'recheck_id' = $1::text",
    )
    .bind(manual)
    .fetch_one(w.pool())
    .await
    .unwrap();
    assert_eq!(
        job, "running",
        "run_job returned Ok; the loop marks it done"
    );
    let i = w.reload(incident.id).await;
    assert_eq!(i.recovered_at, None);
    assert!(i.resolved_at.is_none());

    // The time limit: robots.txt answers too late.
    w.cool_down().await;
    let second = w.request(incident.id).await.expect("not busy");
    let mut ctx = w.ctx.clone();
    ctx.recheck.timeout = Duration::from_millis(500);
    let job: ClaimedJob = sqlx::query_as(
        "UPDATE jobs SET status = 'running', claimed_at = now() \
         WHERE kind = 'ai_recheck' AND payload->>'recheck_id' = $1::text \
         RETURNING id, kind, payload, attempt, max_attempts",
    )
    .bind(second)
    .fetch_one(w.pool())
    .await
    .unwrap();
    let started = std::time::Instant::now();
    run_job(&ctx, job).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(w.recheck(second).await.status, RecheckStatus::Failed);
    // robots.txt did not answer before the fetches' deadline: the site's silence again.
    assert_eq!(failure_of(&w, second).await.as_deref(), Some("site"));

    // Not busy afterwards.
    w.cool_down().await;
    w.request(incident.id).await.expect("not busy");
}

// ---- pages -------------------------------------------------------------------------------

#[tokio::test]
async fn a_page_server_rendered_again_is_clean_and_only_it_is_fetched() {
    let w = World::new("lite").await;
    w.crawl().await;
    w.site.set(|k| k.shells = vec!["/"]);
    w.crawl().await;
    let incident = w.incident(FindingKind::ContentMissing).await;

    w.site.set(|k| k.shells = Vec::new());
    let manual = w.request(incident.id).await.unwrap();
    let mark = w.site.mark();
    w.run(manual).await;
    let r = w.recheck(manual).await;
    assert_eq!(r.outcome, Some(RecheckOutcome::Clean));
    assert!(w.reload(incident.id).await.recovered_at.is_some());
    let paths: Vec<String> = w.site.since(mark).into_iter().map(|s| s.path).collect();
    assert_eq!(paths, vec!["/robots.txt", "/"]);
    let obs = r.observations.unwrap();
    assert_eq!(obs["pages"][0]["status"], 200);
    assert_eq!(obs["pages"][0]["html_ok"], true);
    assert!(
        !obs.to_string().contains("server rendered words"),
        "no body is kept"
    );
}

#[tokio::test]
async fn a_page_robots_txt_keeps_codoseobot_off_is_not_fetched() {
    let w = World::new("lite").await;
    w.crawl().await;
    w.site.set(|k| k.shells = vec!["/", "/a"]);
    w.crawl().await;
    let incident = w.incident(FindingKind::ContentMissing).await;
    let ev: codoseo_geo::findings::Evidence =
        serde_json::from_value(incident.evidence.clone()).unwrap();
    assert!(
        ev.content.iter().any(|c| c.url.ends_with("/a")),
        "{:?}",
        ev.content
    );

    w.site.set(|k| {
        k.shells = Vec::new();
        k.robots_body = "User-agent: CodoSEObot\nDisallow: /a\n".to_owned();
    });
    let manual = w.request(incident.id).await.unwrap();
    let mark = w.site.mark();
    w.run(manual).await;
    let paths: Vec<String> = w.site.since(mark).into_iter().map(|s| s.path).collect();
    assert_eq!(paths, vec!["/robots.txt", "/"]);
    let r = w.recheck(manual).await;
    // /a was not looked at, so the incident can't be cleared.
    assert_eq!(r.outcome, Some(RecheckOutcome::Inconclusive));
    let obs = r.observations.unwrap();
    assert_eq!(obs["robots_skipped"].as_array().unwrap().len(), 1);
    assert!(w.reload(incident.id).await.recovered_at.is_none());
}

// ---- llms.txt ----------------------------------------------------------------------------

#[tokio::test]
async fn an_llms_txt_serving_again_is_clean() {
    let w = World::new("lite").await;
    w.site.set(|k| k.llms_status = 200);
    w.crawl().await;
    w.site.set(|k| k.llms_status = 503);
    w.crawl().await;
    let incident = w.incident(FindingKind::LlmsTxtUnavailable).await;

    w.site.set(|k| k.llms_status = 200);
    let manual = w.request(incident.id).await.unwrap();
    let mark = w.site.mark();
    w.run(manual).await;
    let r = w.recheck(manual).await;
    assert_eq!(r.outcome, Some(RecheckOutcome::Clean));
    let obs = r.observations.unwrap();
    assert_eq!(obs["llms"]["status"], 200);
    // No link checks: the file lists the home page, which is not requested.
    let paths: Vec<String> = w.site.since(mark).into_iter().map(|s| s.path).collect();
    assert_eq!(paths, vec!["/robots.txt", "/llms.txt"]);
}

// ---- probes ------------------------------------------------------------------------------

/// A quiet `FetchBlocked` incident: the WAF turns OAI-SearchBot away from the first crawl.
async fn waf_blocked(w: &World) -> geo::Incident {
    w.site.set(|k| k.blocked = vec!["OAI-SearchBot"]);
    w.crawl().await;
    w.incident(FindingKind::FetchBlocked).await
}

#[tokio::test]
async fn a_lifted_block_is_probed_again_on_the_planned_page_with_the_incidents_bot_only() {
    let w = World::new("free").await;
    let incident = waf_blocked(&w).await;
    w.site.set(|k| k.blocked = Vec::new());
    let manual = w.request(incident.id).await.unwrap();
    let mark = w.site.mark();
    w.run(manual).await;
    let r = w.recheck(manual).await;
    assert_eq!(
        r.outcome,
        Some(RecheckOutcome::Clean),
        "{:?}",
        r.observations
    );
    let seen = w.site.since(mark);
    // robots.txt, then the free plan's one probed page: the baseline and OAI-SearchBot.
    let paths: Vec<&str> = seen.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(paths, vec!["/robots.txt", "/", "/"], "{seen:?}");
    assert!(seen[1].ua.contains("CodoSEObot"));
    assert!(seen[2].ua.contains("OAI-SearchBot"));
    assert!(
        seen[1..].iter().all(|s| s.from.is_some()),
        "From on every probe"
    );
    let obs = r.observations.unwrap();
    assert_eq!(obs["probes"]["results"][0]["token"], "OAI-SearchBot");
}

#[tokio::test]
async fn a_bot_robots_txt_now_disallows_is_not_sent() {
    let w = World::new("free").await;
    let incident = waf_blocked(&w).await;
    w.site.set(|k| {
        k.robots_body =
            "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /\n".into();
    });
    let manual = w.request(incident.id).await.unwrap();
    let mark = w.site.mark();
    w.run(manual).await;
    assert_eq!(
        w.site
            .since(mark)
            .iter()
            .filter(|s| s.ua.contains("OAI-SearchBot"))
            .count(),
        0
    );
}

#[tokio::test]
async fn probes_switched_off_for_the_site_make_the_recheck_inconclusive() {
    let w = World::new("free").await;
    let incident = waf_blocked(&w).await;
    geo::set_ai_probes(w.pool(), w.site_id, false)
        .await
        .unwrap();
    w.site.set(|k| k.blocked = Vec::new());
    let manual = w.request(incident.id).await.unwrap();
    let mark = w.site.mark();
    w.run(manual).await;
    let r = w.recheck(manual).await;
    assert_eq!(r.outcome, Some(RecheckOutcome::Inconclusive));
    let obs = r.observations.unwrap();
    assert!(
        obs["notes"][0]
            .as_str()
            .unwrap()
            .contains("turned off for this site"),
        "{obs}"
    );
    assert!(
        w.site
            .since(mark)
            .iter()
            .all(|s| s.ua.contains("CodoSEObot")),
        "no bot user agent"
    );
}
