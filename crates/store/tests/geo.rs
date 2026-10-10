//! GA.5: AI access reports, incidents, intent and the change rows they write, against real
//! Postgres. Reports are built with the real `codoseo_geo` code from hand-made crawls, so the
//! findings are the ones the worker would produce.

mod support;

use std::collections::HashSet;

use codoseo_core::check::{IssueBits, Severity};
use codoseo_core::crawl::{RobotsFile, SitemapSummary};
use codoseo_core::output::{CrawlOutput, LinkGraph, SiteSignals, StopReason};
use codoseo_core::page::{Indexability, PageFields, PageRecord};
use codoseo_core::report::{CrawlReport, CrawlSummary};
use codoseo_core::url::url_hash;
use codoseo_geo::eligibility::DirectiveSlug;
use codoseo_geo::findings::{FindingKind, evaluated_kinds};
use codoseo_geo::report::{build_report, important_urls};
use codoseo_geo::{Intent, Purpose, Stance};
use codoseo_store::alert_rules::{self, ALL_KINDS, DEFAULT_INSTANT};
use codoseo_store::finalize::finalize;
use codoseo_store::geo::{self, GeoInput};
use sqlx::PgPool;
use support::TestDb;
use url::Url;
use uuid::Uuid;

const WORKER: &str = "test-worker";

async fn make_site(pool: &PgPool, domain: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO sites (domain, start_url) VALUES ($1, $2) RETURNING id")
        .bind(domain)
        .bind(format!("https://{domain}/"))
        .fetch_one(pool)
        .await
        .expect("insert site")
}

async fn make_crawl(pool: &PgPool, site: Uuid, trigger: &str, status: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, worker_id) \
         VALUES ($1, 'example.com', $2::crawl_trigger, 2, $3::crawl_status, $4) RETURNING id",
    )
    .bind(site)
    .bind(trigger)
    .bind(status)
    .bind(WORKER)
    .fetch_one(pool)
    .await
    .expect("insert crawl")
}

fn page(path: &str, meta_robots: Option<&str>) -> PageRecord {
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
            meta_robots: meta_robots.map(str::to_owned),
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

/// A five-page crawl of example.com with the given robots.txt answer.
fn crawl_of(robots_status: u16, robots_body: &str, meta_robots: Option<&str>) -> CrawlOutput {
    CrawlOutput {
        origin: Url::parse("https://example.com/").unwrap(),
        pages: ["/", "/a", "/b", "/c", "/d"]
            .iter()
            .map(|p| page(p, meta_robots))
            .collect(),
        links: LinkGraph::default(),
        robots: Some(RobotsFile {
            status: robots_status,
            body: robots_body.to_owned(),
            hash: 9,
        }),
        sitemap: SitemapSummary::default(),
        stop: StopReason::Completed,
        duration_ms: 1,
        signals: SiteSignals::default(),
    }
}

fn input_for(out: &CrawlOutput) -> GeoInput {
    let important = important_urls(&out.pages, &out.origin, &HashSet::new());
    GeoInput::new(build_report(out, &important))
}

fn check_report() -> CrawlReport {
    CrawlReport {
        health_score: 90,
        checks_passed: 1,
        checks_total: 1,
        counts: Vec::new(),
        inlink_samples: Vec::new(),
        summary: CrawlSummary::default(),
    }
}

/// Finalizes a new full crawl of the site with this robots.txt and page markup.
async fn full_crawl(
    pool: &PgPool,
    site: Uuid,
    robots_status: u16,
    robots_body: &str,
    meta_robots: Option<&str>,
) -> Uuid {
    let crawl = make_crawl(pool, site, "manual", "running").await;
    let out = crawl_of(robots_status, robots_body, meta_robots);
    let input = input_for(&out);
    finalize(
        pool,
        crawl,
        site,
        WORKER,
        &out,
        &check_report(),
        &[],
        Some(&input),
    )
    .await
    .expect("finalize");
    crawl
}

const ALLOW_ALL: &str = "User-agent: *\nAllow: /\n";
const BLOCK_OAI: &str = "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /\n";

async fn change_kinds(pool: &PgPool, crawl: Uuid) -> Vec<(String, String)> {
    sqlx::query_as("SELECT kind::text, severity::text FROM changes WHERE crawl_id = $1 ORDER BY id")
        .bind(crawl)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn alert_jobs(pool: &PgPool, crawl: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = 'send_alert' AND payload->>'crawl_id' = $1",
    )
    .bind(crawl.to_string())
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn report_count(pool: &PgPool, site: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ai_reports WHERE site_id = $1")
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_first_report_is_a_quiet_baseline() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    let crawl = full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;

    let open = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].kind, FindingKind::BotsBlocked);
    assert_eq!(open[0].severity, Severity::Critical);
    assert!(open[0].quiet, "the baseline opens quietly");
    assert!(open[0].evidence["bots"][0]["token"] == "OAI-SearchBot");
    assert!(change_kinds(&db.pool, crawl).await.is_empty());
    assert_eq!(alert_jobs(&db.pool, crawl).await, 0);
    assert_eq!(geo::open_counts(&db.pool, site).await.unwrap(), (1, 1));
}

