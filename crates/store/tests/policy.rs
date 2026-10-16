//! The site policy in Postgres: the 0019 backfill from the AI intent, `policy::save` (mirror,
//! history, the quiet re-evaluation) and the readers that moved onto the policy.

mod support;

use std::borrow::Cow;
use std::collections::HashSet;

use codoseo_core::check::IssueBits;
use codoseo_core::crawl::{RobotsFile, SitemapSummary};
use codoseo_core::output::{CrawlOutput, LinkGraph, SiteSignals, StopReason};
use codoseo_core::page::{Indexability, PageFields, PageRecord};
use codoseo_core::report::{CrawlReport, CrawlSummary};
use codoseo_core::url::url_hash;
use codoseo_geo::eligibility::DirectiveSlug;
use codoseo_geo::findings::FindingKind;
use codoseo_geo::report::{build_report, important_urls};
use codoseo_geo::{Intent, Purpose, Stance};
use codoseo_policy::SitePolicy;
use codoseo_store::finalize::finalize;
use codoseo_store::geo::{self, GeoInput};
use codoseo_store::policy::{self, Actor, SaveError, Via};
use serde_json::{Value, json};
use sqlx::migrate::Migrator;
use sqlx::{Connection, Executor, PgConnection, PgPool};
use support::TestDb;
use url::Url;
use uuid::Uuid;

const WORKER: &str = "test-worker";

fn admin_url() -> String {
    std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://127.0.0.1/postgres".into())
}

