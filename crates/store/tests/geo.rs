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
use codoseo_geo::incident::Before;
use codoseo_geo::report::{build_report, important_urls};
use codoseo_geo::robots::GroupMatch;
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
async fn an_incident_opened_by_a_later_crawl_keeps_what_the_earlier_report_held() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    let open = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(open.len(), 1);
    let before: Before =
        serde_json::from_value(open[0].before.clone().expect("before stored")).unwrap();
    assert_eq!(before.bots.len(), 1);
    let bot = &before.bots[0];
    assert_eq!(bot.token, "OAI-SearchBot");
    assert_eq!(bot.group, GroupMatch::Wildcard);
    // The home page was allowed by `User-agent: *` / `Allow: /` on line 2.
    assert_eq!(bot.paths[0].path, "/");
    assert!(bot.paths[0].allowed);
    assert_eq!(
        bot.paths[0].rule.as_ref().map(|r| (r.line, r.allow)),
        Some((2, true))
    );
    assert!(bot.paths.iter().all(|p| p.allowed));

    // The next crawl updates the incident and leaves `before` as it was.
    full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    let again = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(again[0].before, open[0].before);
}

#[tokio::test]
async fn a_baseline_incident_has_nothing_before_it() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    let open = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(open[0].before, None);
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
    assert!(ALL_KINDS.len() >= 20);
    for kind in [
        codoseo_core::change::ChangeKind::AiBotBlocked,
        codoseo_core::change::ChangeKind::AiAnswersRestricted,
        codoseo_core::change::ChangeKind::AiIssueResolved,
        codoseo_core::change::ChangeKind::AiFetchBlocked,
        codoseo_core::change::ChangeKind::AiContentMissing,
    ] {
        assert!(DEFAULT_INSTANT.contains(&kind), "{kind:?}");
        assert!(ALL_KINDS.contains(&kind));
    }
    for kind in [
        codoseo_core::change::ChangeKind::AiBlockNotApplied,
        codoseo_core::change::ChangeKind::AiPreferencesChanged,
        codoseo_core::change::ChangeKind::AiLlmsTxtUnavailable,
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
    assert_eq!(count(&db.pool).await, 10);
    for kind in [
        codoseo_core::change::ChangeKind::AiBotBlocked,
        codoseo_core::change::ChangeKind::AiAnswersRestricted,
        codoseo_core::change::ChangeKind::AiIssueResolved,
        codoseo_core::change::ChangeKind::AiFetchBlocked,
        codoseo_core::change::ChangeKind::AiContentMissing,
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

// ---- Synthetic fetch probes (GB.8) -------------------------------------------------------------

use codoseo_geo::probe::{ProbeFetch, ProbeReport, ProbeResult, ProbedUrl, Vendor, Verdict};

fn probe_fetch(status: u16, title: &str, bytes: u32) -> ProbeFetch {
    ProbeFetch {
        status: Some(status),
        html: true,
        bytes,
        title: Some(title.to_owned()),
        ..ProbeFetch::default()
    }
}

fn cloudflare() -> Verdict {
    Verdict::Challenged {
        vendor: Vendor::Cloudflare,
    }
}

/// Finalizes a new full crawl that allows every bot in robots.txt and whose probes of the home
/// page gave these verdicts.
async fn probed_crawl(pool: &PgPool, site: Uuid, verdicts: &[(&str, Verdict)]) -> Uuid {
    let crawl = make_crawl(pool, site, "manual", "running").await;
    let out = crawl_of(200, ALLOW_ALL, None);
    let mut input = input_for(&out);
    let results: Vec<ProbeResult> = verdicts
        .iter()
        .map(|(token, verdict)| ProbeResult {
            token: (*token).to_owned(),
            fetch: Some(match verdict {
                Verdict::Same => probe_fetch(200, "Home", 20_000),
                _ => probe_fetch(403, "Just a moment...", 4_000),
            }),
            verdict: verdict.clone(),
        })
        .collect();
    input.report.probes = Some(ProbeReport {
        version: 1,
        synthetic: true,
        from: "hello@codoseo.com".to_owned(),
        bots: verdicts.iter().map(|(t, _)| (*t).to_owned()).collect(),
        skipped_bots: Vec::new(),
        urls: vec![ProbedUrl {
            url: 0,
            baseline: probe_fetch(200, "Home", 20_000),
            results,
        }],
        stop: None,
        requests: 0,
    });
    assert!(evaluated_kinds(&input.report).contains(&FindingKind::FetchBlocked));
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

/// The site's open `fetch_blocked` incidents.
async fn fetch_blocked_open(pool: &PgPool, site: Uuid) -> Vec<geo::Incident> {
    geo::open_incidents(pool, site)
        .await
        .unwrap()
        .into_iter()
        .filter(|i| i.kind == FindingKind::FetchBlocked)
        .collect()
}

#[tokio::test]
async fn the_first_probes_open_fetch_blocked_quietly_even_after_earlier_reports() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    // Phase A reports only: they judged robots.txt and markup, never fetches.
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;

    let crawl = probed_crawl(&db.pool, site, &[("OAI-SearchBot", cloudflare())]).await;
    let open = fetch_blocked_open(&db.pool, site).await;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].subject, "challenge_cloudflare");
    assert_eq!(open[0].severity, Severity::Warning, "the home page");
    assert!(
        open[0].quiet,
        "the first report that probes is fetch_blocked's baseline"
    );
    assert!(change_kinds(&db.pool, crawl).await.is_empty());
    assert_eq!(alert_jobs(&db.pool, crawl).await, 0);
}

#[tokio::test]
async fn probes_widen_resolve_and_a_report_without_probes_leaves_them_alone() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    probed_crawl(&db.pool, site, &[("OAI-SearchBot", cloudflare())]).await;
    assert!(quiet_of(&db.pool, site, "fetch_blocked").await);

    // Claude-SearchBot is challenged too: the same incident, widened, and announced.
    let widened = probed_crawl(
        &db.pool,
        site,
        &[
            ("OAI-SearchBot", cloudflare()),
            ("Claude-SearchBot", cloudflare()),
        ],
    )
    .await;
    let open = fetch_blocked_open(&db.pool, site).await;
    assert_eq!(open.len(), 1);
    assert!(!open[0].quiet, "announced, so no longer quiet");
    assert_eq!(
        change_kinds(&db.pool, widened).await,
        vec![("ai_fetch_blocked".to_owned(), "warning".to_owned())]
    );
    let (before, after): (String, String) =
        sqlx::query_as("SELECT before_value, after_value FROM changes WHERE crawl_id = $1")
            .bind(widened)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(before, "reachable");
    assert_eq!(after, open[0].title);
    assert_eq!(alert_jobs(&db.pool, widened).await, 1);

    // A crawl without probes (switched off, or a failed run) can't judge fetches.
    let unprobed = full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    let still = fetch_blocked_open(&db.pool, site).await;
    assert_eq!(still.len(), 1);
    assert_eq!(still[0].id, open[0].id);
    assert!(change_kinds(&db.pool, unprobed).await.is_empty());

    // Every bot gets the page again: resolved, and the owner hears of it.
    let clear = probed_crawl(
        &db.pool,
        site,
        &[
            ("OAI-SearchBot", Verdict::Same),
            ("Claude-SearchBot", Verdict::Same),
        ],
    )
    .await;
    assert!(fetch_blocked_open(&db.pool, site).await.is_empty());
    let resolved = geo::recent_resolved(&db.pool, site, 5).await.unwrap();
    assert_eq!(resolved[0].kind, FindingKind::FetchBlocked);
    assert_eq!(resolved[0].resolution.as_deref(), Some("fixed"));
    assert_eq!(
        change_kinds(&db.pool, clear).await,
        vec![("ai_issue_resolved".to_owned(), "notice".to_owned())]
    );
}