#[tokio::test]
async fn blocking_a_search_bot_opens_an_incident_and_removing_the_rule_resolves_it() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    assert!(
        geo::open_incidents(&db.pool, site)
            .await
            .unwrap()
            .is_empty()
    );

    // (a) the rule appears: one critical incident, one change, one alert job.
    let second = full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    let open = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(
        (open[0].kind, open[0].subject.as_str(), open[0].severity),
        (FindingKind::BotsBlocked, "search", Severity::Critical)
    );
    assert!(!open[0].quiet);
    assert_eq!(open[0].opened_crawl_id, Some(second));
    let changes = change_kinds(&db.pool, second).await;
    assert_eq!(
        changes,
        vec![("ai_bot_blocked".to_owned(), "critical".to_owned())]
    );
    assert_eq!(alert_jobs(&db.pool, second).await, 1);
    let (before, after): (String, String) =
        sqlx::query_as("SELECT before_value, after_value FROM changes WHERE crawl_id = $1")
            .bind(second)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(before, "allowed");
    assert_eq!(after, open[0].title);
    assert_eq!(geo::open_counts(&db.pool, site).await.unwrap(), (1, 1));

    // Still blocked: the incident is updated, not opened again, and nothing new is recorded.
    let third = full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    assert_eq!(geo::open_incidents(&db.pool, site).await.unwrap().len(), 1);
    assert!(change_kinds(&db.pool, third).await.is_empty());
    assert_eq!(alert_jobs(&db.pool, third).await, 0);

    // (b) the rule is removed: resolved by a crawl, with a notice change.
    let fourth = full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    assert!(
        geo::open_incidents(&db.pool, site)
            .await
            .unwrap()
            .is_empty()
    );
    let resolved = geo::recent_resolved(&db.pool, site, 10).await.unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].resolution.as_deref(), Some("fixed"));
    assert!(resolved[0].resolved_at.is_some());
    assert_eq!(
        change_kinds(&db.pool, fourth).await,
        vec![("ai_issue_resolved".to_owned(), "notice".to_owned())]
    );
    assert_eq!(alert_jobs(&db.pool, fourth).await, 1);
    let (before, after): (String, String) =
        sqlx::query_as("SELECT before_value, after_value FROM changes WHERE crawl_id = $1")
            .bind(fourth)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        (before.as_str(), after.as_str()),
        (resolved[0].title.as_str(), "resolved")
    );
    assert_eq!(
        geo::incident(&db.pool, site, resolved[0].id)
            .await
            .unwrap()
            .map(|i| i.id),
        Some(resolved[0].id)
    );
}

#[tokio::test]
async fn a_site_wide_nosnippet_is_exactly_one_incident() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    let second = full_crawl(&db.pool, site, 200, ALLOW_ALL, Some("nosnippet")).await;

    let open = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(
        (open[0].kind, open[0].subject.as_str()),
        (FindingKind::AnswersRestricted, "nosnippet")
    );
    assert_eq!(
        change_kinds(&db.pool, second).await,
        vec![("ai_answers_restricted".to_owned(), "critical".to_owned())]
    );
    let before: String = sqlx::query_scalar("SELECT before_value FROM changes WHERE crawl_id = $1")
        .bind(second)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(before, "eligible");
}

#[tokio::test]
async fn a_failing_robots_txt_on_a_finished_crawl_opens_robots_unavailable() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    // A robots.txt that fails judges only RobotsUnavailable and leaves the rest alone.
    let second = full_crawl(&db.pool, site, 503, "", None).await;
    let open = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].kind, FindingKind::RobotsUnavailable);
    assert_eq!(
        change_kinds(&db.pool, second).await,
        vec![("ai_bot_blocked".to_owned(), "critical".to_owned())]
    );
}

