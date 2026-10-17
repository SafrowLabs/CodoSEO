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
    // Seven score cards: the SEO score with its checks, indexable share and average response
    // as count-ups; no GEO score without an AI access report, so Overall and GEO read "—".
    assert!(body.contains(&format!(r#"data-count="{score}""#)), "{body}");
    assert!(
        body.contains(&format!("{passed} of {total} checks pass")),
        "{body}"
    );
    assert!(body.contains(r#"data-count="75.0""#), "{body}");
    assert!(body.contains("3 of 4 URLs"), "{body}");
    assert!(body.contains(r#"data-count="120""#), "{body}");
    assert!(body.contains("First crawl"), "{body}");
    assert_eq!(
        body.matches("<b>—</b>").count(),
        3,
        "Overall, GEO, AI products: {body}"
    );
    assert!(body.contains("Needs a GEO score"), "{body}");
    assert!(body.contains("No AI check yet"), "{body}");
    // The crawl's number, size, duration and rate, and the next scheduled crawl.
    assert!(body.contains("Crawl #1 · finished"), "{body}");
    assert!(body.contains("4 URLs"), "{body}");
    assert!(body.contains("URL/s"), "{body}");
    // Open issues: every failing SEO check, a link down to the fix list.
    let failing = total - passed;
    assert!(
        body.contains(&format!(r#"data-count="{failing}">{failing}</b>"#)),
        "{body}"
    );
    assert!(body.contains("See the fix list ↓"), "{body}");
    // Both actions sit in the page head.
    assert!(
        body.contains(&format!(
            r#"<button class="btn btn-primary" hx-post="/s/{}/crawls""#,
            site.id
        )),
        "{body}"
    );
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
    assert!(body.contains("Under 200 ms"), "{body}");
    assert!(body.contains("500 ms–1 s"), "{body}");
    assert!(body.contains("Issues by area"), "{body}");
    // Each row has its fix behind a "?" and a coding-agent prompt to copy.
    assert!(
        body.contains(r#"popovertarget="fx-seo-http_4xx-fix""#),
        "{body}"
    );
    assert!(body.contains("Copy all fix prompts"), "{body}");
    // The 404's example URL is a chip behind "1 page".
    assert!(
        body.contains(r#"aria-controls="fx-seo-http_4xx-urls""#),
        "{body}"
    );
    assert!(
        body.contains(r#"title="https://example.com/gone">/gone</span>"#),
        "{body}"
    );
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

/// What the worker's heartbeat stores on a running crawl: the counters and the newest pages.
fn live_progress(pages_done: u32, newest: &[(u32, u16, &str)]) -> serde_json::Value {
    let recent: Vec<_> = newest
        .iter()
        .map(|&(n, status, path)| {
            serde_json::json!({"n": n, "url": format!("https://{DOMAIN}{path}"), "status": status})
        })
        .collect();
    serde_json::json!({
        "pages_done": pages_done, "queued": 30, "failures": 0, "depth": 3, "elapsed_ms": 42_000,
        "errors": 1, "recent": recent,
    })
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

    // Queued: the live audit, waiting for a crawler and polling itself.
    let body = app
        .get(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await
        .body;
    assert!(body.contains("Waiting for a free crawler"), "{body}");
    assert!(
        body.contains(&format!(r#"hx-get="/s/{}/audit/watch?f=all""#, site.id)),
        "{body}"
    );
    assert!(body.contains("data-quiet"), "{body}");

    // Running with progress: the live fragment shows the counters and the newest pages,
    // newest first, and asks for what is new since the newest one on the next poll.
    sqlx::query(
        "UPDATE crawls SET status = 'running', started_at = now(), worker_id = 'w', \
         progress = $2 WHERE id = $1",
    )
    .bind(crawl_id)
    .bind(live_progress(
        142,
        &[
            (142, 404, "/gone"),
            (141, 200, "/about"),
            (140, 301, "/old"),
        ],
    ))
    .execute(app.pool())
    .await
    .unwrap();
    let live = format!("/s/{}/audit/live", site.id);
    let res = app.get_hx(&live, Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(!res.body.contains("<html"), "{}", res.body);
    assert!(res.body.contains("Crawling example.com"), "{}", res.body);
    for counter in [
        "<dt>Pages crawled</dt><dd class=\"num \">142</dd>",
        "<dt>Queued</dt><dd class=\"num \">30</dd>",
        "<dt>Errors</dt><dd class=\"num c-err\">1</dd>",
        "<dt>Elapsed</dt><dd class=\"num \">42s</dd>",
    ] {
        assert!(res.body.contains(counter), "{counter}: {}", res.body);
    }
    let gone = res.body.find(">/gone<").expect("the newest page");
    let about = res.body.find(">/about<").expect("an older page");
    assert!(gone < about, "newest first");
    assert!(res.body.contains(r#"<span class="code t-err">404</span>"#));
    assert!(res.body.contains(r#"<span class="code t-warn">301</span>"#));
    assert!(
        res.body.contains(&format!(r#"hx-get="{live}?seen=142""#)),
        "{}",
        res.body
    );
    assert!(res.body.contains(r#"hx-trigger="every 2s""#));
    assert!(
        !res.body.contains("is-new"),
        "a first render animates nothing"
    );

    // The next poll: the pages after the one it already shows fade in.
    sqlx::query("UPDATE crawls SET progress = $2 WHERE id = $1")
        .bind(crawl_id)
        .bind(live_progress(
            144,
            &[(144, 200, "/a"), (143, 200, "/b"), (142, 404, "/gone")],
        ))
        .execute(app.pool())
        .await
        .unwrap();
    let res = app.get_hx(&format!("{live}?seen=142"), Some(&cookie)).await;
    assert_eq!(res.body.matches("is-new").count(), 2, "{}", res.body);
    assert!(res.body.contains(&format!(r#"hx-get="{live}?seen=144""#)));

    // The crawl finishes: the fragment stops polling and loads the audit in its place, which
    // is now the finished audit, with no live panel left.
    app.finalize_crawl(
        crawl_id,
        fixture_pages(),
        Vec::new(),
        codoseo_core::output::StopReason::Completed,
    )
    .await;
    let res = app.get_hx(&live, Some(&cookie)).await;
    assert!(!res.body.contains("every 2s"), "{}", res.body);
    assert!(
        res.body
            .contains(&format!(r#"hx-get="/s/{}/audit""#, site.id)),
        "{}",
        res.body
    );
    assert!(res.body.contains(r##"hx-target="#audit-body""##));
    let res = app
        .get_hx(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await;
    assert!(res.body.contains("Fix these, in order"), "{}", res.body);
    assert!(!res.body.contains("live-crawl"), "{}", res.body);
}

#[tokio::test]
async fn audit_live_fragment_stops_polling_once_the_crawl_failed() {
    let (app, _, cookie, site) = setup().await;
    sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, started_at, worker_id, progress) \
         VALUES ($1, $2, 'first', 1, 'running', now(), 'w', $3)",
    )
    .bind(site.id)
    .bind(DOMAIN)
    .bind(live_progress(3, &[(3, 200, "/")]))
    .execute(app.pool())
    .await
    .unwrap();
    let live = format!("/s/{}/audit/live", site.id);
    assert!(
        app.get_hx(&live, Some(&cookie))
            .await
            .body
            .contains("every 2s")
    );

    sqlx::query("UPDATE crawls SET status = 'failed', finished_at = now(), failure_reason = 'x'")
        .execute(app.pool())
        .await
        .unwrap();
    let res = app.get_hx(&live, Some(&cookie)).await;
    assert!(!res.body.contains("every 2s"), "{}", res.body);
}

#[tokio::test]
async fn the_rerun_banner_carries_the_counters() {
    let (app, _, cookie, site) = setup().await;
    app.finished_crawl(&site, fixture_pages(), Vec::new()).await;
    sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, started_at, worker_id, progress) \
         VALUES ($1, $2, 'manual', 1, 'running', now(), 'w', $3)",
    )
    .bind(site.id)
    .bind(DOMAIN)
    .bind(live_progress(142, &[(142, 404, "/gone")]))
    .execute(app.pool())
    .await
    .unwrap();
    let res = app
        .get_hx(
            &format!("/s/{}/audit/live?view=banner", site.id),
            Some(&cookie),
        )
        .await;
    assert!(
        res.body
            .contains("Crawl #2 running · 142 pages · 30 queued · 1 error · 42s"),
        "{}",
        res.body
    );
    assert!(!res.body.contains("live-pages"), "the banner stays slim");
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
async fn audit_compares_with_the_previous_crawl() {
    let (app, _, cookie, site) = setup().await;
    let first = app
        .finished_crawl(&site, vec![page(DOMAIN, "/"), page(DOMAIN, "/a")], vec![])
        .await;
    let second = app
        .finished_crawl(
            &site,
            fixture_pages(),
            vec![change(
                ChangeKind::NewUrl,
                Severity::Notice,
                Some("/gone"),
                "",
                "404",
            )],
        )
        .await;
    let (a, ..) = crawl_row(&app, first).await;
    let (b, ..) = crawl_row(&app, second).await;
    let body = app
        .get(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await
        .body;
    let expected = match b.cmp(&a) {
        std::cmp::Ordering::Less => format!(r#"<span class="down">▼ {}</span>"#, a - b),
        std::cmp::Ordering::Greater => format!(r#"<span class="up">▲ {}</span>"#, b - a),
        std::cmp::Ordering::Equal => "No change".to_owned(),
    };
    assert!(body.contains(&expected), "{expected}\n{body}");
    // Indexable pages: 2 then 3.
    assert!(
        body.contains(r#"<span class="up">▲ 1 page</span>"#),
        "{body}"
    );
    // The 404 and the missing title were not there on the first crawl: new this crawl.
    let row = |id: &str| {
        let at = body.find(&format!(r#"id="{id}""#)).unwrap();
        body[at..at + body[at..].find("Copy fix prompt").unwrap()].to_owned()
    };
    assert!(row("fx-seo-http_4xx").contains("New this crawl"));
    assert!(row("fx-seo-title_missing").contains("New this crawl"));
    // Both crawls lack a sitemap: not new.
    assert!(!row("fx-seo-sitemap_missing").contains("New this crawl"));
}

#[tokio::test]
async fn fixed_checks_are_counted_in_the_footer() {
    let (app, _, cookie, site) = setup().await;
    app.finished_crawl(&site, fixture_pages(), vec![]).await;
    app.finished_crawl(&site, vec![page(DOMAIN, "/"), page(DOMAIN, "/a")], vec![])
        .await;
    let body = app
        .get(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await
        .body;
    let at = body
        .find(" fixed since last crawl:")
        .expect("the fixed line");
    let line = &body[at..at + body[at..].find("</span></span>").unwrap()];
    assert!(line.contains("Page returns a 4xx error"), "{line}");
    assert!(line.contains("Title is missing"), "{line}");
    assert!(
        !line.contains("Site has no sitemap"),
        "still failing: {line}"
    );
    assert!(!body.contains("New this crawl"), "{body}");
}

#[tokio::test]
async fn one_crawl_without_an_ai_report_shows_honest_empty_states() {
    let (app, _, cookie, site) = setup().await;
    app.finished_crawl(&site, fixture_pages(), vec![]).await;
    let body = app
        .get(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await
        .body;
    // No AI access report yet: the GEO parts say when they come, nothing breaks.
    assert!(body.contains("Comes with the next AI check"), "{body}");
    assert!(
        body.contains("The table appears with the next crawl"),
        "{body}"
    );
    assert!(!body.contains(r#"data-kind="ai""#), "{body}");
    // One crawl: the trend waits for the next, and nothing is new or fixed yet.
    assert!(
        body.contains("The trend appears after your second crawl"),
        "{body}"
    );
    assert!(body.contains("No crawl is scheduled"), "{body}");
    assert!(!body.contains("<polyline"), "{body}");
    assert!(!body.contains("New this crawl"), "{body}");
    assert!(!body.contains("fixed since last crawl"), "{body}");
    assert!(body.contains("no crawl scheduled"), "{body}");
    // The fix list: the critical 404 first, the sitemap as a site-wide notice.
    let fixes = body.find("Fix these, in order").unwrap();
    let first = body[fixes..].find("Page returns a 4xx error").unwrap();
    let title = body[fixes..].find("Title is missing").unwrap();
    assert!(first < title, "critical before warning");
    assert!(body.contains(" SEO</span>"), "{body}");
    assert!(body.contains("Issues by area"), "{body}");
    // Passing facts and the count of passing checks close the list.
    assert!(body.contains("✓ 0 server errors"), "{body}");
    assert!(
        !body.contains("✓ 0 broken links"),
        "/ links to /gone: {body}"
    );
    assert!(body.contains(" passing checks"), "{body}");
}

#[tokio::test]
async fn many_crawls_draw_the_trend_with_a_pin_for_a_serious_change() {
    let (app, _, cookie, site) = setup().await;
    for _ in 0..3 {
        app.finished_crawl(&site, vec![page(DOMAIN, "/"), page(DOMAIN, "/a")], vec![])
            .await;
    }
    app.finished_crawl(
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
                ChangeKind::BecameNoindex,
                Severity::Critical,
                Some("/"),
                "Indexable",
                "Noindex",
            ),
        ],
    )
    .await;
    // The scheduler's next slot shows under the title.
    sqlx::query(
        "UPDATE sites SET monitoring_active = true, next_crawl_at = '2031-03-04 06:00:00+00' \
         WHERE id = $1",
    )
    .bind(site.id)
    .execute(app.pool())
    .await
    .unwrap();
    let body = app
        .get(&format!("/s/{}/audit", site.id), Some(&cookie))
        .await
        .body;
    assert!(body.contains("next crawl Mar 4 2031, 06:00 UTC"), "{body}");
    assert!(body.contains("tr-line tr-seo"), "{body}");
    // Crawls without a GEO score draw no GEO or Overall line.
    assert!(!body.contains("tr-line tr-geo"), "{body}");
    assert!(body.contains("Last 4 crawls"), "{body}");
    assert!(body.contains("The SEO score"), "{body}");
    // The noindex change pins crawl #4, with what changed and what it did.
    assert!(body.contains(r#"popovertarget="pin-1""#), "{body}");
    let at = body.find(r#"id="pin-1""#).unwrap();
    let pin = &body[at..at + body[at..].find("</div>").unwrap()];
    assert!(pin.contains("crawl #4"), "{pin}");
    assert!(pin.contains("Became noindex"), "{pin}");
    assert!(pin.contains("SEO "), "{pin}");
    assert!(body.contains("Today"), "{body}");
}

#[tokio::test]
async fn a_failed_crawl_after_a_finished_one_shows_a_banner_over_its_results() {
    let (app, _, cookie, site) = setup().await;
    app.finished_crawl(&site, fixture_pages(), vec![]).await;
    sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, finished_at, failure_reason) \
         VALUES ($1, $2, 'manual', 2, 'failed', now(), 'The server answered 503 for every page.')",
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
    let at = body
        .find(r#"class="au-failed" role="alert""#)
        .expect("the banner");
    let banner = &body[at..at + body[at..].find("</div>").unwrap()];
    assert!(banner.contains("Crawl #2 failed"), "{banner}");
    assert!(banner.contains("503 for every page"), "{banner}");
    assert!(
        banner.contains("Showing results from crawl #1 below."),
        "{banner}"
    );
    assert!(banner.contains("Retry crawl"), "{banner}");
    assert!(body.contains("Page returns a 4xx error"), "{body}");
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
        body.contains("href=\"/settings/alerts\">Choose what is sent at once"),
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

#[tokio::test]
async fn the_live_audit_lists_checked_pages_with_their_tags_and_hands_over_when_done() {
    let (app, _, cookie, site) = setup().await;
    let crawl_id: Uuid = sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, started_at, worker_id, progress) \
         VALUES ($1, $2, 'first', 1, 'running', now(), 'w', $3) RETURNING id",
    )
    .bind(site.id)
    .bind(DOMAIN)
    .bind(serde_json::json!({
        "pages_done": 2, "queued": 8, "failures": 0, "depth": 1, "elapsed_ms": 9_000,
        "errors": 1, "phase": "pages",
        "files": {"robots": 200, "sitemap_urls": 40, "llms_txt": 404},
        "seo_estimate": 91,
        "tally": {"checks": [["title_missing", 1]], "with_issues": 1, "ai_blocked": 0,
                  "ai_judged": 1, "response_ms_total": 240, "answered": 2},
    }))
    .fetch_one(app.pool())
    .await
    .unwrap();
    for (n, status, path, issues) in [(1, 200, "/", 1u64 << 12), (2, 404, "/gone", 0)] {
        let row = serde_json::json!({
            "n": n, "url": format!("https://{DOMAIN}{path}"), "status": status,
            "response_ms": 120, "indexability": if status == 200 { "indexable" } else { "client_error" },
            "issues": issues, "ai_blocked": 0, "ai_of": 10,
        });
        sqlx::query("INSERT INTO crawl_live_rows (crawl_id, n, row) VALUES ($1, $2, $3)")
            .bind(crawl_id)
            .bind(n)
            .bind(row)
            .execute(app.pool())
            .await
            .unwrap();
    }

    let watch = format!("/s/{}/audit/watch", site.id);
    let res = app.get(&watch, Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("<html"), "a full page on its own screen");
    for part in [
        "Checking pages",
        "2 / 40",
        "40 URLs",
        "/robots.txt",
        "AI rules read",
        ">/gone<",
        "Indexable</b> · 404",
        "AI can reach</b> · 10/10",
        "Issues so far",
        "91",
    ] {
        assert!(res.body.contains(part), "{part}: {}", res.body);
    }
    let fragment = app.get_hx(&watch, Some(&cookie)).await.body;
    assert!(
        !fragment.contains('—'),
        "no em dash in the live audit: {fragment}"
    );

    // A filter keeps itself on the next poll, and lists only the matching pages.
    let res = app
        .get_hx(&format!("{watch}?f=errors"), Some(&cookie))
        .await;
    assert!(!res.body.contains("<html"));
    assert!(
        res.body.contains(&format!(r#"hx-get="{watch}?f=errors""#)),
        "{}",
        res.body
    );
    assert!(
        res.body.contains(">/gone<") && !res.body.contains(">/<"),
        "{}",
        res.body
    );

    // Done: final scores, then the results in its place.
    app.finalize_crawl(
        crawl_id,
        fixture_pages(),
        Vec::new(),
        codoseo_core::output::StopReason::Completed,
    )
    .await;
    let res = app.get_hx(&watch, Some(&cookie)).await;
    assert!(res.body.contains("Scores final"), "{}", res.body);
    assert!(
        !res.body.contains("every 2s"),
        "a finished crawl stops polling"
    );
    assert!(res.body.contains(r##"hx-target="#audit-body""##));

    // Long after, the screen is just the results.
    sqlx::query("UPDATE crawls SET finished_at = now() - interval '1 hour' WHERE id = $1")
        .bind(crawl_id)
        .execute(app.pool())
        .await
        .unwrap();
    let res = app.get(&watch, Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
}