#[tokio::test]
async fn the_newest_probed_report_is_kept_and_an_intent_change_resolves_from_it_quietly() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    let probed = probed_crawl(
        &db.pool,
        site,
        &[
            ("OAI-SearchBot", cloudflare()),
            ("Claude-SearchBot", cloudflare()),
        ],
    )
    .await;
    for _ in 0..3 {
        full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    }
    // The two newest, plus the newest that judged fetch_blocked.
    assert_eq!(report_count(&db.pool, site).await, 3);
    let kept: Vec<Uuid> = sqlx::query_scalar("SELECT crawl_id FROM ai_reports WHERE site_id = $1")
        .bind(site)
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert!(kept.contains(&probed));
    assert_eq!(fetch_blocked_open(&db.pool, site).await.len(), 1);
    let changes_before: i64 = sqlx::query_scalar("SELECT count(*) FROM changes WHERE site_id = $1")
        .bind(site)
        .fetch_one(&db.pool)
        .await
        .unwrap();

    // The owner wants both bots kept out after all: the challenge is no longer a problem.
    let intent = Intent {
        bots: [
            ("OAI-SearchBot".to_owned(), Stance::Block),
            ("Claude-SearchBot".to_owned(), Stance::Block),
        ]
        .into(),
        ..Intent::default()
    };
    geo::set_intent(&db.pool, site, &intent).await.unwrap();
    geo::reevaluate_quietly(&db.pool, site).await.unwrap();
    assert!(fetch_blocked_open(&db.pool, site).await.is_empty());
    let resolved: Vec<geo::Incident> = geo::recent_resolved(&db.pool, site, 10)
        .await
        .unwrap()
        .into_iter()
        .filter(|i| i.kind == FindingKind::FetchBlocked)
        .collect();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].resolution.as_deref(), Some("intent"));
    let changes_after: i64 = sqlx::query_scalar("SELECT count(*) FROM changes WHERE site_id = $1")
        .bind(site)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(changes_after, changes_before, "an intent change is quiet");
}

#[tokio::test]
async fn the_probe_switch_defaults_on_and_round_trips() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    let other = make_site(&db.pool, "other.example").await;
    assert!(geo::get_ai_probes(&db.pool, site).await.unwrap());
    geo::set_ai_probes(&db.pool, site, false).await.unwrap();
    assert!(!geo::get_ai_probes(&db.pool, site).await.unwrap());
    assert!(geo::get_ai_probes(&db.pool, other).await.unwrap());
    geo::set_ai_probes(&db.pool, site, true).await.unwrap();
    assert!(geo::get_ai_probes(&db.pool, site).await.unwrap());
}

#[tokio::test]
async fn a_bot_inconclusive_for_one_run_neither_narrows_rewidens_nor_resolves() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    let both = [
        ("OAI-SearchBot", cloudflare()),
        ("PerplexityBot", cloudflare()),
    ];
    probed_crawl(&db.pool, site, &both).await;
    let open = fetch_blocked_open(&db.pool, site).await;
    assert_eq!(open.len(), 1);
    let unstable = Verdict::Inconclusive {
        reason: codoseo_geo::probe::InconclusiveReason::Unstable,
    };

    // PerplexityBot's re-fetch disagreed: it keeps its place in the incident.
    let flaky = probed_crawl(
        &db.pool,
        site,
        &[
            ("OAI-SearchBot", cloudflare()),
            ("PerplexityBot", unstable.clone()),
        ],
    )
    .await;
    let still = fetch_blocked_open(&db.pool, site).await;
    assert_eq!(still.len(), 1);
    assert_eq!(still[0].id, open[0].id);
    assert_eq!(still[0].title, open[0].title, "not narrowed");
    assert!(change_kinds(&db.pool, flaky).await.is_empty());

    // Back the next run: not a widening, so no alert.
    let back = probed_crawl(&db.pool, site, &both).await;
    assert!(change_kinds(&db.pool, back).await.is_empty());
    assert_eq!(alert_jobs(&db.pool, back).await, 0);

    // OAI-SearchBot is fine now but PerplexityBot can't be judged: still open, about
    // PerplexityBot.
    let half = probed_crawl(
        &db.pool,
        site,
        &[
            ("OAI-SearchBot", Verdict::Same),
            ("PerplexityBot", unstable),
        ],
    )
    .await;
    let still = fetch_blocked_open(&db.pool, site).await;
    assert_eq!(still.len(), 1, "not resolved");
    assert!(
        still[0].title.contains("PerplexityBot"),
        "{}",
        still[0].title
    );
    assert!(
        !still[0].title.contains("OAI-SearchBot"),
        "{}",
        still[0].title
    );
    assert!(change_kinds(&db.pool, half).await.is_empty());

    // Both judged and fine: resolved.
    probed_crawl(
        &db.pool,
        site,
        &[
            ("OAI-SearchBot", Verdict::Same),
            ("PerplexityBot", Verdict::Same),
        ],
    )
    .await;
    assert!(fetch_blocked_open(&db.pool, site).await.is_empty());
}

// ---- Content in the HTML and llms.txt (B2.10) --------------------------------------------------

use codoseo_core::output::LlmsTxtFetch;

/// Finalizes a new full crawl that allows every bot, whose pages are an empty app shell when
/// `shell`, and whose llms.txt fetch is `llms` (adopted, when it failed).
async fn content_crawl(
    pool: &PgPool,
    site: Uuid,
    shell: bool,
    llms: Option<(LlmsTxtFetch, bool)>,
) -> Uuid {
    content_crawl_with(
        pool,
        site,
        |out| {
            if shell {
                for p in &mut out.pages {
                    p.fields.word_count = 0;
                    p.fields.raw.mount_empty = true;
                }
            }
        },
        llms,
    )
    .await
}

/// [`content_crawl`] with its pages shaped by `pages`.
async fn content_crawl_with(
    pool: &PgPool,
    site: Uuid,
    pages: impl Fn(&mut CrawlOutput),
    llms: Option<(LlmsTxtFetch, bool)>,
) -> Uuid {
    let crawl = make_crawl(pool, site, "manual", "running").await;
    let mut out = crawl_of(200, ALLOW_ALL, None);
    pages(&mut out);
    out.signals.llms_txt = llms.as_ref().map(|(f, _)| f.clone());
    let mut input = input_for(&out);
    if let (Some(report), Some((_, adopted))) = (input.report.llms_txt.as_mut(), llms.as_ref()) {
        report.adopted = report.adopted || *adopted;
    }
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

fn llms_file() -> LlmsTxtFetch {
    LlmsTxtFetch {
        status: 200,
        content_type: Some("text/plain".to_owned()),
        body: "# Example\n\n> A site.\n\n## Docs\n\n- [Home](https://example.com/)\n".to_owned(),
        ..LlmsTxtFetch::default()
    }
}

fn llms_down() -> LlmsTxtFetch {
    LlmsTxtFetch {
        status: 503,
        ..LlmsTxtFetch::default()
    }
}

async fn open_of(pool: &PgPool, site: Uuid, kind: FindingKind) -> Vec<geo::Incident> {
    geo::open_incidents(pool, site)
        .await
        .unwrap()
        .into_iter()
        .filter(|i| i.kind == kind)
        .collect()
}

#[tokio::test]
async fn content_missing_opens_quietly_and_a_later_regression_is_announced() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;

    // The first report that judges it is the baseline: a quiet incident, no change, no alert.
    let first = content_crawl(&db.pool, site, true, None).await;
    let open = open_of(&db.pool, site, FindingKind::ContentMissing).await;
    assert_eq!(open.len(), 1);
    assert!(open[0].quiet);
    assert!(quiet_of(&db.pool, site, "content_missing").await);
    assert!(change_kinds(&db.pool, first).await.is_empty());
    assert_eq!(alert_jobs(&db.pool, first).await, 0);

    // Fixed, then broken again: the regression is a change row of its own kind, and instant.
    content_crawl(&db.pool, site, false, None).await;
    assert!(
        open_of(&db.pool, site, FindingKind::ContentMissing)
            .await
            .is_empty()
    );
    let again = content_crawl(&db.pool, site, true, None).await;
    let changes = change_kinds(&db.pool, again).await;
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].0, "ai_content_missing");
    let (before, after): (String, String) =
        sqlx::query_as("SELECT before_value, after_value FROM changes WHERE crawl_id = $1")
            .bind(again)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(before, "content in the HTML");
    assert_eq!(
        after,
        open_of(&db.pool, site, FindingKind::ContentMissing).await[0].title
    );
    assert_eq!(alert_jobs(&db.pool, again).await, 1);
}