#[tokio::test]
async fn the_failed_crawl_path_records_the_incident_once_across_a_retry() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;

    // The crawl fails with nothing but a 503 robots.txt: no pages, the origin is the only
    // important URL.
    let crawl = make_crawl(&db.pool, site, "manual", "running").await;
    let mut out = crawl_of(503, "", None);
    out.pages.clear();
    out.stop = StopReason::Blocked("robots.txt unavailable".to_owned());
    let input = input_for(&out);
    assert_eq!(
        evaluated_kinds(&input.report),
        vec![FindingKind::RobotsUnavailable]
    );
    let n = geo::record_failed_crawl(&db.pool, site, crawl, WORKER, &input)
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        change_kinds(&db.pool, crawl).await,
        vec![("ai_bot_blocked".to_owned(), "critical".to_owned())]
    );
    assert_eq!(alert_jobs(&db.pool, crawl).await, 1);
    // The incident from the earlier crawl (bots_blocked) was not judged, so it stays open.
    let kinds: Vec<_> = geo::open_incidents(&db.pool, site)
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.kind)
        .collect();
    assert_eq!(
        kinds.iter().copied().collect::<HashSet<_>>(),
        HashSet::from([FindingKind::BotsBlocked, FindingKind::RobotsUnavailable])
    );

    // Recorded again under the same crawl id: one report, no new change.
    let n = geo::record_failed_crawl(&db.pool, site, crawl, WORKER, &input)
        .await
        .unwrap();
    assert_eq!(n, 0);
    assert_eq!(report_count(&db.pool, site).await, 2);
    assert_eq!(change_kinds(&db.pool, crawl).await.len(), 1);
    let latest = geo::latest_report(&db.pool, site).await.unwrap().unwrap();
    assert_eq!(latest.crawl_id, crawl);
    assert_eq!(latest.crawl_status, "running");
}

#[tokio::test]
async fn an_intent_change_resolves_and_opens_quietly_without_changes() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    let second = full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    assert_eq!(geo::open_incidents(&db.pool, site).await.unwrap().len(), 1);

    let intent = Intent {
        bots: [("OAI-SearchBot".to_owned(), Stance::Any)].into(),
        ..Intent::default()
    };
    geo::set_intent(&db.pool, site, &intent).await.unwrap();
    assert_eq!(geo::get_intent(&db.pool, site).await.unwrap(), intent);
    let (opened, resolved) = geo::reevaluate_quietly(&db.pool, site).await.unwrap();
    assert_eq!((opened, resolved), (0, 1));
    assert!(
        geo::open_incidents(&db.pool, site)
            .await
            .unwrap()
            .is_empty()
    );
    let r = geo::recent_resolved(&db.pool, site, 5).await.unwrap();
    assert_eq!(r[0].resolution.as_deref(), Some("intent"));
    assert_eq!(
        change_kinds(&db.pool, second).await.len(),
        1,
        "only the original change"
    );
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM changes WHERE site_id = $1")
        .bind(site)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(total, 1);

    // Wanting training blocked while GPTBot may crawl opens a quiet incident, no change row.
    let intent = Intent {
        purposes: [(Purpose::Training, Stance::Block)].into(),
        ..intent
    };
    geo::set_intent(&db.pool, site, &intent).await.unwrap();
    let (opened, resolved) = geo::reevaluate_quietly(&db.pool, site).await.unwrap();
    assert_eq!((opened, resolved), (1, 0));
    let open = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(open[0].kind, FindingKind::BotsNotBlocked);
    assert!(open[0].quiet);
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM changes WHERE site_id = $1")
        .bind(site)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(total, 1);
}

