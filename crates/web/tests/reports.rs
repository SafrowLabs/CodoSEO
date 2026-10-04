//! T5.5: the site audit and changes screens, and the report queries behind them.

mod support;

use axum::http::StatusCode;
use codoseo_core::change::{Change, ChangeKind};
use codoseo_core::check::Severity;
use codoseo_core::page::{Indexability, PageRecord};
use codoseo_store::accounts::Account;
use codoseo_store::sites::Site;
use support::{TestApp, page};
use url::Url;
use uuid::Uuid;

const DOMAIN: &str = "example.com";

/// Four pages: a healthy home and about page, a 404 and a page with no title.
fn fixture_pages() -> Vec<PageRecord> {
    let mut gone = PageRecord {
        status: 404,
        indexability: Indexability::ClientError,
        ..page(DOMAIN, "/gone")
    };
    gone.key_hash = gone.compute_key_hash();
    let mut untitled = page(DOMAIN, "/untitled");
    untitled.fields.title = None;
    untitled.fields.title_count = 0;
    untitled.key_hash = untitled.compute_key_hash();
    vec![page(DOMAIN, "/"), page(DOMAIN, "/about"), gone, untitled]
}

async fn setup() -> (TestApp, Account, String, Site) {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("owner@example.com").await;
    let site = app.site(&account, DOMAIN).await;
    (app, account, cookie, site)
}

fn change(
    kind: ChangeKind,
    severity: Severity,
    path: Option<&str>,
    before: &str,
    after: &str,
) -> Change {
    Change {
        kind,
        severity,
        url: path.map(|p| Url::parse(&format!("https://{DOMAIN}{p}")).unwrap()),
        before: before.to_owned(),
        after: after.to_owned(),
    }
}

/// One change of each interesting shape, across all three severities.
fn fixture_changes() -> Vec<Change> {
    vec![
        change(
            ChangeKind::RobotsTxtChanged,
            Severity::Critical,
            None,
            "Disallow: /admin",
            "Disallow: /",
        ),
        change(
            ChangeKind::BecameNoindex,
            Severity::Critical,
            Some("/"),
            "Indexable",
            "Noindex",
        ),
        change(
            ChangeKind::StatusChanged,
            Severity::Warning,
            Some("/pricing"),
            "200",
            "404",
        ),
        change(
            ChangeKind::TitleRemoved,
            Severity::Warning,
            Some("/team"),
            "Our team",
            "",
        ),
        change(
            ChangeKind::NewUrl,
            Severity::Notice,
            Some("/new"),
            "",
            "200",
        ),
        change(
            ChangeKind::NewUrl,
            Severity::Notice,
            Some("/newer"),
            "",
            "200",
        ),
        change(
            ChangeKind::RemovedUrl,
            Severity::Notice,
            Some("/old"),
            "200",
            "",
        ),
        change(
            ChangeKind::TitleChanged,
            Severity::Notice,
            Some("/about"),
            "About",
            "About us",
        ),
    ]
}

async fn crawl_row(app: &TestApp, crawl_id: Uuid) -> (i16, i16, i16) {
    sqlx::query_as("SELECT health_score, checks_passed, checks_total FROM crawls WHERE id = $1")
        .bind(crawl_id)
        .fetch_one(app.pool())
        .await
        .unwrap()
}

fn hex(path: &str) -> String {
    let url = Url::parse(&format!("https://{DOMAIN}{path}")).unwrap();
    format!("{:016x}", codoseo_core::url::url_hash(&url))
}

// ── Site audit ───────────────────────────────────────────

