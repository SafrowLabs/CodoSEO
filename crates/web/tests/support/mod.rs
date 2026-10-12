//! Shared harness for the web crate's integration tests: a throwaway Postgres database per
//! test, the app router driven in-process with `oneshot`, signed-in clients, and fixture crawls
//! written through the real `finalize` so screens read exactly what the worker writes.
//!
//! `TEST_DATABASE_URL` (falling back to a local default) must point at a server where the test
//! role may `CREATE DATABASE`.

#![allow(dead_code)]

pub mod mcp;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use codoseo_core::change::Change;
use codoseo_core::check::IssueBits;
use codoseo_core::crawl::{RobotsFile, SitemapSummary};
use codoseo_core::output::{CrawlOutput, Edge, LinkGraph, LlmsTxtFetch, SiteSignals, StopReason};
use codoseo_core::page::{Indexability, JsonLdStatus, OgTags, PageFields, PageRecord};
use codoseo_core::plan::Plan;
use codoseo_geo::probe::ProbeReport;
use codoseo_geo::report::AccessReport;
use codoseo_store::accounts::{Account, SignIn, SignupPolicy};
use codoseo_store::sites::Site;
use codoseo_web::auth::mailer::{Email, Mailer};
use codoseo_web::{AppState, Config};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, Executor, PgConnection, PgPool};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

fn admin_url() -> String {
    std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://127.0.0.1/postgres".to_string())
}

/// Owns a database created for one test and drops it when the guard is dropped.
pub struct TestDb {
    name: String,
    pub url: String,
    pub pool: PgPool,
}

impl TestDb {
    pub async fn new() -> TestDb {
        let name = format!("codoseo_test_{}", Uuid::new_v4().simple());
        let mut admin = PgConnection::connect(&admin_url())
            .await
            .expect("connect to admin database");
        admin
            .execute(format!(r#"CREATE DATABASE "{name}""#).as_str())
            .await
            .expect("create test database");
        let mut db_url = Url::parse(&admin_url()).expect("valid admin url");
        db_url.set_path(&format!("/{name}"));
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(db_url.as_str())
            .await
            .expect("connect to test database");
        codoseo_store::pool::migrate(&pool)
            .await
            .expect("run migrations");
        TestDb {
            name,
            url: db_url.to_string(),
            pool,
        }
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let name = self.name.clone();
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("runtime for cleanup");
            rt.block_on(async move {
                if let Ok(mut admin) = PgConnection::connect(&admin_url()).await {
                    let _ = admin
                        .execute(
                            format!(r#"DROP DATABASE IF EXISTS "{name}" WITH (FORCE)"#).as_str(),
                        )
                        .await;
                }
            });
        })
        .join();
    }
}

/// The app wired to a fresh database, with a capturing mailer.
/// What a test crawl read besides its pages, for [`TestApp::finalize_with`]: the robots.txt
/// (which makes the worker build the AI access report), synthetic fetch probes over that
/// report, and the `/llms.txt` fetch with whether the site adopted one before.
#[derive(Default)]
struct CrawlExtras<'a> {
    robots: Option<RobotsFile>,
    probes: Option<&'a dyn Fn(&AccessReport) -> ProbeReport>,
    llms: Option<(LlmsTxtFetch, bool)>,
}

pub struct TestApp {
    pub db: TestDb,
    pub state: AppState,
    pub router: Router,
    pub mail: Arc<Mutex<Vec<Email>>>,
}

pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl TestResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// The `Location` of a redirect.
    pub fn location(&self) -> Option<&str> {
        self.header("location")
    }

    /// `name=value` of a `Set-Cookie` header, ready to send back as `Cookie`.
    pub fn cookie(&self, name: &str) -> Option<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or("").to_owned())
            .find(|kv| kv.starts_with(&format!("{name}=")))
    }
}

impl TestApp {
    pub async fn new() -> TestApp {
        TestApp::with_config(Config::for_tests()).await
    }

    pub async fn with_config(config: Config) -> TestApp {
        TestApp::build(config, None, None).await
    }

    /// Like [`with_config`](Self::with_config), with `mailer` instead of the capturing one
    /// (`app.mail` then stays empty).
    pub async fn with_mailer(config: Config, mailer: Mailer) -> TestApp {
        TestApp::build(config, None, Some(mailer)).await
    }

    /// Like [`with_config`](Self::with_config), with the client that delivers to Slack, Discord
    /// and webhooks replaced (tests resolve names without DNS).
    pub async fn with_notify_http(config: Config, http: codoseo_notify::GuardedHttp) -> TestApp {
        TestApp::build(config, Some(http), None).await
    }