/// `(last_seen_crawl_id, last_seen_at)` of the site's open incident of this kind.
async fn last_seen(pool: &PgPool, site: Uuid, kind: &str) -> (Uuid, time::OffsetDateTime) {
    sqlx::query_as(
        "SELECT last_seen_crawl_id, last_seen_at FROM ai_incidents \
         WHERE site_id = $1 AND kind = $2 AND resolved_at IS NULL",
    )
    .bind(site)
    .bind(kind)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn only_a_crawl_moves_last_seen_never_an_intent_change() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    let second = full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    // Pretend the crawl that saw it ran a while ago.
    let then = time::macros::datetime!(2026-01-02 03:04:05 UTC);
    sqlx::query("UPDATE ai_incidents SET last_seen_at = $2 WHERE site_id = $1")
        .bind(site)
        .bind(then)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        last_seen(&db.pool, site, "bots_blocked").await,
        (second, then)
    );

    // An intent change that keeps it open (and opens another one) only re-reads the stored
    // report: nothing was seen again, twice over.
    let intent = Intent {
        purposes: [(Purpose::Training, Stance::Block)].into(),
        ..Intent::default()
    };
    geo::set_intent(&db.pool, site, &intent).await.unwrap();
    let (opened, resolved) = geo::reevaluate_quietly(&db.pool, site).await.unwrap();
    assert_eq!((opened, resolved), (1, 0));
    geo::reevaluate_quietly(&db.pool, site).await.unwrap();
    assert_eq!(
        last_seen(&db.pool, site, "bots_blocked").await,
        (second, then)
    );

    // The next crawl that still finds it moves it.
    let third = full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    let (crawl, at) = last_seen(&db.pool, site, "bots_blocked").await;
    assert_eq!(crawl, third);
    assert!(at > then);
}

/// Records a crawl that failed with a 503 robots.txt and nothing else.
async fn failed_robots_crawl(pool: &PgPool, site: Uuid) -> Uuid {
    let crawl = make_crawl(pool, site, "manual", "running").await;
    let mut out = crawl_of(503, "", None);
    out.pages.clear();
    geo::record_failed_crawl(pool, site, crawl, WORKER, &input_for(&out))
        .await
        .unwrap();
    sqlx::query("UPDATE crawls SET status = 'failed' WHERE id = $1")
        .bind(crawl)
        .execute(pool)
        .await
        .unwrap();
    crawl
}

#[tokio::test]
async fn reevaluating_after_a_failed_crawl_judges_each_kind_by_the_newest_report_that_could() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    let failed = failed_robots_crawl(&db.pool, site).await;
    let latest = geo::latest_report(&db.pool, site).await.unwrap().unwrap();
    assert_eq!(
        (latest.crawl_id, latest.crawl_status.as_str()),
        (failed, "failed")
    );

    // The newest report is the failed crawl's and can't judge bots_blocked; the crawl before it
    // can, so "Mark intended" on the blocked bot still resolves the incident.
    let intent = Intent {
        bots: [("OAI-SearchBot".to_owned(), Stance::Any)].into(),
        ..Intent::default()
    };
    geo::set_intent(&db.pool, site, &intent).await.unwrap();
    let (opened, resolved) = geo::reevaluate_quietly(&db.pool, site).await.unwrap();
    assert_eq!((opened, resolved), (0, 1));
    let open = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].kind, FindingKind::RobotsUnavailable);
    let r = geo::recent_resolved(&db.pool, site, 5).await.unwrap();
    assert_eq!(
        (r[0].kind, r[0].resolution.as_deref()),
        (FindingKind::BotsBlocked, Some("intent"))
    );
}