#[tokio::test]
async fn an_adopted_llms_txt_that_fails_opens_quietly_first_then_announces_digest_only() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;

    // Its first judged report is a baseline, even when it already fails.
    let first = content_crawl(&db.pool, site, false, Some((llms_down(), true))).await;
    let open = open_of(&db.pool, site, FindingKind::LlmsTxtUnavailable).await;
    assert_eq!(open.len(), 1);
    assert!(open[0].quiet);
    assert!(change_kinds(&db.pool, first).await.is_empty());

    // Served again, then failing again: announced as its own kind, but not an instant alert.
    content_crawl(&db.pool, site, false, Some((llms_file(), false))).await;
    assert!(
        open_of(&db.pool, site, FindingKind::LlmsTxtUnavailable)
            .await
            .is_empty()
    );
    let down = content_crawl(&db.pool, site, false, Some((llms_down(), true))).await;
    let changes = change_kinds(&db.pool, down).await;
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].0, "ai_llms_txt_unavailable");
    assert_ne!(changes[0].1, "critical");
    let (before, after): (String, String) =
        sqlx::query_as("SELECT before_value, after_value FROM changes WHERE crawl_id = $1")
            .bind(down)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(before, "llms.txt served");
    assert_eq!(
        after,
        open_of(&db.pool, site, FindingKind::LlmsTxtUnavailable).await[0].title
    );
    // The planner job is queued for any change; whether it alerts instantly or waits for the
    // digest is the alert rules' decision (it is digest-only: not in DEFAULT_INSTANT).
    assert!(!DEFAULT_INSTANT.contains(&codoseo_core::change::ChangeKind::AiLlmsTxtUnavailable));

    // Served again: the recovery is announced as the same digest-only kind, not as an instant
    // ai_issue_resolved.
    let back = content_crawl(&db.pool, site, false, Some((llms_file(), false))).await;
    let changes = change_kinds(&db.pool, back).await;
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].0, "ai_llms_txt_unavailable");
    let after: String = sqlx::query_scalar("SELECT after_value FROM changes WHERE crawl_id = $1")
        .bind(back)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(after, "resolved");
}

#[tokio::test]
async fn shell_pages_that_fail_one_crawl_keep_content_missing_open_without_a_change() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    content_crawl(&db.pool, site, true, None).await;
    let open = open_of(&db.pool, site, FindingKind::ContentMissing).await;
    assert_eq!(open.len(), 1);

    // The home page is fixed; the other shells answer 503 this time, so they are not judged.
    let failed = content_crawl_with(
        &db.pool,
        site,
        |out| {
            for p in out.pages.iter_mut().skip(1) {
                p.status = 503;
            }
        },
        None,
    )
    .await;
    let still = open_of(&db.pool, site, FindingKind::ContentMissing).await;
    assert_eq!(still.len(), 1, "held, not resolved");
    assert_eq!(still[0].id, open[0].id);
    assert!(change_kinds(&db.pool, failed).await.is_empty());

    // Judged and fine: resolved.
    content_crawl(&db.pool, site, false, None).await;
    assert!(
        open_of(&db.pool, site, FindingKind::ContentMissing)
            .await
            .is_empty()
    );
}

const BLOCK_OAI_TXT: &str = "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /\n";

/// A site with one open incident.
async fn site_with_incident(pool: &PgPool) -> (Uuid, i64) {
    let site = make_site(pool, "example.com").await;
    full_crawl(pool, site, 200, ALLOW_ALL, None).await;
    full_crawl(pool, site, 200, BLOCK_OAI_TXT, None).await;
    let open = geo::open_incidents(pool, site).await.unwrap();
    assert_eq!(open.len(), 1);
    (site, open[0].id)
}

#[tokio::test]
async fn a_new_incident_has_none_of_the_repair_fields() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    // Only `before` is written when it opens: what the earlier crawl saw.
    assert!(i.before.is_some());
    assert_eq!(i.accepted_at, None);
    assert_eq!(i.accepted_by, None);
    assert_eq!(i.snoozed_until, None);
    assert_eq!(i.recovered_at, None);
    assert_eq!(i.verified_at, None);
    assert_eq!(i.resolved_by, None);
}

#[tokio::test]
async fn the_repair_fields_of_an_incident_round_trip() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let account: Uuid = sqlx::query_scalar(
        "INSERT INTO accounts (email, email_canonical) VALUES ('o@example.com', 'o@example.com') \
         RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE ai_incidents SET before = '{\"rule\": \"Disallow: /\"}', accepted_at = now(), \
         accepted_by = $2, snoozed_until = now() + interval '1 day', recovered_at = now(), \
         verified_at = now(), resolved_by = 'recheck' WHERE id = $1",
    )
    .bind(id)
    .bind(account)
    .execute(&db.pool)
    .await
    .unwrap();
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!(i.before, Some(serde_json::json!({"rule": "Disallow: /"})));
    assert!(i.accepted_at.is_some());
    assert_eq!(i.accepted_by, Some(account));
    assert!(i.snoozed_until.is_some());
    assert!(i.recovered_at.is_some());
    assert!(i.verified_at.is_some());
    assert_eq!(i.resolved_by.as_deref(), Some("recheck"));
    // The listings read the same columns.
    let open = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(open[0].resolved_by.as_deref(), Some("recheck"));

    // Deleting the account clears the reference but keeps the incident.
    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(account)
        .execute(&db.pool)
        .await
        .unwrap();
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!(i.accepted_by, None);
    assert!(i.accepted_at.is_some());
}

#[tokio::test]
async fn resolved_by_only_takes_the_three_known_values() {
    let db = TestDb::new().await;
    let (_, id) = site_with_incident(&db.pool).await;
    for ok in ["crawl", "recheck", "intent"] {
        sqlx::query("UPDATE ai_incidents SET resolved_by = $2 WHERE id = $1")
            .bind(id)
            .bind(ok)
            .execute(&db.pool)
            .await
            .unwrap();
    }
    let bad = sqlx::query("UPDATE ai_incidents SET resolved_by = 'magic' WHERE id = $1")
        .bind(id)
        .execute(&db.pool)
        .await;
    assert!(bad.is_err());
}

async fn insert_recheck(pool: &PgPool, site: Uuid, incident: i64) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO ai_rechecks (site_id, incident_id, origin, due_at) \
         VALUES ($1, $2, 'followup', now() + interval '10 minutes') RETURNING id",
    )
    .bind(site)
    .bind(incident)
    .fetch_one(pool)
    .await
}