    async fn build(
        config: Config,
        http: Option<codoseo_notify::GuardedHttp>,
        mailer: Option<Mailer>,
    ) -> TestApp {
        let db = TestDb::new().await;
        let (captured, mail) = Mailer::capture();
        let mailer = mailer.unwrap_or(captured);
        let mut state = AppState::new(db.pool.clone(), config, mailer);
        if let Some(http) = http {
            state.notify_http = http;
        }
        let router = codoseo_web::app(state.clone());
        TestApp {
            db,
            state,
            router,
            mail,
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.db.pool
    }

    pub fn origin(&self) -> String {
        self.state.config.origin()
    }

    /// Sends a request through the router. POSTs carry this app's `Origin` unless the caller
    /// set one.
    pub async fn send(&self, mut req: Request<Body>) -> TestResponse {
        if req.method() != Method::GET && !req.headers().contains_key(header::ORIGIN) {
            req.headers_mut()
                .insert(header::ORIGIN, self.origin().parse().unwrap());
        }
        let res = self.router.clone().oneshot(req).await.expect("infallible");
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = to_bytes(res.into_body(), usize::MAX).await.expect("body");
        TestResponse {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }

    pub async fn get(&self, path: &str, cookie: Option<&str>) -> TestResponse {
        self.send(build(Method::GET, path, cookie, None, false))
            .await
    }

    /// A GET as htmx sends it (`HX-Request: true`, not boosted): wants a fragment.
    pub async fn get_hx(&self, path: &str, cookie: Option<&str>) -> TestResponse {
        self.send(build(Method::GET, path, cookie, None, true))
            .await
    }

    /// A form POST with extra request headers (`CF-Connecting-IP`, ...).
    pub async fn post_with_headers(
        &self,
        path: &str,
        form: &str,
        cookie: Option<&str>,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        let mut req = build(Method::POST, path, cookie, Some(form), false);
        for (name, value) in headers {
            req.headers_mut().insert(
                header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        self.send(req).await
    }

    /// A form POST (`application/x-www-form-urlencoded`).
    pub async fn post(&self, path: &str, form: &str, cookie: Option<&str>) -> TestResponse {
        self.send(build(Method::POST, path, cookie, Some(form), false))
            .await
    }

    pub async fn post_hx(&self, path: &str, form: &str, cookie: Option<&str>) -> TestResponse {
        self.send(build(Method::POST, path, cookie, Some(form), true))
            .await
    }

    /// Creates (or finds) an account and a live session for it. Returns the account and the
    /// `Cookie` header value to send.
    pub async fn login(&self, email: &str) -> (Account, String) {
        self.login_with_plan(email, None).await
    }

    /// Like [`login`](Self::login), but forces the account's plan (cloud plan-limit tests).
    pub async fn login_with_plan(&self, email: &str, plan: Option<Plan>) -> (Account, String) {
        let canonical = codoseo_web::auth::email::canonical(email);
        let account = codoseo_store::accounts::sign_in(
            self.pool(),
            &SignIn {
                email,
                canonical: &canonical,
                github_id: None,
            },
            SignupPolicy {
                self_hosted: self.state.config.mode == codoseo_web::Mode::SelfHost,
            },
        )
        .await
        .expect("sign in")
        .account()
        .expect("signups open in tests");
        let account = match plan {
            Some(p) => {
                set_plan(self.pool(), account.id, p).await;
                Account { plan: p, ..account }
            }
            None => account,
        };
        let token = codoseo_web::auth::session::random_token();
        codoseo_store::auth::create_session(
            self.pool(),
            account.id,
            &codoseo_web::auth::session::hash(&token),
            time::Duration::days(1),
        )
        .await
        .expect("session");
        (account, format!("codoseo_session={token}"))
    }

    pub async fn site(&self, account: &Account, domain: &str) -> Site {
        codoseo_store::sites::create(
            self.pool(),
            account.id,
            domain,
            &format!("https://{domain}/"),
            Some("weekly"),
        )
        .await
        .expect("create site")
    }

    /// Writes a finished crawl of `pages` for `site` exactly as the worker would: the checks
    /// run over the output (setting issue bits, inlinks and the summary), then `finalize`
    /// writes it all in one transaction. The first page links to every other page, so every
    /// page has at least one inlink. Returns the crawl ID.
    pub async fn finished_crawl(
        &self,
        site: &Site,
        pages: Vec<PageRecord>,
        changes: Vec<Change>,
    ) -> Uuid {
        let crawl_id: Uuid = sqlx::query_scalar(
            "INSERT INTO crawls (site_id, domain, trigger, priority, status, worker_id, started_at, heartbeat_at) \
             VALUES ($1, $2, 'manual', 2, 'running', 'test-worker', now() - interval '95 seconds', now()) \
             RETURNING id",
        )
        .bind(site.id)
        .bind(&site.domain)
        .fetch_one(self.pool())
        .await
        .expect("insert crawl");
        self.finalize_crawl(crawl_id, pages, changes, StopReason::Completed)
            .await;
        crawl_id
    }

    /// Like [`finished_crawl`](Self::finished_crawl), with the robots.txt the crawl read
    /// (`status`, `body`) and the AI access report the worker builds from it under the site's
    /// intent and starred pages, so `finalize` opens and resolves incidents as in production.
    pub async fn finished_crawl_with_robots(
        &self,
        site: &Site,
        pages: Vec<PageRecord>,
        robots: (u16, &str),
        changes: Vec<Change>,
    ) -> Uuid {
        let crawl_id: Uuid = sqlx::query_scalar(
            "INSERT INTO crawls (site_id, domain, trigger, priority, status, worker_id, started_at, heartbeat_at) \
             VALUES ($1, $2, 'manual', 2, 'running', 'test-worker', now() - interval '95 seconds', now()) \
             RETURNING id",
        )
        .bind(site.id)
        .bind(&site.domain)
        .fetch_one(self.pool())
        .await
        .expect("insert crawl");
        let robots = RobotsFile {
            status: robots.0,
            body: robots.1.to_owned(),
            hash: xxhash_rust::xxh3::xxh3_64(robots.1.as_bytes()),
        };
        let extras = CrawlExtras {
            robots: Some(robots),
            ..CrawlExtras::default()
        };
        self.finalize_with(crawl_id, pages, changes, StopReason::Completed, extras)
            .await;
        crawl_id
    }

    /// Like [`finished_crawl_with_robots`](Self::finished_crawl_with_robots), with the
    /// `/llms.txt` the crawl fetched; `adopted` marks a site that served one before (as the
    /// worker carries from the previous report) so a failing file opens an incident.
    pub async fn finished_crawl_with_llms(
        &self,
        site: &Site,
        pages: Vec<PageRecord>,
        robots: (u16, &str),
        llms: LlmsTxtFetch,
        adopted: bool,
    ) -> Uuid {
        let crawl_id: Uuid = sqlx::query_scalar(
            "INSERT INTO crawls (site_id, domain, trigger, priority, status, worker_id, started_at, heartbeat_at) \
             VALUES ($1, $2, 'manual', 2, 'running', 'test-worker', now() - interval '95 seconds', now()) \
             RETURNING id",
        )
        .bind(site.id)
        .bind(&site.domain)
        .fetch_one(self.pool())
        .await
        .expect("insert crawl");
        let robots = RobotsFile {
            status: robots.0,
            body: robots.1.to_owned(),
            hash: xxhash_rust::xxh3::xxh3_64(robots.1.as_bytes()),
        };
        let extras = CrawlExtras {
            robots: Some(robots),
            llms: Some((llms, adopted)),
            ..CrawlExtras::default()
        };
        self.finalize_with(crawl_id, pages, Vec::new(), StopReason::Completed, extras)
            .await;
        crawl_id
    }

    /// Like [`finished_crawl_with_robots`](Self::finished_crawl_with_robots), with synthetic
    /// fetch probes: `probes` gets the report the crawl built and returns what the probes found,
    /// stored in `report.probes` before `finalize` as the worker does.
    pub async fn finished_crawl_with_probes(
        &self,
        site: &Site,
        pages: Vec<PageRecord>,
        robots: (u16, &str),
        probes: &dyn Fn(&AccessReport) -> ProbeReport,
    ) -> Uuid {
        let crawl_id: Uuid = sqlx::query_scalar(
            "INSERT INTO crawls (site_id, domain, trigger, priority, status, worker_id, started_at, heartbeat_at) \
             VALUES ($1, $2, 'manual', 2, 'running', 'test-worker', now() - interval '95 seconds', now()) \
             RETURNING id",
        )
        .bind(site.id)
        .bind(&site.domain)
        .fetch_one(self.pool())
        .await
        .expect("insert crawl");
        let robots = RobotsFile {
            status: robots.0,
            body: robots.1.to_owned(),
            hash: xxhash_rust::xxh3::xxh3_64(robots.1.as_bytes()),
        };
        let extras = CrawlExtras {
            robots: Some(robots),
            probes: Some(probes),
            ..CrawlExtras::default()
        };
        self.finalize_with(crawl_id, pages, Vec::new(), StopReason::Completed, extras)
            .await;
        crawl_id
    }

    /// Finishes a crawl that already exists (queued or running) as the worker would: claims it
    /// for `test-worker`, runs the checks over `pages` and `finalize`s. For no-signup audits
    /// and first crawls the test queued itself.
    pub async fn finalize_crawl(
        &self,
        crawl_id: Uuid,
        pages: Vec<PageRecord>,
        changes: Vec<Change>,
        stop: StopReason,
    ) {
        self.finalize_with(crawl_id, pages, changes, stop, CrawlExtras::default())
            .await;
    }

    /// [`finalize_crawl`](Self::finalize_crawl), plus the AI access report when the crawl read
    /// a robots.txt.
    async fn finalize_with(
        &self,
        crawl_id: Uuid,
        pages: Vec<PageRecord>,
        changes: Vec<Change>,
        stop: StopReason,
        extras: CrawlExtras<'_>,
    ) {
        let CrawlExtras {
            robots,
            probes,
            llms,
        } = extras;
        let (site_id, start_url): (Uuid, String) = sqlx::query_as(
            "SELECT s.id, s.start_url FROM crawls c JOIN sites s ON s.id = c.site_id WHERE c.id = $1",
        )
        .bind(crawl_id)
        .fetch_one(self.pool())
        .await
        .expect("crawl and site");
        sqlx::query(
            "UPDATE crawls SET status = 'running', worker_id = 'test-worker', \
             started_at = now() - interval '95 seconds', heartbeat_at = now() WHERE id = $1",
        )
        .bind(crawl_id)
        .execute(self.pool())
        .await
        .expect("claim crawl");

        let origin = Url::parse(&start_url).expect("start url");
        let edges = (1..pages.len() as u32)
            .map(|to| Edge {
                from: 0,
                to,
                anchor: 0,
                nofollow: false,
            })
            .collect();
        let mut out = CrawlOutput {
            origin,
            pages,
            links: LinkGraph {
                edges,
                anchors: vec!["Main navigation".to_owned()],
            },
            robots,
            sitemap: SitemapSummary::default(),
            stop,
            duration_ms: 95_000,
            signals: SiteSignals {
                llms_txt: llms.as_ref().map(|(f, _)| f.clone()),
                ..SiteSignals::default()
            },
        };
        let report = codoseo_checks::run_checks(&mut out);
        let geo = match &out.robots {
            Some(_) => {
                let starred = codoseo_store::sites::starred_key_pages(self.pool(), site_id)
                    .await
                    .expect("starred pages");
                let important =
                    codoseo_geo::report::important_urls(&out.pages, &out.origin, &starred);
                let mut report = codoseo_geo::report::build_report(&out, &important);
                if let Some(probes) = probes {
                    report.probes = Some(probes(&report));
                }
                if let (Some(l), Some((_, true))) = (report.llms_txt.as_mut(), llms.as_ref()) {
                    l.adopted = true;
                }
                Some(codoseo_store::geo::GeoInput::new(report))
            }
            None => None,
        };
        codoseo_store::finalize::finalize(
            self.pool(),
            crawl_id,
            site_id,
            "test-worker",
            &out,
            &report,
            &changes,
            geo.as_ref(),
        )
        .await
        .expect("finalize");
    }
}

/// A cloud-mode config: https base URL, plan limits and signup rules of codoseo.com.
pub fn cloud_config() -> Config {
    cloud_config_with(&[])
}

/// A cloud config with extra environment values (`TURNSTILE_SECRET`, `ADMIN_EMAILS`, ...).
pub fn cloud_config_with(extra: &[(&str, &str)]) -> Config {
    let extra: Vec<(String, String)> = extra
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Config::from_lookup(|k| match k {
        "CODOSEO_MODE" => Some("cloud".into()),
        "BASE_URL" => Some("https://codoseo.com".into()),
        "SECRET_KEY" => Some("test-secret".into()),
        "SMTP_URL" => Some("smtp://127.0.0.1:2525".into()),
        other => extra
            .iter()
            .find(|(n, _)| n == other)
            .map(|(_, v)| v.clone()),
    })
    .expect("cloud config is valid")
}

pub async fn set_plan(pool: &PgPool, account_id: Uuid, plan: Plan) {
    let slug = serde_json::to_value(plan).unwrap();
    sqlx::query("UPDATE accounts SET plan = $2::plan WHERE id = $1")
        .bind(account_id)
        .bind(slug.as_str().unwrap())
        .execute(pool)
        .await
        .expect("set plan");
}

fn build(
    method: Method,
    path: &str,
    cookie: Option<&str>,
    form: Option<&str>,
    htmx: bool,
) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    if htmx {
        b = b.header("hx-request", "true");
    }
    match form {
        Some(f) => b
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(f.to_owned())),
        None => b.body(Body::empty()),
    }
    .expect("valid request")
}

/// A healthy, indexable HTML page at `path` on `https://{domain}`. Adjust fields with struct
/// update syntax for the case under test.
pub fn page(domain: &str, path: &str) -> PageRecord {
    let url = Url::parse(&format!("https://{domain}{path}")).expect("valid url");
    let title = format!("{path} | A page title that is long enough");
    let mut p = PageRecord {
        url_hash: codoseo_core::url::url_hash(&url),
        url,
        status: 200,
        redirect_chain: Vec::new(),
        response_ms: 120,
        size_bytes: 24_000,
        content_type: Some("text/html; charset=utf-8".to_owned()),
        depth: Some(1),
        in_sitemap: true,
        indexability: Indexability::Indexable,
        fields: PageFields {
            title: Some(title),
            title_count: 1,
            meta_description: Some(
                "A meta description that is comfortably long enough to pass the length check \
                 without being too long for the results page."
                    .to_owned(),
            ),
            meta_robots: None,
            x_robots_tag: None,
            canonical: None,
            hreflang: Vec::new(),
            h1: vec![format!("Heading for {path}")],
            h2: Vec::new(),
            word_count: 640,
            content_hash: 0,
            images_missing_alt: 0,
            og: OgTags::default(),
            jsonld: JsonLdStatus::default(),
            mixed_content: 0,
            ai: Default::default(),
            raw: Default::default(),
        },
        inlinks: 0,
        outlinks_internal: 0,
        outlinks_external: 0,
        issues: IssueBits::default(),
        key_hash: 0,
        redirect_target: None,
        outlinks_nofollow: 0,
        error: None,
    };
    p.fields.content_hash = p.url_hash ^ 0x5555;
    p.key_hash = p.compute_key_hash();
    p
}

/// What the worker would store after a crawl with synthetic fetch probes on two pages: OAI-SearchBot
/// challenged by Cloudflare on both, PerplexityBot refused with a 403 on the second, ChatGPT-User
/// kept out of the first by robots.txt and the same as CodoSEObot on the second.
pub fn probe_report() -> serde_json::Value {
    let baseline = serde_json::json!({"status": 200, "html": true, "bytes": 5000, "title": "Home"});
    let same = serde_json::json!({"status": 200, "html": true, "bytes": 5000, "title": "Home"});
    let challenge = serde_json::json!({
        "status": 403, "html": true, "bytes": 900, "title": "Just a moment...",
        "challenge": {"vendor": "cloudflare", "signal": "cf-mitigated: challenge"}
    });
    let cf = serde_json::json!({"kind": "challenged", "vendor": "cloudflare"});
    serde_json::json!({
        "version": 1,
        "synthetic": true,
        "from": "hello@codoseo.com",
        "bots": ["OAI-SearchBot", "ChatGPT-User", "PerplexityBot"],
        "skipped_bots": [
            {"token": "Claude-SearchBot", "reason": "no_user_agent"},
            {"token": "Googlebot", "reason": "verified_search_engine"}
        ],
        "urls": [
            {"url": 0, "baseline": baseline, "results": [
                {"token": "OAI-SearchBot", "fetch": challenge, "verdict": cf},
                {"token": "ChatGPT-User", "verdict": {"kind": "skipped_robots"}},
                {"token": "PerplexityBot", "fetch": same, "verdict": {"kind": "same"}}
            ]},
            {"url": 1, "baseline": baseline, "results": [
                {"token": "OAI-SearchBot", "fetch": challenge, "verdict": cf},
                {"token": "ChatGPT-User", "fetch": same, "verdict": {"kind": "same"}},
                {"token": "PerplexityBot", "fetch": {"status": 403, "bytes": 120},
                 "verdict": {"kind": "blocked", "status": 403}}
            ]}
        ],
        "requests": 8
    })
}

/// Puts [`probe_report`] into the site's stored reports and reconciles, so the `fetch_blocked`
/// incidents open (quietly, as on the first crawl with probes).
pub async fn store_probes(app: &TestApp, site: &Site) {
    sqlx::query(
        "UPDATE ai_reports SET report = jsonb_set(report, '{probes}', $2) WHERE site_id = $1",
    )
    .bind(site.id)
    .bind(probe_report())
    .execute(app.pool())
    .await
    .unwrap();
    codoseo_store::geo::reevaluate_quietly(app.pool(), site.id)
        .await
        .unwrap();
}