#[tokio::test]
async fn another_sites_incidents_are_untouched() {
    let db = TestDb::new().await;
    let a = make_site(&db.pool, "a.example").await;
    let b = make_site(&db.pool, "b.example").await;
    for site in [a, b] {
        full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
        full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    }
    // Site A fixes its robots.txt and changes its intent; B keeps its incident.
    full_crawl(&db.pool, a, 200, ALLOW_ALL, None).await;
    geo::reevaluate_quietly(&db.pool, a).await.unwrap();
    assert!(geo::open_incidents(&db.pool, a).await.unwrap().is_empty());
    let open_b = geo::open_incidents(&db.pool, b).await.unwrap();
    assert_eq!(open_b.len(), 1);
    assert!(open_b[0].resolved_at.is_none());
    assert_eq!(geo::open_counts(&db.pool, b).await.unwrap(), (1, 1));
    // An incident id of one site is not readable through another.
    assert!(
        geo::incident(&db.pool, a, open_b[0].id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn only_the_two_newest_reports_per_site_are_kept() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    let other = make_site(&db.pool, "other.example").await;
    full_crawl(&db.pool, other, 200, ALLOW_ALL, None).await;
    let mut crawls = Vec::new();
    for _ in 0..4 {
        crawls.push(full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await);
    }
    assert_eq!(report_count(&db.pool, site).await, 2);
    let kept: Vec<Uuid> = sqlx::query_scalar("SELECT crawl_id FROM ai_reports WHERE site_id = $1")
        .bind(site)
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        kept.iter().copied().collect::<HashSet<_>>(),
        HashSet::from([crawls[2], crawls[3]])
    );
    assert_eq!(report_count(&db.pool, other).await, 1);
}

#[tokio::test]
async fn changed_ai_preferences_are_one_notice_and_a_moved_line_is_not_a_change() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(
        &db.pool,
        site,
        200,
        "User-agent: *\nContent-Signal: ai-train=no\n",
        None,
    )
    .await;
    // Same preference on another line: nothing.
    let moved = full_crawl(
        &db.pool,
        site,
        200,
        "# comment\nUser-agent: *\nContent-Signal: ai-train=no\n",
        None,
    )
    .await;
    assert!(change_kinds(&db.pool, moved).await.is_empty());
    let changed = full_crawl(
        &db.pool,
        site,
        200,
        "User-agent: *\nContent-Signal: ai-train=yes\n",
        None,
    )
    .await;
    assert_eq!(
        change_kinds(&db.pool, changed).await,
        vec![("ai_preferences_changed".to_owned(), "notice".to_owned())]
    );
    let (before, after): (String, String) =
        sqlx::query_as("SELECT before_value, after_value FROM changes WHERE crawl_id = $1")
            .bind(changed)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(before.contains("ai-train=no"), "{before}");
    assert!(after.contains("ai-train=yes"), "{after}");
    assert_eq!(alert_jobs(&db.pool, changed).await, 1);

    // A failing robots.txt says nothing about preferences, so it is no change either.
    let failing = full_crawl(&db.pool, site, 503, "", None).await;
    assert!(
        !change_kinds(&db.pool, failing)
            .await
            .iter()
            .any(|(k, _)| k == "ai_preferences_changed")
    );
}

#[tokio::test]
async fn change_text_is_cut_to_300_characters_on_a_character_boundary() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    // A long, multi-byte declared preference.
    let long = "é".repeat(400);
    let body = format!("User-agent: *\nContent-Signal: ai-train={long}\n");
    let crawl = full_crawl(&db.pool, site, 200, &body, None).await;
    let after: String = sqlx::query_scalar(
        "SELECT after_value FROM changes WHERE crawl_id = $1 AND kind = 'ai_preferences_changed'",
    )
    .bind(crawl)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(after.chars().count(), 300);
    assert!(after.ends_with('…'));
}

#[tokio::test]
async fn intent_validation_rejects_nonsense_tokens() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    let bad = Intent {
        bots: [("".to_owned(), Stance::Block)].into(),
        ..Intent::default()
    };
    assert!(matches!(
        geo::set_intent(&db.pool, site, &bad).await,
        Err(geo::IntentError::Invalid(_))
    ));
    assert_eq!(
        geo::get_intent(&db.pool, site).await.unwrap(),
        Intent::default()
    );
}

#[tokio::test]
async fn the_new_instant_kinds_are_default_rules_and_reach_an_existing_site() {
    assert!(ALL_KINDS.len() >= 17);
    for kind in [
        codoseo_core::change::ChangeKind::AiBotBlocked,
        codoseo_core::change::ChangeKind::AiAnswersRestricted,
        codoseo_core::change::ChangeKind::AiIssueResolved,
    ] {
        assert!(DEFAULT_INSTANT.contains(&kind), "{kind:?}");
    }
    for kind in [
        codoseo_core::change::ChangeKind::AiBlockNotApplied,
        codoseo_core::change::ChangeKind::AiPreferencesChanged,
    ] {
        assert!(
            !DEFAULT_INSTANT.contains(&kind),
            "{kind:?} waits for the digest"
        );
        assert!(ALL_KINDS.contains(&kind));
    }

    let db = TestDb::new().await;
    let key = codoseo_notify::ChannelKey::derive("k");
    let account: Uuid =
        sqlx::query_scalar("INSERT INTO accounts (email, email_canonical) VALUES ('o@example.com', 'o@example.com') RETURNING id")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let site: Uuid = sqlx::query_scalar(
        "INSERT INTO sites (account_id, domain, start_url) \
         VALUES ($1, 'example.com', 'https://example.com/') RETURNING id",
    )
    .bind(account)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let channel = codoseo_store::channels::ensure_default_email(&db.pool, &key, account)
        .await
        .unwrap();
    // A site from before the GEO kinds: it has only the five original rules.
    alert_rules::create_defaults_for_site(&db.pool, account, site)
        .await
        .unwrap();
    sqlx::query("DELETE FROM alert_rules WHERE site_id = $1 AND change_kind::text LIKE 'ai_%'")
        .bind(site)
        .execute(&db.pool)
        .await
        .unwrap();
    let count = |pool: &PgPool| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM alert_rules WHERE site_id = $1")
                .bind(site)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(count(&db.pool).await, 5);

    alert_rules::create_defaults_for_site(&db.pool, account, site)
        .await
        .unwrap();
    assert_eq!(count(&db.pool).await, 8);
    for kind in [
        codoseo_core::change::ChangeKind::AiBotBlocked,
        codoseo_core::change::ChangeKind::AiAnswersRestricted,
        codoseo_core::change::ChangeKind::AiIssueResolved,
    ] {
        assert_eq!(
            alert_rules::instant_channels_for(&db.pool, site, kind)
                .await
                .unwrap(),
            vec![channel]
        );
    }
}