#[tokio::test]
async fn a_recheck_row_loads_with_its_defaults_and_values() {
    use geo::{RecheckOrigin, RecheckOutcome, RecheckStatus};

    let db = TestDb::new().await;
    let (site, incident) = site_with_incident(&db.pool).await;
    let id = insert_recheck(&db.pool, site, incident).await.unwrap();
    let r = geo::recheck(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!((r.site_id, r.incident_id), (site, incident));
    assert_eq!(r.origin, RecheckOrigin::Followup);
    assert_eq!(r.status, RecheckStatus::Queued);
    assert_eq!(
        (r.requested_by, r.outcome, r.observations),
        (None, None, None)
    );
    assert!(r.started_at.is_none() && r.finished_at.is_none());
    assert!(r.due_at > r.created_at);
    assert!(
        geo::recheck(&db.pool, Uuid::new_v4(), id)
            .await
            .unwrap()
            .is_none()
    );

    sqlx::query(
        "UPDATE ai_rechecks SET status = 'done', outcome = 'still_failing', \
         observations = '{\"probes\": []}', started_at = now(), finished_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    let r = geo::recheck(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!(r.status, RecheckStatus::Done);
    assert_eq!(r.outcome, Some(RecheckOutcome::StillFailing));
    assert_eq!(r.observations, Some(serde_json::json!({"probes": []})));
}

#[tokio::test]
async fn only_one_recheck_per_incident_is_queued_or_running() {
    let db = TestDb::new().await;
    let (site, incident) = site_with_incident(&db.pool).await;
    let first = insert_recheck(&db.pool, site, incident).await.unwrap();
    assert!(insert_recheck(&db.pool, site, incident).await.is_err());
    sqlx::query("UPDATE ai_rechecks SET status = 'running' WHERE id = $1")
        .bind(first)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(insert_recheck(&db.pool, site, incident).await.is_err());
    // Once it is over, another can be queued.
    sqlx::query("UPDATE ai_rechecks SET status = 'superseded' WHERE id = $1")
        .bind(first)
        .execute(&db.pool)
        .await
        .unwrap();
    insert_recheck(&db.pool, site, incident).await.unwrap();
}

#[tokio::test]
async fn recheck_values_are_checked_and_go_with_their_incident_and_site() {
    let db = TestDb::new().await;
    let (site, incident) = site_with_incident(&db.pool).await;
    for (column, bad) in [
        ("origin", "robot"),
        ("status", "waiting"),
        ("outcome", "fine"),
    ] {
        let id = insert_recheck(&db.pool, site, incident).await;
        let id = match id {
            Ok(id) => id,
            Err(_) => {
                sqlx::query("DELETE FROM ai_rechecks")
                    .execute(&db.pool)
                    .await
                    .unwrap();
                insert_recheck(&db.pool, site, incident).await.unwrap()
            }
        };
        let bad = sqlx::query(&format!(
            "UPDATE ai_rechecks SET {column} = $2 WHERE id = $1"
        ))
        .bind(id)
        .bind(bad)
        .execute(&db.pool)
        .await;
        assert!(bad.is_err(), "{column} accepted a bad value");
    }
    sqlx::query("DELETE FROM ai_incidents WHERE id = $1")
        .bind(incident)
        .execute(&db.pool)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM ai_rechecks")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "deleting the incident deletes its rechecks");

    let (site2, incident2) = {
        let site = make_site(&db.pool, "other.example").await;
        full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
        full_crawl(&db.pool, site, 200, BLOCK_OAI_TXT, None).await;
        (
            site,
            geo::open_incidents(&db.pool, site).await.unwrap()[0].id,
        )
    };
    insert_recheck(&db.pool, site2, incident2).await.unwrap();
    sqlx::query("DELETE FROM sites WHERE id = $1")
        .bind(site2)
        .execute(&db.pool)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM ai_rechecks")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "deleting the site deletes its rechecks");
}

// ---- Accept and snooze (B3.6) ------------------------------------------------------------------

use codoseo_store::geo::IncidentActionError;
use time::{Duration as TimeDuration, OffsetDateTime};

async fn make_account(pool: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO accounts (email, email_canonical) VALUES ($1, $1) RETURNING id")
        .bind(email)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The crawl's change rows as (kind, alerted).
async fn changes_alerted(pool: &PgPool, crawl: Uuid) -> Vec<(String, bool)> {
    sqlx::query_as(
        "SELECT kind::text, alerted_at IS NOT NULL FROM changes WHERE crawl_id = $1 ORDER BY id",
    )
    .bind(crawl)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// A site whose one open incident is a quiet `fetch_blocked` for OAI-SearchBot.
async fn site_with_fetch_blocked(pool: &PgPool) -> (Uuid, i64) {
    let site = make_site(pool, "example.com").await;
    probed_crawl(pool, site, &[("OAI-SearchBot", cloudflare())]).await;
    let open = fetch_blocked_open(pool, site).await;
    assert_eq!(open.len(), 1);
    (site, open[0].id)
}

#[tokio::test]
async fn an_accepted_incident_stays_listed_but_is_not_counted_and_writes_no_changes() {
    let db = TestDb::new().await;
    let (site, id) = site_with_fetch_blocked(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    assert_eq!(geo::open_counts(&db.pool, site).await.unwrap(), (1, 0));

    geo::accept(&db.pool, site, id, owner).await.unwrap();
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert!(i.accepted_at.is_some());
    assert_eq!(i.accepted_by, Some(owner));
    let accepted_at = i.accepted_at;
    // Accepting again keeps the first time.
    geo::accept(&db.pool, site, id, owner).await.unwrap();
    assert_eq!(
        geo::incident(&db.pool, site, id)
            .await
            .unwrap()
            .unwrap()
            .accepted_at,
        accepted_at
    );
    assert_eq!(geo::open_incidents(&db.pool, site).await.unwrap().len(), 1);
    assert_eq!(geo::open_counts(&db.pool, site).await.unwrap(), (0, 0));

    // Widened: same incident, no change row, no alert job, still quiet.
    let widened = probed_crawl(
        &db.pool,
        site,
        &[
            ("OAI-SearchBot", cloudflare()),
            ("Claude-SearchBot", cloudflare()),
        ],
    )
    .await;
    let open = fetch_blocked_open(&db.pool, site).await;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, id);
    assert!(open[0].evidence["probes"].as_array().unwrap().len() == 2);
    assert!(change_kinds(&db.pool, widened).await.is_empty());
    assert_eq!(alert_jobs(&db.pool, widened).await, 0);

    // Resolved by a crawl: resolved quietly, by the crawl.
    let clear = probed_crawl(
        &db.pool,
        site,
        &[
            ("OAI-SearchBot", Verdict::Same),
            ("Claude-SearchBot", Verdict::Same),
        ],
    )
    .await;
    assert!(fetch_blocked_open(&db.pool, site).await.is_empty());
    assert!(change_kinds(&db.pool, clear).await.is_empty());
    let r = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!(r.resolution.as_deref(), Some("fixed"));
    assert_eq!(r.resolved_by.as_deref(), Some("crawl"));
}

#[tokio::test]
async fn unaccepting_makes_the_incident_count_and_announce_again() {
    let db = TestDb::new().await;
    let (site, id) = site_with_fetch_blocked(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    geo::accept(&db.pool, site, id, owner).await.unwrap();
    geo::unaccept(&db.pool, site, id).await.unwrap();
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!((i.accepted_at, i.accepted_by), (None, None));
    assert_eq!(geo::open_counts(&db.pool, site).await.unwrap(), (1, 0));
    let widened = probed_crawl(
        &db.pool,
        site,
        &[
            ("OAI-SearchBot", cloudflare()),
            ("Claude-SearchBot", cloudflare()),
        ],
    )
    .await;
    assert_eq!(
        changes_alerted(&db.pool, widened).await,
        vec![("ai_fetch_blocked".to_owned(), false)]
    );
}

#[tokio::test]
async fn only_kinds_without_an_intent_path_can_be_accepted() {
    let db = TestDb::new().await;
    let owner = make_account(&db.pool, "o@example.com").await;
    // bots_blocked: "Mark intended" is its path.
    let (site, id) = site_with_incident(&db.pool).await;
    assert!(matches!(
        geo::accept(&db.pool, site, id, owner).await,
        Err(IncidentActionError::NotAcceptable(FindingKind::BotsBlocked))
    ));
    // robots_unavailable: snooze only.
    let crawl = full_crawl(&db.pool, site, 503, "", None).await;
    let robots = open_of(&db.pool, site, FindingKind::RobotsUnavailable).await;
    assert_eq!(robots.len(), 1, "{crawl}");
    assert!(matches!(
        geo::accept(&db.pool, site, robots[0].id, owner).await,
        Err(IncidentActionError::NotAcceptable(
            FindingKind::RobotsUnavailable
        ))
    ));
    let until = OffsetDateTime::now_utc() + TimeDuration::days(2);
    geo::snooze(&db.pool, site, robots[0].id, until)
        .await
        .unwrap();
    assert!(
        geo::incident(&db.pool, site, robots[0].id)
            .await
            .unwrap()
            .unwrap()
            .accepted_at
            .is_none()
    );
    // Another site's id, an unknown id.
    let other = make_site(&db.pool, "other.example").await;
    assert!(matches!(
        geo::snooze(&db.pool, other, robots[0].id, until).await,
        Err(IncidentActionError::NotFound)
    ));
    assert!(matches!(
        geo::unaccept(&db.pool, site, 999_999).await,
        Err(IncidentActionError::NotFound)
    ));
}

#[tokio::test]
async fn a_resolved_incident_can_be_neither_accepted_nor_snoozed() {
    let db = TestDb::new().await;
    let (site, id) = site_with_fetch_blocked(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    probed_crawl(&db.pool, site, &[("OAI-SearchBot", Verdict::Same)]).await;
    let until = OffsetDateTime::now_utc() + TimeDuration::days(2);
    assert!(matches!(
        geo::accept(&db.pool, site, id, owner).await,
        Err(IncidentActionError::Resolved)
    ));
    assert!(matches!(
        geo::snooze(&db.pool, site, id, until).await,
        Err(IncidentActionError::Resolved)
    ));
    assert!(matches!(
        geo::unsnooze(&db.pool, site, id).await,
        Err(IncidentActionError::Resolved)
    ));
}

#[tokio::test]
async fn a_snooze_must_end_in_the_future_and_lasts_at_most_90_days() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let past = OffsetDateTime::now_utc() - TimeDuration::minutes(1);
    assert!(matches!(
        geo::snooze(&db.pool, site, id, past).await,
        Err(IncidentActionError::SnoozeInPast)
    ));
    let far = OffsetDateTime::now_utc() + TimeDuration::days(400);
    let until = geo::snooze(&db.pool, site, id, far).await.unwrap();
    assert!(until <= OffsetDateTime::now_utc() + TimeDuration::days(90));
    assert!(until > OffsetDateTime::now_utc() + TimeDuration::days(89));
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!(i.snoozed_until, Some(until));
    assert_eq!(geo::open_counts(&db.pool, site).await.unwrap(), (0, 0));

    geo::unsnooze(&db.pool, site, id).await.unwrap();
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!(i.snoozed_until, None);
    assert_eq!(geo::open_counts(&db.pool, site).await.unwrap(), (1, 1));
}

#[tokio::test]
async fn a_snoozed_incident_still_writes_its_changes_but_alerts_nobody() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    let below_home = "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /a\n";
    full_crawl(&db.pool, site, 200, below_home, None).await;
    let id = geo::open_incidents(&db.pool, site).await.unwrap()[0].id;
    let until = OffsetDateTime::now_utc() + TimeDuration::days(7);
    geo::snooze(&db.pool, site, id, until).await.unwrap();
    assert_eq!(geo::open_counts(&db.pool, site).await.unwrap(), (0, 0));

    // Escalated while snoozed: the change is on record (changes page, digest) but already
    // marked alerted, so no instant alert goes out.
    let worse = full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    assert_eq!(
        changes_alerted(&db.pool, worse).await,
        vec![("ai_bot_blocked".to_owned(), true)]
    );
    assert!(
        alert_rules::unalerted_changes(&db.pool, worse)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(alert_jobs(&db.pool, worse).await, 0);
    assert!(
        !quiet_of(&db.pool, site, "bots_blocked").await,
        "announced on the record"
    );

    // Resolved while snoozed: the same.
    let fixed = full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    assert_eq!(
        changes_alerted(&db.pool, fixed).await,
        vec![("ai_issue_resolved".to_owned(), true)]
    );
    assert_eq!(alert_jobs(&db.pool, fixed).await, 0);
}

#[tokio::test]
async fn an_expired_snooze_alerts_again() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    let below_home = "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /a\n";
    full_crawl(&db.pool, site, 200, below_home, None).await;
    let id = geo::open_incidents(&db.pool, site).await.unwrap()[0].id;
    geo::snooze(
        &db.pool,
        site,
        id,
        OffsetDateTime::now_utc() + TimeDuration::days(1),
    )
    .await
    .unwrap();
    // Time passes.
    sqlx::query(
        "UPDATE ai_incidents SET snoozed_until = now() - interval '1 minute' WHERE id = $1",
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    assert_eq!(geo::open_counts(&db.pool, site).await.unwrap(), (1, 0));
    let worse = full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    assert_eq!(
        changes_alerted(&db.pool, worse).await,
        vec![("ai_bot_blocked".to_owned(), false)]
    );
    assert_eq!(
        alert_rules::unalerted_changes(&db.pool, worse)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(alert_jobs(&db.pool, worse).await, 1);
}

#[tokio::test]
async fn a_crawl_that_still_finds_the_problem_clears_its_recovery() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    sqlx::query("UPDATE ai_incidents SET recovered_at = now() WHERE id = $1")
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    full_crawl(&db.pool, site, 200, BLOCK_OAI_TXT, None).await;
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!(i.recovered_at, None);
    assert!(i.resolved_at.is_none());
}

#[tokio::test]
async fn resolved_by_says_whether_a_crawl_or_the_intent_resolved_it() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!(i.resolved_by.as_deref(), Some("crawl"));

    full_crawl(&db.pool, site, 200, BLOCK_OAI_TXT, None).await;
    let id = geo::open_incidents(&db.pool, site).await.unwrap()[0].id;
    let intent = Intent {
        bots: [("OAI-SearchBot".to_owned(), Stance::Any)].into(),
        ..Intent::default()
    };
    geo::set_intent(&db.pool, site, &intent).await.unwrap();
    geo::reevaluate_quietly(&db.pool, site).await.unwrap();
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!(i.resolution.as_deref(), Some("intent"));
    assert_eq!(i.resolved_by.as_deref(), Some("intent"));
}

// ---- Rechecks (B3.8) ---------------------------------------------------------------------------

use codoseo_core::plan::{Plan, PlanLimits};
use codoseo_geo::recheck::{self as rc, Observations};
use codoseo_geo::report::AccessReport;
use codoseo_store::geo::{RecheckError, RecheckOrigin, RecheckOutcome, RecheckStatus};

fn limits_with(cap: Option<u16>) -> PlanLimits {
    PlanLimits {
        ai_rechecks_per_day: cap,
        ..PlanLimits::for_plan(Plan::Pro)
    }
}

/// What a recheck of the incident would conclude if robots.txt now read `robots_body`: the
/// stored report with it laid over, and the kinds it judged.
async fn rechecked_robots(
    pool: &PgPool,
    site: Uuid,
    incident: i64,
    robots_body: &str,
) -> (AccessReport, Vec<FindingKind>) {
    let i = geo::incident(pool, site, incident).await.unwrap().unwrap();
    let stored = geo::latest_report(pool, site)
        .await
        .unwrap()
        .unwrap()
        .report;
    let ev = serde_json::from_value(i.evidence).unwrap();
    let plan = rc::plan(i.kind, &i.subject, &ev, &stored, 3);
    let obs = Observations {
        robots: Some(RobotsFile {
            status: 200,
            body: robots_body.to_owned(),
            hash: 3,
        }),
        ..Observations::default()
    };
    rc::overlay(&stored, &obs, &plan)
}

/// The site's rechecks of the incident, oldest first.
async fn rechecks_of(pool: &PgPool, site: Uuid, incident: i64) -> Vec<geo::Recheck> {
    let ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM ai_rechecks WHERE incident_id = $1 ORDER BY id")
            .bind(incident)
            .fetch_all(pool)
            .await
            .unwrap();
    let mut out = Vec::new();
    for id in ids {
        out.push(geo::recheck(pool, site, id).await.unwrap().unwrap());
    }
    out
}

/// The `ai_recheck` jobs as (recheck_id, run_after, max_attempts).
async fn recheck_jobs(pool: &PgPool) -> Vec<(i64, OffsetDateTime, i16)> {
    sqlx::query_as(
        "SELECT (payload->>'recheck_id')::bigint, run_after, max_attempts FROM jobs \
         WHERE kind = 'ai_recheck' ORDER BY run_after",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Asks for a manual recheck and applies a clean robots.txt to it: the incident recovers and a
/// follow-up is queued. Returns the follow-up's id.
async fn recover(pool: &PgPool, site: Uuid, incident: i64, owner: Uuid) -> i64 {
    let queued = geo::request_recheck(pool, site, incident, owner, &limits_with(None))
        .await
        .unwrap();
    geo::start_recheck(pool, queued.id).await.unwrap().unwrap();
    let (report, kinds) = rechecked_robots(pool, site, incident, ALLOW_ALL).await;
    let outcome = geo::apply_recheck(pool, queued.id, &report, &kinds, None)
        .await
        .unwrap();
    assert_eq!(outcome, Some(RecheckOutcome::Clean));
    let all = rechecks_of(pool, site, incident).await;
    let followup = all.last().unwrap();
    assert_eq!(followup.origin, RecheckOrigin::Followup);
    assert_eq!(followup.status, RecheckStatus::Queued);
    followup.id
}

#[tokio::test]
async fn a_recheck_is_queued_once_with_its_job() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    let queued = geo::request_recheck(&db.pool, site, id, owner, &limits_with(Some(3)))
        .await
        .unwrap();
    let r = geo::recheck(&db.pool, site, queued.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.origin, RecheckOrigin::Manual);
    assert_eq!(r.status, RecheckStatus::Queued);
    assert_eq!(r.requested_by, Some(owner));
    assert_eq!(r.due_at, queued.due_at);
    assert_eq!(
        recheck_jobs(&db.pool).await,
        vec![(queued.id, queued.due_at, 1)]
    );

    let started = geo::start_recheck(&db.pool, queued.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(started.status, RecheckStatus::Running);
    assert!(started.started_at.is_some());
    assert!(
        geo::start_recheck(&db.pool, queued.id)
            .await
            .unwrap()
            .is_none(),
        "only a queued recheck starts"
    );
}

#[tokio::test]
async fn a_repair_is_resolved_in_two_steps_by_rechecks() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    let opened_by = geo::incident(&db.pool, site, id)
        .await
        .unwrap()
        .unwrap()
        .opened_crawl_id
        .unwrap();
    let alerts_before = alert_jobs(&db.pool, opened_by).await;

    // Step one: the manual recheck sees it fixed. Recovered, not resolved.
    let followup = recover(&db.pool, site, id, owner).await;
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert!(i.recovered_at.is_some());
    assert!(i.resolved_at.is_none());
    let manual = &rechecks_of(&db.pool, site, id).await[0];
    assert_eq!(manual.status, RecheckStatus::Done);
    assert_eq!(manual.outcome, Some(RecheckOutcome::Clean));
    assert!(manual.finished_at.is_some());
    // The follow-up waits for OpenAI's 24 hours.
    let f = geo::recheck(&db.pool, site, followup)
        .await
        .unwrap()
        .unwrap();
    let wait = f.due_at - f.created_at;
    assert!(
        wait >= TimeDuration::hours(24) && wait < TimeDuration::hours(25),
        "{wait}"
    );
    assert_eq!(f.requested_by, None);
    let jobs = recheck_jobs(&db.pool).await;
    assert_eq!(jobs.last().unwrap(), &(followup, f.due_at, 1));
    // Nothing announced yet.
    assert_eq!(change_kinds(&db.pool, opened_by).await.len(), 1);

    // Step two: the follow-up still sees it fixed. Resolved by the recheck, and announced.
    let (report, kinds) = rechecked_robots(&db.pool, site, id, ALLOW_ALL).await;
    let outcome = geo::apply_recheck(&db.pool, followup, &report, &kinds, None)
        .await
        .unwrap();
    assert_eq!(outcome, Some(RecheckOutcome::Clean));
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert!(i.resolved_at.is_some());
    assert!(i.verified_at.is_some());
    assert_eq!(i.resolution.as_deref(), Some("fixed"));
    assert_eq!(i.resolved_by.as_deref(), Some("recheck"));
    assert_eq!(
        changes_alerted(&db.pool, opened_by).await,
        vec![
            ("ai_bot_blocked".to_owned(), false),
            ("ai_issue_resolved".to_owned(), false)
        ]
    );
    assert_eq!(alert_jobs(&db.pool, opened_by).await, alerts_before + 1);
    let f = geo::recheck(&db.pool, site, followup)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (f.status, f.outcome),
        (RecheckStatus::Done, Some(RecheckOutcome::Clean))
    );
    assert!(
        geo::open_incidents(&db.pool, site)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_crawl_that_still_finds_it_makes_the_follow_up_a_no_op() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    let followup = recover(&db.pool, site, id, owner).await;

    full_crawl(&db.pool, site, 200, BLOCK_OAI_TXT, None).await;
    assert_eq!(
        geo::incident(&db.pool, site, id)
            .await
            .unwrap()
            .unwrap()
            .recovered_at,
        None
    );
    let (report, kinds) = rechecked_robots(&db.pool, site, id, ALLOW_ALL).await;
    let outcome = geo::apply_recheck(&db.pool, followup, &report, &kinds, None)
        .await
        .unwrap();
    assert_eq!(outcome, None);
    let f = geo::recheck(&db.pool, site, followup)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f.status, RecheckStatus::Superseded);
    assert_eq!(f.outcome, None);
    assert!(f.finished_at.is_some());
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert!(i.resolved_at.is_none());

    // And a follow-up for an incident a crawl resolved meanwhile is a no-op too.
    sqlx::query("UPDATE ai_rechecks SET created_at = now() - interval '1 hour'")
        .execute(&db.pool)
        .await
        .unwrap();
    let followup = recover(&db.pool, site, id, owner).await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    let (report, kinds) = rechecked_robots(&db.pool, site, id, ALLOW_ALL).await;
    assert_eq!(
        geo::apply_recheck(&db.pool, followup, &report, &kinds, None)
            .await
            .unwrap(),
        None
    );
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!(i.resolved_by.as_deref(), Some("crawl"));
}

#[tokio::test]
async fn a_recheck_that_still_sees_it_refreshes_the_incident_but_keeps_its_crawl() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    let (crawl_before, seen_before) = last_seen(&db.pool, site, "bots_blocked").await;
    sqlx::query("UPDATE ai_incidents SET recovered_at = now() WHERE id = $1")
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    let queued = geo::request_recheck(&db.pool, site, id, owner, &limits_with(None))
        .await
        .unwrap();
    let (report, kinds) = rechecked_robots(&db.pool, site, id, BLOCK_OAI_TXT).await;
    let outcome = geo::apply_recheck(
        &db.pool,
        queued.id,
        &report,
        &kinds,
        Some(&serde_json::json!({"robots": 200})),
    )
    .await
    .unwrap();
    assert_eq!(outcome, Some(RecheckOutcome::StillFailing));
    let (crawl_after, seen_after) = last_seen(&db.pool, site, "bots_blocked").await;
    assert_eq!(crawl_after, crawl_before);
    assert!(seen_after > seen_before);
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!(i.recovered_at, None);
    let r = geo::recheck(&db.pool, site, queued.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.observations, Some(serde_json::json!({"robots": 200})));
    // No follow-up for something still failing.
    assert_eq!(rechecks_of(&db.pool, site, id).await.len(), 1);
}

#[tokio::test]
async fn an_inconclusive_recheck_changes_nothing() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    let before = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    let queued = geo::request_recheck(&db.pool, site, id, owner, &limits_with(None))
        .await
        .unwrap();
    let (report, _) = rechecked_robots(&db.pool, site, id, ALLOW_ALL).await;
    let outcome = geo::apply_recheck(&db.pool, queued.id, &report, &[], None)
        .await
        .unwrap();
    assert_eq!(outcome, Some(RecheckOutcome::Inconclusive));
    let after = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!(after.last_seen_at, before.last_seen_at);
    assert_eq!(after.recovered_at, None);
    assert!(after.resolved_at.is_none());
    assert_eq!(rechecks_of(&db.pool, site, id).await.len(), 1);
}

#[tokio::test]
async fn a_recheck_never_opens_an_incident() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    let queued = geo::request_recheck(&db.pool, site, id, owner, &limits_with(None))
        .await
        .unwrap();
    // robots.txt now also blocks ChatGPT-User: a user_fetch finding no incident has.
    let both = format!("{BLOCK_OAI_TXT}\nUser-agent: ChatGPT-User\nDisallow: /\n");
    let (report, _) = rechecked_robots(&db.pool, site, id, &both).await;
    let found = codoseo_geo::findings::findings(&report, &Intent::default());
    assert!(
        found
            .iter()
            .any(|f| f.kind == FindingKind::BotsBlocked && f.subject == "user_fetch")
    );
    let outcome = geo::apply_recheck(
        &db.pool,
        queued.id,
        &report,
        &[FindingKind::BotsBlocked, FindingKind::RobotsUnavailable],
        None,
    )
    .await
    .unwrap();
    assert_eq!(outcome, Some(RecheckOutcome::StillFailing));
    let open = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, id);
}