#[tokio::test]
async fn audit_renders_score_kpis_and_issues() {
    let (app, _, cookie, site) = setup().await;
    let crawl = app.finished_crawl(&site, fixture_pages(), vec![]).await;
    let (score, passed, total) = crawl_row(&app, crawl).await;

    let res = app
        .get(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let body = &res.body;
    assert!(body.contains("<h1>Site audit</h1>"), "{body}");
    assert!(
        body.contains(&format!("{passed} of {total} checks passed")),
        "{body}"
    );
    // Health score, URLs crawled, indexable share and average response, as count-ups.
    assert!(body.contains(&format!(r#"data-count="{score}""#)), "{body}");
    assert!(body.contains(r#"data-count="4""#), "{body}");
    assert!(body.contains(r#"data-count="75.0""#), "{body}");
    assert!(body.contains("3 of 4"), "{body}");
    assert!(body.contains(r#"data-count="120""#), "{body}");
    assert!(body.contains("— first crawl"), "{body}");
    assert!(body.contains("URL/s"), "{body}");
    // Export link bypasses htmx.
    assert!(
        body.contains(&format!(
            r#"href="/s/{}/export.csv" hx-boost="false" download"#,
            site.id
        )),
        "{body}"
    );

    // The 404 and the missing title are issue rows.
    assert!(body.contains("Page returns a 4xx error"), "{body}");
    assert!(body.contains("Title is missing"), "{body}");
    // Charts: response codes legend, depth bars and response-time buckets.
    assert!(body.contains("Response codes"), "{body}");
    assert!(body.contains("stackbar"), "{body}");
    assert!(body.contains("clicks from homepage"), "{body}");
    assert!(body.contains("&#60; 200 ms"), "{body}");
    assert!(body.contains("500–1000"), "{body}");
}

#[tokio::test]
async fn issue_rows_link_to_explorer_filters() {
    let (app, _, cookie, site) = setup().await;
    app.finished_crawl(&site, fixture_pages(), vec![]).await;
    let body = app
        .get(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await
        .body;

    let base = format!("/s/{}/explorer?filter=check:", site.id);
    assert!(
        body.contains(&format!(r#"href="{base}http_4xx""#)),
        "{body}"
    );
    assert!(
        body.contains(&format!(r#"href="{base}title_missing""#)),
        "{body}"
    );
    // The fixture has no sitemap: a site-wide check, shown without a link.
    assert!(body.contains("Site has no sitemap"), "{body}");
    assert!(body.contains("site-wide"), "{body}");
    assert!(!body.contains(&format!("{base}sitemap_missing")), "{body}");

    // Critical rows come before warnings.
    let critical = body.find("Page returns a 4xx error").unwrap();
    let warning = body.find("Title is missing").unwrap();
    assert!(critical < warning);
}

#[tokio::test]
async fn audit_empty_state_without_crawls() {
    let (app, _, cookie, site) = setup().await;
    let res = app
        .get(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("No crawls yet"), "{}", res.body);
    assert!(
        res.body.contains(&format!(
            r#"hx-post="/s/{}/crawls" hx-swap="none""#,
            site.id
        )),
        "{}",
        res.body
    );
}

#[tokio::test]
async fn audit_shows_first_crawl_progress() {
    let (app, _, cookie, site) = setup().await;
    let crawl_id: Uuid = sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, trigger, priority) VALUES ($1, $2, 'first', 1) RETURNING id",
    )
    .bind(site.id)
    .bind(DOMAIN)
    .fetch_one(app.pool())
    .await
    .unwrap();

    // Queued: skeleton KPIs and a polling live line.
    let body = app
        .get(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await
        .body;
    assert!(body.contains("skel"), "{body}");
    assert!(
        body.contains(&format!(r#"hx-get="/s/{}/audit/live""#, site.id)),
        "{body}"
    );
    assert!(body.contains("data-quiet"), "{body}");
    assert!(body.contains("queued"), "{body}");

    // Running with progress: the live fragment shows pages and elapsed time.
    sqlx::query(
        "UPDATE crawls SET status = 'running', started_at = now(), worker_id = 'w', \
         progress = $2 WHERE id = $1",
    )
    .bind(crawl_id)
    .bind(serde_json::json!({"pages_done": 142, "queued": 30, "failures": 0, "depth": 3, "elapsed_ms": 42_000}))
    .execute(app.pool())
    .await
    .unwrap();
    let res = app
        .get_hx(&format!("/s/{}/audit/live", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(!res.body.contains("<html"), "{}", res.body);
    assert!(
        res.body.contains("Crawling example.com · 142 pages · 42s"),
        "{}",
        res.body
    );
    assert!(res.body.contains("shimmer"), "{}", res.body);
    assert!(
        res.body.contains(r#"hx-trigger="every 2s""#),
        "{}",
        res.body
    );

    // Once the crawl is over the fragment stops polling.
    sqlx::query("UPDATE crawls SET status = 'failed', finished_at = now(), failure_reason = 'x' WHERE id = $1")
        .bind(crawl_id)
        .execute(app.pool())
        .await
        .unwrap();
    let res = app
        .get_hx(&format!("/s/{}/audit/live", site.id), Some(&cookie))
        .await;
    assert!(!res.body.contains("every 2s"), "{}", res.body);
}

#[tokio::test]
async fn audit_failed_state_shows_the_reason() {
    let (app, _, cookie, site) = setup().await;
    sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, finished_at, failure_reason) \
         VALUES ($1, $2, 'first', 1, 'failed', now(), 'Could not reach example.com: connection refused')",
    )
    .bind(site.id)
    .bind(DOMAIN)
    .execute(app.pool())
    .await
    .unwrap();
    let body = app
        .get(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await
        .body;
    assert!(
        body.contains("Could not reach example.com: connection refused"),
        "{body}"
    );
    assert!(
        body.contains(&format!(r#"hx-post="/s/{}/crawls""#, site.id)),
        "{body}"
    );
}

#[tokio::test]
async fn audit_shows_a_banner_while_a_newer_crawl_runs() {
    let (app, _, cookie, site) = setup().await;
    app.finished_crawl(&site, fixture_pages(), vec![]).await;
    sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, started_at, worker_id, progress) \
         VALUES ($1, $2, 'manual', 2, 'running', now(), 'w', $3)",
    )
    .bind(site.id)
    .bind(DOMAIN)
    .bind(serde_json::json!({"pages_done": 142, "queued": 3, "failures": 0, "depth": 2, "elapsed_ms": 9_000}))
    .execute(app.pool())
    .await
    .unwrap();
    let body = app
        .get(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await
        .body;
    assert!(body.contains("Crawl #2 running · 142 pages"), "{body}");
    assert!(body.contains("Page returns a 4xx error"), "{body}");
}

#[tokio::test]
async fn audit_health_delta_against_previous_crawl() {
    let (app, _, cookie, site) = setup().await;
    let first = app
        .finished_crawl(&site, vec![page(DOMAIN, "/"), page(DOMAIN, "/a")], vec![])
        .await;
    let second = app
        .finished_crawl(
            &site,
            fixture_pages(),
            vec![
                change(
                    ChangeKind::NewUrl,
                    Severity::Notice,
                    Some("/gone"),
                    "",
                    "404",
                ),
                change(
                    ChangeKind::NewUrl,
                    Severity::Notice,
                    Some("/untitled"),
                    "",
                    "200",
                ),
                change(
                    ChangeKind::RemovedUrl,
                    Severity::Notice,
                    Some("/a"),
                    "200",
                    "",
                ),
            ],
        )
        .await;
    let (a, ..) = crawl_row(&app, first).await;
    let (b, ..) = crawl_row(&app, second).await;
    let body = app
        .get(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await
        .body;
    assert!(body.contains("+2 new · −1 removed"), "{body}");
    let expected = match b.cmp(&a) {
        std::cmp::Ordering::Less => format!("↓ {} vs crawl #1", a - b),
        std::cmp::Ordering::Greater => format!("↑ {} vs crawl #1", b - a),
        std::cmp::Ordering::Equal => "no change vs crawl #1".to_owned(),
    };
    assert!(body.contains(&expected), "{expected}\n{body}");
}

// ── Changes ──────────────────────────────────────────────

#[tokio::test]
async fn changes_render_the_comparison() {
    let (app, _, cookie, site) = setup().await;
    app.finished_crawl(&site, fixture_pages(), vec![]).await;
    app.finished_crawl(&site, fixture_pages(), fixture_changes())
        .await;

    let res = app
        .get(&format!("/s/{}/changes", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let body = &res.body;
    assert!(body.contains("<h1>Crawl comparison</h1>"), "{body}");
    assert!(body.contains("#1 · "), "{body}");
    assert!(body.contains("#2 · "), "{body}");

    // Tiles: 2 new, 1 removed, 1 status change, 1 noindex, 2 title changes.
    assert!(body.contains(">+2<"), "{body}");
    assert!(body.contains(">−1<"), "{body}");
    assert!(body.contains("Titles changed"), "{body}");

    // Row titles per kind, with the alert mode.
    assert!(body.contains("Status 200 → 404"), "{body}");
    assert!(body.contains("robots.txt changed"), "{body}");
    assert!(body.contains("Became noindex"), "{body}");
    assert!(body.contains("Title removed"), "{body}");
    assert!(body.contains(">New URL<"), "{body}");
    assert!(body.contains(">Removed URL<"), "{body}");
    assert!(body.contains("Instant alert"), "{body}");
    assert!(body.contains("Weekly digest"), "{body}");
    // Diff with severity colouring.
    assert!(body.contains(r#"class="after critical""#), "{body}");
    assert!(body.contains(r#"class="after warning""#), "{body}");
    // On-site URLs link to the explorer selection.
    assert!(
        body.contains(&format!("/s/{}/explorer?sel={}", site.id, hex("/pricing"))),
        "{body}"
    );

    // Right column: the score history and the default alert rules.
    assert!(body.contains("Health score by crawl"), "{body}");
    assert!(body.contains("spark"), "{body}");
    assert!(body.contains("Key page becomes noindex"), "{body}");
    assert!(
        body.contains("Alert channels arrive with monitoring (M7)."),
        "{body}"
    );
}

#[tokio::test]
async fn severity_tabs_filter_changes() {
    let (app, _, cookie, site) = setup().await;
    app.finished_crawl(&site, fixture_pages(), vec![]).await;
    app.finished_crawl(&site, fixture_pages(), fixture_changes())
        .await;
    let base = format!("/s/{}/changes", site.id);

    let all = app.get(&base, Some(&cookie)).await.body;
    // Tab counts: all 8, critical 2, warning 2, notice 4.
    for (label, n) in [("All", 8), ("Critical", 2), ("Warning", 2), ("Notice", 4)] {
        assert!(
            all.contains(&format!(r#"{label}<span class="n">{n}</span>"#)),
            "{label} {n}\n{all}"
        );
    }

    let critical = app
        .get(&format!("{base}?sev=critical"), Some(&cookie))
        .await
        .body;
    assert!(critical.contains("robots.txt changed"), "{critical}");
    assert!(critical.contains("Became noindex"), "{critical}");
    assert!(!critical.contains("Status 200 → 404"), "{critical}");
    assert!(!critical.contains(">New URL<"), "{critical}");
    assert_eq!(
        critical.matches(r#"class="row-item change""#).count(),
        2,
        "{critical}"
    );

    let warning = app
        .get(&format!("{base}?sev=warning"), Some(&cookie))
        .await
        .body;
    assert!(warning.contains("Status 200 → 404"), "{warning}");
    assert!(warning.contains("Title removed"), "{warning}");
    assert!(!warning.contains("robots.txt changed"), "{warning}");
    assert_eq!(
        warning.matches(r#"class="row-item change""#).count(),
        2,
        "{warning}"
    );

    let notice = app
        .get(&format!("{base}?sev=notice"), Some(&cookie))
        .await
        .body;
    assert!(notice.contains("Title changed"), "{notice}");
    assert!(!notice.contains("Title removed"), "{notice}");
    assert_eq!(
        notice.matches(r#"class="row-item change""#).count(),
        4,
        "{notice}"
    );

    // The active tab is marked.
    assert!(
        notice.contains(r#"class="tab is-active" href="/s/"#),
        "{notice}"
    );
    // Unknown values fall back to all.
    let bogus = app
        .get(&format!("{base}?sev=bogus"), Some(&cookie))
        .await
        .body;
    assert_eq!(
        bogus.matches(r#"class="row-item change""#).count(),
        8,
        "{bogus}"
    );
}

#[tokio::test]
async fn changes_empty_states() {
    let (app, _, cookie, site) = setup().await;
    let url = format!("/s/{}/changes", site.id);

    let none = app.get(&url, Some(&cookie)).await;
    assert_eq!(none.status, StatusCode::OK);
    assert!(none.body.contains("No finished crawl yet"), "{}", none.body);

    app.finished_crawl(&site, fixture_pages(), vec![]).await;
    let one = app.get(&url, Some(&cookie)).await.body;
    assert!(
        one.contains("Changes appear after your second crawl."),
        "{one}"
    );

    app.finished_crawl(&site, fixture_pages(), vec![]).await;
    let same = app.get(&url, Some(&cookie)).await.body;
    assert!(
        same.contains("Nothing changed between #1 and #2."),
        "{same}"
    );
}

// ── Access and queries ───────────────────────────────────

#[tokio::test]
async fn other_accounts_sites_are_not_found() {
    let (app, _, _, site) = setup().await;
    app.finished_crawl(&site, fixture_pages(), vec![]).await;
    let (_, intruder) = app.login("intruder@example.com").await;
    for path in ["audit", "audit/live", "changes"] {
        let res = app
            .get(&format!("/s/{}/{path}", site.id), Some(&intruder))
            .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{path}");
    }
    let res = app.get("/s/not-a-uuid/audit", Some(&intruder)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn report_queries() {
    let (app, _, _, site) = setup().await;
    let timed = |path: &str, ms: u32| PageRecord {
        response_ms: ms,
        ..page(DOMAIN, path)
    };
    let mut failed = timed("/timeout", 30_000);
    failed.status = 0;
    failed.indexability = Indexability::ServerError;
    let first = app
        .finished_crawl(&site, vec![page(DOMAIN, "/")], vec![])
        .await;
    let second = app
        .finished_crawl(
            &site,
            vec![
                timed("/", 120),
                timed("/b", 350),
                timed("/c", 700),
                timed("/d", 1500),
                failed,
            ],
            fixture_changes(),
        )
        .await;
    let pool = app.pool();

    let buckets = codoseo_store::reports::response_time_buckets(pool, second)
        .await
        .unwrap();
    assert_eq!(
        (buckets.fast, buckets.ok, buckets.slow, buckets.very_slow),
        (1, 1, 1, 1)
    );

    let history = codoseo_store::reports::health_history(pool, site.id, 14)
        .await
        .unwrap();
    assert_eq!(
        history.iter().map(|h| h.number).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(history[0].score, crawl_row(&app, first).await.0);
    let latest_only = codoseo_store::reports::health_history(pool, site.id, 1)
        .await
        .unwrap();
    assert_eq!(latest_only.len(), 1);
    assert_eq!(latest_only[0].number, 2);

    let counts = codoseo_store::reports::change_kind_counts(pool, second)
        .await
        .unwrap();
    assert_eq!(counts.total(), 8);
    assert_eq!(counts.kind(ChangeKind::NewUrl), 2);
    assert_eq!(counts.severity(Severity::Critical), 2);

    let rows = codoseo_store::reports::changes_for_crawl(pool, second, None, 500)
        .await
        .unwrap();
    assert_eq!(rows.len(), 8);
    assert_eq!(rows[0].kind, ChangeKind::RobotsTxtChanged);
    assert_eq!(rows[0].url, None);
    let warnings =
        codoseo_store::reports::changes_for_crawl(pool, second, Some(Severity::Warning), 500)
            .await
            .unwrap();
    assert!(warnings.iter().all(|r| r.severity == Severity::Warning));
    assert_eq!(warnings.len(), 2);
    let capped = codoseo_store::reports::changes_for_crawl(pool, second, None, 3)
        .await
        .unwrap();
    assert_eq!(capped.len(), 3);
}