#[tokio::test]
async fn the_migration_moves_every_intent_into_the_policy() {
    let name = format!("codoseo_test_{}", Uuid::new_v4().simple());
    let mut admin = PgConnection::connect(&admin_url()).await.unwrap();
    admin
        .execute(format!(r#"CREATE DATABASE "{name}""#).as_str())
        .await
        .unwrap();
    let mut url = Url::parse(&admin_url()).unwrap();
    url.set_path(&format!("/{name}"));
    let pool = PgPool::connect(url.as_str()).await.unwrap();

    // Everything up to 0018, then sites with intents as the old screens saved them.
    let all = sqlx::migrate!("./migrations");
    let before = Migrator {
        migrations: Cow::Owned(
            all.migrations
                .iter()
                .filter(|m| m.version < 19)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    before.run(&pool).await.unwrap();

    let intents: Vec<(Value, bool)> = vec![
        (json!({}), true),
        (
            json!({"purposes": {"training": "block", "agent": "allow"}}),
            true,
        ),
        (
            json!({"bots": {"GPTBot": "allow", "SomeNewBot": "block"}, "accepted_directives": ["max_snippet"]}),
            false,
        ),
    ];
    let mut sites = Vec::new();
    for (i, (intent, probes)) in intents.iter().enumerate() {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO sites (domain, start_url, ai_intent, ai_probes) \
             VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(format!("s{i}.example.com"))
        .bind(format!("https://s{i}.example.com/"))
        .bind(intent)
        .bind(probes)
        .fetch_one(&pool)
        .await
        .unwrap();
        sites.push(id);
    }

    all.run(&pool).await.unwrap();

    for (site, (intent, probes)) in sites.iter().zip(&intents) {
        let old: Intent = serde_json::from_value(intent.clone()).unwrap();
        let p = policy::get(&pool, *site).await.unwrap();
        assert_eq!(p.ai.intent, old, "{intent}");
        assert_eq!(p.ai.probes, *probes);
        assert_eq!(p.version, 1);
        assert_eq!(p.seo, Default::default());
        assert_eq!(geo::get_intent(&pool, *site).await.unwrap(), old);
        assert_eq!(geo::get_ai_probes(&pool, *site).await.unwrap(), *probes);
        // Stored exactly as the policy writes it.
        let stored: Value = sqlx::query_scalar("SELECT policy FROM sites WHERE id = $1")
            .bind(site)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, p.to_json());
    }
    // The customised ones say where their policy came from.
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT site_id, via FROM policy_history ORDER BY site_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let mut want = vec![(sites[1], "migration".to_owned()), (sites[2], "migration".to_owned())];
    want.sort();
    assert_eq!(rows, want);

    pool.close().await;
    admin
        .execute(format!(r#"DROP DATABASE IF EXISTS "{name}" WITH (FORCE)"#).as_str())
        .await
        .unwrap();
}

// ---- save ------------------------------------------------------------------------------------

async fn make_site(pool: &PgPool) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO sites (domain, start_url) VALUES ('example.com', 'https://example.com/') \
         RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn make_account(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("INSERT INTO accounts (email) VALUES ('owner@example.com') RETURNING id")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn page(path: &str) -> PageRecord {
    let url = Url::parse(&format!("https://example.com{path}")).unwrap();
    PageRecord {
        url_hash: url_hash(&url),
        url,
        status: 200,
        redirect_chain: Vec::new(),
        response_ms: 10,
        size_bytes: 1000,
        content_type: Some("text/html; charset=utf-8".to_owned()),
        depth: Some(1),
        in_sitemap: false,
        indexability: Indexability::Indexable,
        fields: PageFields {
            word_count: 100,
            ..PageFields::default()
        },
        inlinks: 1,
        outlinks_internal: 0,
        outlinks_external: 0,
        issues: IssueBits::default(),
        key_hash: 0,
        redirect_target: None,
        outlinks_nofollow: 0,
        error: None,
    }
}

async fn full_crawl(pool: &PgPool, site: Uuid, robots: &str) -> Uuid {
    let crawl: Uuid = sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, worker_id) \
         VALUES ($1, 'example.com', 'manual', 2, 'running', $2) RETURNING id",
    )
    .bind(site)
    .bind(WORKER)
    .fetch_one(pool)
    .await
    .unwrap();
    let out = CrawlOutput {
        origin: Url::parse("https://example.com/").unwrap(),
        pages: ["/", "/a", "/b"].iter().map(|p| page(p)).collect(),
        links: LinkGraph::default(),
        robots: Some(RobotsFile {
            status: 200,
            body: robots.to_owned(),
            hash: 9,
        }),
        sitemap: SitemapSummary::default(),
        stop: StopReason::Completed,
        duration_ms: 1,
        signals: SiteSignals::default(),
    };
    let important = important_urls(&out.pages, &out.origin, &HashSet::new());
    let input = GeoInput::new(build_report(&out, &important));
    let report = CrawlReport {
        health_score: 90,
        checks_passed: 1,
        checks_total: 1,
        counts: Vec::new(),
        inlink_samples: Vec::new(),
        summary: CrawlSummary::default(),
    };
    finalize(pool, crawl, site, WORKER, &out, &report, &[], Some(&input))
        .await
        .unwrap();
    crawl
}

fn nobody() -> Actor {
    Actor {
        via: Via::Api,
        account_id: None,
        api_key_id: None,
    }
}

fn training_blocked() -> SitePolicy {
    let mut p = SitePolicy::default();
    p.ai.intent.purposes.insert(Purpose::Training, Stance::Block);
    p
}

#[tokio::test]
async fn save_writes_the_mirror_and_a_history_row() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool).await;
    let account = make_account(&db.pool).await;

    let mut p = training_blocked();
    p.ai.probes = false;
    p.ai.intent.accepted_directives.insert(DirectiveSlug::Nosnippet);
    let saved = policy::save(&db.pool, site, &p, &Actor::web(account))
        .await
        .unwrap();
    assert!(!saved.had_report);
    assert_eq!((saved.opened, saved.resolved), (0, 0));
    let id = saved.history_id.expect("a change is recorded");

    assert_eq!(policy::get(&db.pool, site).await.unwrap(), p);
    // The mirror: what the previous release reads.
    let (intent, probes): (Value, bool) =
        sqlx::query_as("SELECT ai_intent, ai_probes FROM sites WHERE id = $1")
            .bind(site)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(serde_json::from_value::<Intent>(intent).unwrap(), p.ai.intent);
    assert!(!probes);

    let h = policy::history(&db.pool, site, 10).await.unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].id, id);
    assert_eq!(h[0].via, "web");
    assert_eq!(h[0].account_id, Some(account));
    assert_eq!(h[0].email.as_deref(), Some("owner@example.com"));
    assert!(h[0].summary.contains("ai.purposes.training: default → block"), "{}", h[0].summary);
    assert!(h[0].summary.contains("ai.probes: true → false"), "{}", h[0].summary);
    assert_eq!(h[0].before, Some(SitePolicy::default().to_json()));
    assert_eq!(h[0].after, p.to_json());
    assert_eq!(h[0].crawl_id, None);

    // The same policy again changes nothing and records nothing.
    let again = policy::save(&db.pool, site, &p, &Actor::web(account))
        .await
        .unwrap();
    assert_eq!(again.history_id, None);
    assert_eq!(policy::history(&db.pool, site, 10).await.unwrap().len(), 1);

    let (_, at) = policy::get_with_time(&db.pool, site).await.unwrap();
    assert_eq!(at, Some(h[0].created_at));
}