#[tokio::test]
async fn a_quiet_incidents_verified_resolution_writes_no_change() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, BLOCK_OAI_TXT, None).await;
    let id = geo::open_incidents(&db.pool, site).await.unwrap()[0].id;
    assert!(quiet_of(&db.pool, site, "bots_blocked").await);
    let owner = make_account(&db.pool, "o@example.com").await;
    let followup = recover(&db.pool, site, id, owner).await;
    let (report, kinds) = rechecked_robots(&db.pool, site, id, ALLOW_ALL).await;
    geo::apply_recheck(&db.pool, followup, &report, &kinds, None)
        .await
        .unwrap();
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!(i.resolved_by.as_deref(), Some("recheck"));
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM changes WHERE site_id = $1")
        .bind(site)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(total, 0);
}

#[tokio::test]
async fn a_snoozed_incidents_verified_resolution_is_recorded_already_alerted() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    let opened_by = geo::incident(&db.pool, site, id)
        .await
        .unwrap()
        .unwrap()
        .opened_crawl_id
        .unwrap();
    let alerts_before = alert_jobs(&db.pool, opened_by).await;
    geo::snooze(
        &db.pool,
        site,
        id,
        OffsetDateTime::now_utc() + TimeDuration::days(3),
    )
    .await
    .unwrap();
    let followup = recover(&db.pool, site, id, owner).await;
    let (report, kinds) = rechecked_robots(&db.pool, site, id, ALLOW_ALL).await;
    geo::apply_recheck(&db.pool, followup, &report, &kinds, None)
        .await
        .unwrap();
    assert_eq!(
        changes_alerted(&db.pool, opened_by).await,
        vec![
            ("ai_bot_blocked".to_owned(), false),
            ("ai_issue_resolved".to_owned(), true)
        ]
    );
    assert_eq!(alert_jobs(&db.pool, opened_by).await, alerts_before);
}