#[tokio::test]
async fn a_stale_workers_finalize_leaves_no_geo_rows() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;

    let crawl = make_crawl(&db.pool, site, "manual", "running").await;
    sqlx::query("UPDATE crawls SET worker_id = 'worker-b' WHERE id = $1")
        .bind(crawl)
        .execute(&db.pool)
        .await
        .unwrap();
    let out = crawl_of(200, BLOCK_OAI, None);
    let input = input_for(&out);
    let result = finalize(
        &db.pool,
        crawl,
        site,
        WORKER,
        &out,
        &check_report(),
        &[],
        Some(&input),
    )
    .await;
    assert!(result.is_err());

    assert_eq!(report_count(&db.pool, site).await, 1);
    assert!(
        geo::open_incidents(&db.pool, site)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(change_kinds(&db.pool, crawl).await.is_empty());
    assert_eq!(alert_jobs(&db.pool, crawl).await, 0);
}

/// Finalizes a new full crawl with this output.
async fn finalize_out(pool: &PgPool, site: Uuid, out: &CrawlOutput) -> Uuid {
    let crawl = make_crawl(pool, site, "manual", "running").await;
    finalize(
        pool,
        crawl,
        site,
        WORKER,
        out,
        &check_report(),
        &[],
        Some(&input_for(out)),
    )
    .await
    .expect("finalize");
    crawl
}

async fn quiet_of(pool: &PgPool, site: Uuid, kind: &str) -> bool {
    sqlx::query_scalar(
        "SELECT quiet FROM ai_incidents WHERE site_id = $1 AND kind = $2 AND resolved_at IS NULL",
    )
    .bind(site)
    .bind(kind)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn an_intent_saved_while_a_crawl_runs_is_the_one_its_findings_follow() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;

    // A crawl that still sees the block has built its report...
    let crawl = make_crawl(&db.pool, site, "manual", "running").await;
    let out = crawl_of(200, BLOCK_OAI, None);
    let input = input_for(&out);
    // ...when the owner marks the block as intended.
    let intent = Intent {
        bots: [("OAI-SearchBot".to_owned(), Stance::Any)].into(),
        ..Intent::default()
    };
    geo::set_intent(&db.pool, site, &intent).await.unwrap();
    assert_eq!(
        geo::reevaluate_quietly(&db.pool, site).await.unwrap(),
        (0, 1)
    );
    finalize(
        &db.pool,
        crawl,
        site,
        WORKER,
        &out,
        &check_report(),
        &[],
        Some(&input),
    )
    .await
    .expect("finalize");

    // The crawl follows the intent stored when it finished: nothing reopens, nobody is told.
    assert!(
        geo::open_incidents(&db.pool, site)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(change_kinds(&db.pool, crawl).await.is_empty());
    assert_eq!(alert_jobs(&db.pool, crawl).await, 0);
}

#[tokio::test]
async fn a_kind_first_judged_after_failed_crawls_opens_quietly() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    // The site's first report is a failing robots.txt: it judges only that.
    let failed = failed_robots_crawl(&db.pool, site).await;
    assert!(change_kinds(&db.pool, failed).await.is_empty());
    assert!(quiet_of(&db.pool, site, "robots_unavailable").await);

    // The first good crawl is the baseline for the bots and the answers: no storm.
    let good = full_crawl(&db.pool, site, 200, BLOCK_OAI, Some("nosnippet")).await;
    let open = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(open.len(), 2);
    assert!(open.iter().all(|i| i.quiet), "{open:?}");
    assert!(change_kinds(&db.pool, good).await.is_empty());
    assert_eq!(alert_jobs(&db.pool, good).await, 0);
}

