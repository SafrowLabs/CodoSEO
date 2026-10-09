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
use codoseo_geo::findings::FindingKind;
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

fn input_for(out: &CrawlOutput, intent: &Intent) -> GeoInput {
    let important = important_urls(&out.pages, &out.origin, &HashSet::new());
    GeoInput::new(build_report(out, &important), intent)
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
    let intent = geo::get_intent(pool, site).await.unwrap();
    let input = input_for(&out, &intent);
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
    let input = input_for(&out, &Intent::default());
    assert_eq!(input.evaluated, vec![FindingKind::RobotsUnavailable]);
    let n = geo::record_failed_crawl(&db.pool, site, crawl, &input)
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

    // The retry fails the same way under the same crawl id: one report, no new change.
    let n = geo::record_failed_crawl(&db.pool, site, crawl, &input)
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

#[tokio::test]
async fn reevaluating_after_a_failed_crawl_keeps_what_that_report_could_not_judge() {
    let db = TestDb::new().await;
    let site = make_site(&db.pool, "example.com").await;
    full_crawl(&db.pool, site, 200, ALLOW_ALL, None).await;
    full_crawl(&db.pool, site, 200, BLOCK_OAI, None).await;
    let failed = make_crawl(&db.pool, site, "manual", "failed").await;
    let mut out = crawl_of(503, "", None);
    out.pages.clear();
    let input = input_for(&out, &Intent::default());
    geo::record_failed_crawl(&db.pool, site, failed, &input)
        .await
        .unwrap();

    // The newest report is the failed crawl's; it can't judge bots_blocked, so an intent change
    // leaves that incident alone.
    geo::set_intent(&db.pool, site, &Intent::default())
        .await
        .unwrap();
    let (opened, resolved) = geo::reevaluate_quietly(&db.pool, site).await.unwrap();
    assert_eq!((opened, resolved), (0, 0));
    assert_eq!(geo::open_incidents(&db.pool, site).await.unwrap().len(), 2);
    let latest = geo::latest_report(&db.pool, site).await.unwrap().unwrap();
    assert_eq!(
        (latest.crawl_id, latest.crawl_status.as_str()),
        (failed, "failed")
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
    let input = input_for(&out, &Intent::default());
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