#[tokio::test]
async fn a_recheck_is_refused_while_busy_cooling_down_crawling_or_over_the_cap() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    let one = limits_with(Some(1));

    // Unknown, another site's, resolved.
    let other = make_site(&db.pool, "other.example").await;
    assert!(matches!(
        geo::request_recheck(&db.pool, other, id, owner, &one).await,
        Err(RecheckError::NotFound)
    ));

    let first = geo::request_recheck(&db.pool, site, id, owner, &one)
        .await
        .unwrap();
    assert!(matches!(
        geo::request_recheck(&db.pool, site, id, owner, &limits_with(None)).await,
        Err(RecheckError::Busy)
    ));
    // Done, but asked for a minute ago.
    sqlx::query(
        "UPDATE ai_rechecks SET status = 'done', created_at = now() - interval '1 minute' \
         WHERE id = $1",
    )
    .bind(first.id)
    .execute(&db.pool)
    .await
    .unwrap();
    match geo::request_recheck(&db.pool, site, id, owner, &limits_with(None)).await {
        Err(RecheckError::Cooldown(until)) => {
            let left = until - OffsetDateTime::now_utc();
            assert!(
                left > TimeDuration::minutes(8) && left <= TimeDuration::minutes(9),
                "{left}"
            );
        }
        other => panic!("{other:?}"),
    }
    // Eleven minutes on: no cooldown, but one a day is used up until a day after the first.
    sqlx::query("UPDATE ai_rechecks SET created_at = now() - interval '11 minutes' WHERE id = $1")
        .bind(first.id)
        .execute(&db.pool)
        .await
        .unwrap();
    match geo::request_recheck(&db.pool, site, id, owner, &one).await {
        Err(RecheckError::LimitReached(Some(frees_at))) => {
            let left = frees_at - OffsetDateTime::now_utc();
            assert!(
                left > TimeDuration::hours(23) && left < TimeDuration::hours(24),
                "{left}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        geo::request_recheck(&db.pool, site, id, owner, &limits_with(Some(0))).await,
        Err(RecheckError::LimitReached(None))
    ));
    geo::request_recheck(&db.pool, site, id, owner, &limits_with(Some(2)))
        .await
        .unwrap();
    sqlx::query("UPDATE ai_rechecks SET status = 'done', created_at = now() - interval '1 hour'")
        .execute(&db.pool)
        .await
        .unwrap();

    // A crawl queued for the site: it will tell soon enough.
    let crawl = make_crawl(&db.pool, site, "manual", "queued").await;
    assert!(matches!(
        geo::request_recheck(&db.pool, site, id, owner, &limits_with(None)).await,
        Err(RecheckError::CrawlActive)
    ));
    sqlx::query("UPDATE crawls SET status = 'failed' WHERE id = $1")
        .bind(crawl)
        .execute(&db.pool)
        .await
        .unwrap();

    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    assert!(matches!(
        geo::request_recheck(&db.pool, site, id, owner, &limits_with(None)).await,
        Err(RecheckError::Resolved)
    ));
}