#[tokio::test]
async fn an_invalid_policy_is_refused_and_nothing_changes() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool).await;
    let mut p = SitePolicy::default();
    p.ai.intent.accepted_directives.insert(DirectiveSlug::Noindex);
    p.seo.must_work = vec!["pricing".into()];
    let err = policy::save(&db.pool, site, &p, &nobody())
        .await
        .unwrap_err();
    let SaveError::Invalid(errors) = err else {
        panic!("not a validation error: {err}");
    };
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert_eq!(policy::get(&db.pool, site).await.unwrap(), SitePolicy::default());
    assert!(policy::history(&db.pool, site, 10).await.unwrap().is_empty());

    let gone = policy::save(&db.pool, Uuid::new_v4(), &SitePolicy::default(), &nobody())
        .await
        .unwrap_err();
    assert!(matches!(gone, SaveError::NotFound));
}

#[tokio::test]
async fn save_re_evaluates_quietly_like_the_intent_save_did() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool).await;
    let crawl = full_crawl(&db.pool, site, "User-agent: *\nAllow: /\n").await;
    let geo_before: Option<i16> = sqlx::query_scalar("SELECT geo_score FROM crawls WHERE id = $1")
        .bind(crawl)
        .fetch_one(&db.pool)
        .await
        .unwrap();

    // Wanting training blocked while GPTBot may crawl opens a quiet incident and no change row.
    let saved = policy::save(
        &db.pool,
        site,
        &training_blocked(),
        &Actor { via: Via::MarkIntended, account_id: None, api_key_id: None },
    )
    .await
    .unwrap();
    assert!(saved.had_report);
    assert_eq!((saved.opened, saved.resolved), (1, 0));
    let open = geo::open_incidents(&db.pool, site).await.unwrap();
    let opened: Vec<_> = open.iter().filter(|i| !i.kind.is_advisory()).collect();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].kind, FindingKind::BotsNotBlocked);
    assert!(opened[0].quiet);

    // The newest crawl was rescored, and the history row has its scores.
    let geo_after: Option<i16> = sqlx::query_scalar("SELECT geo_score FROM crawls WHERE id = $1")
        .bind(crawl)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(saved.geo, (geo_before, geo_after));
    let h = policy::history(&db.pool, site, 10).await.unwrap();
    assert_eq!(h[0].via, "mark_intended");
    assert_eq!(h[0].account_id, None);
    assert_eq!(h[0].crawl_id, Some(crawl));
    assert_eq!(h[0].crawl_number, Some(1));
    assert_eq!((h[0].geo_before, h[0].geo_after), (geo_before, geo_after));
    assert_eq!(h[0].health_after, Some(90));

    // Back to the defaults resolves it again.
    let back = policy::save(&db.pool, site, &SitePolicy::default(), &nobody())
        .await
        .unwrap();
    assert_eq!((back.opened, back.resolved), (0, 1));
}

#[tokio::test]
async fn a_site_without_a_policy_reads_its_mirror_columns() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool).await;
    // A site created after the migration has no policy until its first save.
    sqlx::query(
        "UPDATE sites SET policy = NULL, ai_intent = '{\"purposes\":{\"ads\":\"block\"}}', \
         ai_probes = false WHERE id = $1",
    )
    .bind(site)
    .execute(&db.pool)
    .await
    .unwrap();
    let p = policy::get(&db.pool, site).await.unwrap();
    assert_eq!(p.ai.intent.purpose_stance(Purpose::Ads), Stance::Block);
    assert!(!p.ai.probes);

    // The old writers keep both in step.
    geo::set_ai_probes(&db.pool, site, true).await.unwrap();
    let p = policy::get(&db.pool, site).await.unwrap();
    assert!(p.ai.probes);
    assert_eq!(p.ai.intent.purpose_stance(Purpose::Ads), Stance::Block);
    let mirror: bool = sqlx::query_scalar("SELECT ai_probes FROM sites WHERE id = $1")
        .bind(site)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(mirror);
}