#[tokio::test]
async fn a_run_of_failed_crawls_keeps_the_last_good_report_and_its_baselines() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    let good = full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    failed_robots_crawl(&db.pool, site).await;
    failed_robots_crawl(&db.pool, site).await;
    // The good report is three back, and still kept: it is the only one that judged the bots.
    let kept: Vec<Uuid> = sqlx::query_scalar("SELECT crawl_id FROM ai_reports WHERE site_id = $1")
        .bind(site)
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(kept.len(), 3);
    assert!(kept.contains(&good));

    // So a block that appears next is news, not a baseline (and robots.txt is fixed).
    let blocked = full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    assert_eq!(
        change_kinds(&db.pool, blocked).await,
        vec![
            ("ai_issue_resolved".to_owned(), "notice".to_owned()),
            ("ai_bot_blocked".to_owned(), "critical".to_owned())
        ]
    );
    // With a good report newest again, the older ones go.
    assert_eq!(report_count(&db.pool, site).await, 2);
}

#[tokio::test]
async fn preferences_are_compared_with_the_newest_report_that_knew_them() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(
        &db.pool,
        site,
        200,
        "User-agent: *\nContent-Signal: ai-train=no\n",
        None,
    )
    .await;
    failed_robots_crawl(&db.pool, site).await;
    // Changed while robots.txt was failing: still noticed, against the crawl before.
    let changed = full_crawl(
        &db.pool,
        site,
        200,
        "User-agent: *\nContent-Signal: ai-train=yes\n",
        None,
    )
    .await;
    let (before, after): (String, String) = sqlx::query_as(
        "SELECT before_value, after_value FROM changes \
         WHERE crawl_id = $1 AND kind = 'ai_preferences_changed'",
    )
    .bind(changed)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(before.contains("ai-train=no"), "{before}");
    assert!(after.contains("ai-train=yes"), "{after}");
}

#[tokio::test]
async fn an_error_home_page_says_nothing_about_header_preferences() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    let with_header = |home_status: u16| {
        let mut out = crawl_of(200, ALLOW_ALL, None);
        out.pages[0].status = home_status;
        if home_status == 200 {
            out.signals.home_headers =
                vec![("content-signal".to_owned(), "ai-train=no".to_owned())];
        }
        out
    };
    finalize_out(&db.pool, site, &with_header(200)).await;
    // The home page answers 503 without the header: unknown, not removed.
    let down = finalize_out(&db.pool, site, &with_header(503)).await;
    // And back: compared with the last crawl that read it, nothing changed.
    let back = finalize_out(&db.pool, site, &with_header(200)).await;
    for crawl in [down, back] {
        assert!(
            !change_kinds(&db.pool, crawl)
                .await
                .iter()
                .any(|(k, _)| k == "ai_preferences_changed"),
            "{crawl}"
        );
    }
}

#[tokio::test]
async fn a_widening_incident_is_announced_and_its_resolution_too() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    // The baseline: OAI-SearchBot blocked, quietly.
    full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    assert!(quiet_of(&db.pool, site, "bots_blocked").await);

    // PerplexityBot is blocked too: same incident, same severity, but news.
    let both = format!("{BLOCK_OAI}\nUser-agent: PerplexityBot\nDisallow: /\n");
    let widened = full_crawl(&db.pool, site, 200, &both, None).await;
    let open = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(open.len(), 1);
    let (kind, after): (String, String) =
        sqlx::query_as("SELECT kind::text, after_value FROM changes WHERE crawl_id = $1")
            .bind(widened)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(kind, "ai_bot_blocked");
    assert_eq!(after, open[0].title);
    assert!(after.contains("OAI-SearchBot and PerplexityBot"), "{after}");
    assert!(!open[0].quiet, "announced, so no longer quiet");
    assert_eq!(alert_jobs(&db.pool, widened).await, 1);

    // One bot unblocked is not news; both unblocked is a resolution the owner hears of.
    let narrowed = full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    assert!(change_kinds(&db.pool, narrowed).await.is_empty());
    let fixed = full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    assert_eq!(
        change_kinds(&db.pool, fixed).await,
        vec![("ai_issue_resolved".to_owned(), "notice".to_owned())]
    );
}