#[tokio::test]
async fn follow_ups_do_not_count_toward_the_daily_cap() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    for _ in 0..3 {
        sqlx::query(
            "INSERT INTO ai_rechecks (site_id, incident_id, origin, status, due_at, created_at) \
             VALUES ($1, $2, 'followup', 'done', now(), now() - interval '1 hour')",
        )
        .bind(site)
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    // Manual ones older than a day don't count either.
    sqlx::query(
        "INSERT INTO ai_rechecks (site_id, incident_id, origin, status, due_at, created_at) \
         VALUES ($1, $2, 'manual', 'done', now(), now() - interval '25 hours')",
    )
    .bind(site)
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    geo::request_recheck(&db.pool, site, id, owner, &limits_with(Some(1)))
        .await
        .unwrap();
}

#[tokio::test]
async fn superseded_and_failed_rechecks_do_not_use_the_daily_allowance() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    // A crawl took one over, another never ran on our side: neither costs the owner a slot.
    for status in ["superseded", "failed"] {
        sqlx::query(
            "INSERT INTO ai_rechecks (site_id, incident_id, origin, status, due_at, created_at) \
             VALUES ($1, $2, 'manual', $3, now(), now() - interval '1 hour')",
        )
        .bind(site)
        .bind(id)
        .bind(status)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    geo::request_recheck(&db.pool, site, id, owner, &limits_with(Some(1)))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE ai_rechecks SET created_at = now() - interval '11 minutes' WHERE status = 'queued'",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE ai_rechecks SET status = 'done' WHERE status = 'queued'")
        .execute(&db.pool)
        .await
        .unwrap();
    // The one that ran does count.
    assert!(matches!(
        geo::request_recheck(&db.pool, site, id, owner, &limits_with(Some(1))).await,
        Err(RecheckError::LimitReached(Some(_)))
    ));
}

#[tokio::test]
async fn a_superseded_recheck_still_cools_down() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    let queued = geo::request_recheck(&db.pool, site, id, owner, &limits_with(Some(5)))
        .await
        .unwrap();
    assert!(geo::supersede_recheck(&db.pool, queued.id).await.unwrap());
    assert!(matches!(
        geo::request_recheck(&db.pool, site, id, owner, &limits_with(Some(5))).await,
        Err(RecheckError::Cooldown(_))
    ));
}

#[tokio::test]
async fn the_latest_recheck_of_each_open_incident_is_listed() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    assert!(
        geo::latest_rechecks(&db.pool, site)
            .await
            .unwrap()
            .is_empty()
    );
    let first = geo::request_recheck(&db.pool, site, id, owner, &limits_with(None))
        .await
        .unwrap();
    geo::fail_recheck(&db.pool, first.id).await.unwrap();
    sqlx::query("UPDATE ai_rechecks SET created_at = now() - interval '11 minutes'")
        .execute(&db.pool)
        .await
        .unwrap();
    let second = geo::request_recheck(&db.pool, site, id, owner, &limits_with(None))
        .await
        .unwrap();
    let latest = geo::latest_rechecks(&db.pool, site).await.unwrap();
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[&id].id, second.id);
    assert_eq!(latest[&id].status, RecheckStatus::Queued);
    // Another site sees none of them.
    let other = make_site(&db.pool, "other.example").await;
    assert!(
        geo::latest_rechecks(&db.pool, other)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn the_daily_cap_counts_every_site_of_the_account() {
    let db = TestDb::new().await;
    let owner = make_account(&db.pool, "o@example.com").await;
    let (a, ia) = site_with_incident(&db.pool).await;
    let b = make_site(&db.pool, "other.example").await;
    full_crawl(&db.pool, b, 200, ALLOW_ALL, None).await;
    full_crawl(&db.pool, b, 200, BLOCK_OAI_TXT, None).await;
    let ib = geo::open_incidents(&db.pool, b).await.unwrap()[0].id;
    sqlx::query("UPDATE sites SET account_id = $1")
        .bind(owner)
        .execute(&db.pool)
        .await
        .unwrap();
    geo::request_recheck(&db.pool, a, ia, owner, &limits_with(Some(1)))
        .await
        .unwrap();
    assert!(matches!(
        geo::request_recheck(&db.pool, b, ib, owner, &limits_with(Some(1))).await,
        Err(RecheckError::LimitReached(Some(_)))
    ));
}

#[tokio::test]
async fn a_failed_recheck_is_marked_and_frees_the_incident() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    let queued = geo::request_recheck(&db.pool, site, id, owner, &limits_with(None))
        .await
        .unwrap();
    geo::start_recheck(&db.pool, queued.id).await.unwrap();
    assert!(geo::fail_recheck(&db.pool, queued.id).await.unwrap());
    let r = geo::recheck(&db.pool, site, queued.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.status, RecheckStatus::Failed);
    assert!(r.finished_at.is_some());
    assert!(!geo::fail_recheck(&db.pool, queued.id).await.unwrap());
    // Not busy any more (the cooldown still applies).
    assert!(matches!(
        geo::request_recheck(&db.pool, site, id, owner, &limits_with(None)).await,
        Err(RecheckError::Cooldown(_))
    ));
}

#[tokio::test]
async fn an_inconclusive_follow_up_queues_the_next_six_hours_on_three_times_in_a_row() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    let mut followup = recover(&db.pool, site, id, owner).await;
    let (report, _) = rechecked_robots(&db.pool, site, id, ALLOW_ALL).await;

    for round in 1..=geo::MAX_INCONCLUSIVE_FOLLOWUPS {
        geo::start_recheck(&db.pool, followup)
            .await
            .unwrap()
            .unwrap();
        let outcome = geo::apply_recheck(&db.pool, followup, &report, &[], None)
            .await
            .unwrap();
        assert_eq!(outcome, Some(RecheckOutcome::Inconclusive));
        let all = rechecks_of(&db.pool, site, id).await;
        let last = all.last().unwrap();
        if round < geo::MAX_INCONCLUSIVE_FOLLOWUPS {
            assert_ne!(last.id, followup, "round {round} queues another");
            assert_eq!(last.origin, RecheckOrigin::Followup);
            assert_eq!(last.status, RecheckStatus::Queued);
            let wait = last.due_at - last.created_at;
            assert!(
                wait >= TimeDuration::hours(6) && wait < TimeDuration::hours(7),
                "{wait}"
            );
            assert!(
                recheck_jobs(&db.pool)
                    .await
                    .contains(&(last.id, last.due_at, 1))
            );
            followup = last.id;
        } else {
            assert_eq!(last.id, followup, "the third in a row queues none");
        }
    }
    // One manual, three follow-ups; the incident stays recovered for the next crawl.
    assert_eq!(rechecks_of(&db.pool, site, id).await.len(), 4);
    let i = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert!(i.recovered_at.is_some());
    assert!(i.resolved_at.is_none());
}

#[tokio::test]
async fn a_superseded_recheck_is_finished_and_frees_the_incident() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    let queued = geo::request_recheck(&db.pool, site, id, owner, &limits_with(None))
        .await
        .unwrap();
    geo::start_recheck(&db.pool, queued.id)
        .await
        .unwrap()
        .unwrap();
    assert!(geo::supersede_recheck(&db.pool, queued.id).await.unwrap());
    assert!(!geo::supersede_recheck(&db.pool, queued.id).await.unwrap());
    let r = geo::recheck(&db.pool, site, queued.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.status, RecheckStatus::Superseded);
    assert_eq!(r.outcome, None);
    assert!(r.finished_at.is_some());
}

#[tokio::test]
async fn rechecks_nothing_will_finish_are_failed() {
    let db = TestDb::new().await;
    let (site, id) = site_with_incident(&db.pool).await;
    let owner = make_account(&db.pool, "o@example.com").await;
    let ten_minutes = std::time::Duration::from_secs(600);

    // Queued with its job queued: left alone.
    let queued = geo::request_recheck(&db.pool, site, id, owner, &limits_with(None))
        .await
        .unwrap();
    assert_eq!(
        geo::fail_orphaned_rechecks(&db.pool, ten_minutes)
            .await
            .unwrap(),
        0
    );
    // Running, its job running, started a minute ago: left alone.
    geo::start_recheck(&db.pool, queued.id)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE jobs SET status = 'running', claimed_at = now() WHERE kind = 'ai_recheck'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        geo::fail_orphaned_rechecks(&db.pool, ten_minutes)
            .await
            .unwrap(),
        0
    );
    // Its worker died: the job failed (one attempt) and the row would stay running forever.
    sqlx::query("UPDATE jobs SET status = 'failed' WHERE kind = 'ai_recheck'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        geo::fail_orphaned_rechecks(&db.pool, ten_minutes)
            .await
            .unwrap(),
        1
    );
    let r = geo::recheck(&db.pool, site, queued.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.status, RecheckStatus::Failed);
    assert!(r.finished_at.is_some());

    // Running far past the runner's time limit, whatever its job says.
    sqlx::query("UPDATE ai_rechecks SET created_at = now() - interval '1 hour'")
        .execute(&db.pool)
        .await
        .unwrap();
    let second = geo::request_recheck(&db.pool, site, id, owner, &limits_with(None))
        .await
        .unwrap();
    geo::start_recheck(&db.pool, second.id)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE ai_rechecks SET started_at = now() - interval '11 minutes' WHERE id = $1")
        .bind(second.id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        geo::fail_orphaned_rechecks(&db.pool, ten_minutes)
            .await
            .unwrap(),
        1
    );
    // Not busy any more.
    sqlx::query("UPDATE ai_rechecks SET created_at = now() - interval '1 hour'")
        .execute(&db.pool)
        .await
        .unwrap();
    geo::request_recheck(&db.pool, site, id, owner, &limits_with(None))
        .await
        .unwrap();
}

// ---- B3 review ---------------------------------------------------------------------------------

/// Applies a manual recheck that read `robots_body` and returns its outcome.
async fn manual_recheck(
    pool: &PgPool,
    site: Uuid,
    incident: i64,
    owner: Uuid,
    robots_body: &str,
) -> Option<RecheckOutcome> {
    sqlx::query("UPDATE ai_rechecks SET created_at = now() - interval '1 hour'")
        .execute(pool)
        .await
        .unwrap();
    let queued = geo::request_recheck(pool, site, incident, owner, &limits_with(None))
        .await
        .unwrap();
    geo::start_recheck(pool, queued.id).await.unwrap().unwrap();
    let (report, kinds) = rechecked_robots(pool, site, incident, robots_body).await;
    geo::apply_recheck(pool, queued.id, &report, &kinds, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_recheck_that_sees_it_worse_leaves_the_escalation_to_the_next_crawl() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    let owner = make_account(&db.pool, "o@example.com").await;
    let below_home = "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /a\n";
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    full_crawl(&db.pool, site, 200, below_home, None).await;
    let before = geo::open_incidents(&db.pool, site).await.unwrap();
    assert_eq!(before.len(), 1);
    let id = before[0].id;

    // The recheck sees the whole site blocked now: still failing, but it changes neither the
    // stored severity nor the evidence.
    let outcome = manual_recheck(&db.pool, site, id, owner, BLOCK_OAI).await;
    assert_eq!(outcome, Some(RecheckOutcome::StillFailing));
    let after = geo::incident(&db.pool, site, id).await.unwrap().unwrap();
    assert_eq!(after.severity, before[0].severity);
    assert_eq!(after.evidence, before[0].evidence);
    assert_eq!(after.title, before[0].title);

    // The next crawl sees it worse and announces it.
    let worse = full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    assert_eq!(
        changes_alerted(&db.pool, worse).await,
        vec![("ai_bot_blocked".to_owned(), false)]
    );
    assert_eq!(alert_jobs(&db.pool, worse).await, 1);
}