#[tokio::test]
async fn a_quiet_incident_that_escalates_is_loud_from_then_on() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    let below_home = "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /a\n";
    full_crawl(&db.pool, site, 200, below_home, None).await;
    let open = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(open[0].severity, Severity::Warning);
    assert!(open[0].quiet);

    let worse = full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    assert_eq!(
        change_kinds(&db.pool, worse).await,
        vec![("ai_bot_blocked".to_owned(), "critical".to_owned())]
    );
    assert!(!quiet_of(&db.pool, site, "bots_blocked").await);
    let fixed = full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    assert_eq!(
        change_kinds(&db.pool, fixed).await,
        vec![("ai_issue_resolved".to_owned(), "notice".to_owned())]
    );
}

#[tokio::test]
async fn an_accepted_directive_resolves_its_incident_and_noindex_cannot_be_accepted() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, Some("nosnippet")).await;
    assert_eq!(geo::open_incidents(&db.pool, site).await.unwrap().len(), 1);

    let accept = Intent {
        accepted_directives: [DirectiveSlug::Nosnippet].into(),
        ..Intent::default()
    };
    geo::set_intent(&db.pool, site, &accept).await.unwrap();
    assert_eq!(geo::get_intent(&db.pool, site).await.unwrap(), accept);
    assert_eq!(
        geo::reevaluate_quietly(&db.pool, site).await.unwrap(),
        (0, 1)
    );
    // The next crawl that still finds it opens nothing.
    let again = full_crawl(&db.pool, site, 200, ALLOW_ALL, Some("nosnippet")).await;
    assert!(change_kinds(&db.pool, again).await.is_empty());
    assert!(
        geo::open_incidents(&db.pool, site)
            .await
            .unwrap()
            .is_empty()
    );

    let noindex = Intent {
        accepted_directives: [DirectiveSlug::Noindex].into(),
        ..Intent::default()
    };
    assert!(matches!(
        geo::set_intent(&db.pool, site, &noindex).await,
        Err(geo::IntentError::Invalid(_))
    ));
    assert_eq!(geo::get_intent(&db.pool, site).await.unwrap(), accept);
}

#[tokio::test]
async fn a_site_wide_noarchive_is_one_incident_naming_bing() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    let second = full_crawl(&db.pool, site, 200, ALLOW_ALL, Some("noarchive")).await;
    let open = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(
        (open[0].kind, open[0].subject.as_str()),
        (FindingKind::AnswersRestricted, "noarchive")
    );
    let engines: Vec<&str> = open[0].evidence["engines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["engine"].as_str().unwrap())
        .collect();
    assert_eq!(engines, ["bing"]);
    assert_eq!(
        change_kinds(&db.pool, second).await,
        vec![("ai_answers_restricted".to_owned(), "critical".to_owned())]
    );
}

#[tokio::test]
async fn a_stale_worker_cannot_record_a_failed_crawl() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    let mut out = crawl_of(503, "", None);
    out.pages.clear();
    let input = input_for(&out);

    // Requeued and claimed by another worker, or already requeued.
    let crawl = make_crawl(&db.pool, site, "manual", "running").await;
    sqlx::query("UPDATE crawls SET worker_id = 'worker-b' WHERE id = $1")
        .bind(crawl)
        .execute(&db.pool)
        .await
        .unwrap();
    let queued = make_crawl(&db.pool, site, "manual", "queued").await;
    for id in [crawl, queued] {
        let result = geo::record_failed_crawl(&db.pool, site, id, WORKER, &input).await;
        assert!(
            matches!(result, Err(sqlx::Error::RowNotFound)),
            "{result:?}"
        );
        assert!(change_kinds(&db.pool, id).await.is_empty());
        assert_eq!(alert_jobs(&db.pool, id).await, 0);
    }
    assert_eq!(report_count(&db.pool, site).await, 1);
    assert!(
        geo::open_incidents(&db.pool, site)
            .await
            .unwrap()
            .is_empty()
    );
}
