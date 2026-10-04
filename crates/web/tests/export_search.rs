//! T5.6: the CSV export (filters, search, escaping, and a 50,000-page export that streams in
//! bounded frames) and the ⌘K palette's page search.

mod support;

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use codoseo_core::page::{Indexability, PageRecord};
use codoseo_store::sites::Site;
use futures_util::StreamExt;
use support::{TestApp, page};
use tower::ServiceExt;
use url::Url;

const COLUMNS: [&str; 22] = [
    "Address",
    "Status",
    "Indexability",
    "Content type",
    "Title",
    "Title length",
    "Meta description",
    "Description length",
    "H1",
    "Canonical",
    "Meta robots",
    "X-Robots-Tag",
    "Word count",
    "Depth",
    "Inlinks",
    "Outlinks (internal)",
    "Outlinks (external)",
    "Response time (ms)",
    "Size (bytes)",
    "In sitemap",
    "Redirect target",
    "Issues",
];

/// The header row and each data row keyed by column name.
fn parse_csv(body: &str) -> (Vec<String>, Vec<HashMap<String, String>>) {
    let mut rdr = csv::Reader::from_reader(body.as_bytes());
    let head: Vec<String> = rdr
        .headers()
        .expect("header row")
        .iter()
        .map(str::to_owned)
        .collect();
    let rows = rdr
        .records()
        .map(|r| {
            let r = r.expect("valid CSV record");
            head.iter()
                .cloned()
                .zip(r.iter().map(str::to_owned))
                .collect()
        })
        .collect();
    (head, rows)
}

fn addresses(rows: &[HashMap<String, String>]) -> Vec<&str> {
    rows.iter().map(|r| r["Address"].as_str()).collect()
}

fn with_key(mut p: PageRecord) -> PageRecord {
    p.key_hash = p.compute_key_hash();
    p
}

/// `/`, `/about`, a 404 at `/gone`, and a two-hop redirect at `/old`.
async fn small_crawl(app: &TestApp, site: &Site) -> uuid::Uuid {
    let gone = with_key(PageRecord {
        status: 404,
        indexability: Indexability::ClientError,
        ..page("example.com", "/gone")
    });
    let old = with_key(PageRecord {
        status: 301,
        indexability: Indexability::Redirected,
        redirect_chain: vec![
            (301, Url::parse("https://example.com/old").unwrap()),
            (302, Url::parse("https://example.com/mid").unwrap()),
        ],
        ..page("example.com", "/old")
    });
    app.finished_crawl(
        site,
        vec![
            page("example.com", "/"),
            page("example.com", "/about"),
            gone,
            old,
        ],
        vec![],
    )
    .await
}

async fn crawl_day(app: &TestApp, crawl_id: uuid::Uuid) -> String {
    sqlx::query_scalar(
        "SELECT (finished_at AT TIME ZONE 'UTC')::date::text FROM crawls WHERE id = $1",
    )
    .bind(crawl_id)
    .fetch_one(app.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn export_has_header_and_one_row_per_page() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, "example.com").await;
    let crawl = small_crawl(&app, &site).await;

    let res = app
        .get(&format!("/s/{}/export.csv", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.header("content-type"), Some("text/csv; charset=utf-8"));
    assert_eq!(res.header("cache-control"), Some("no-store"));
    let day = crawl_day(&app, crawl).await;
    assert_eq!(
        res.header("content-disposition"),
        Some(format!("attachment; filename=\"example.com-all-{day}.csv\"").as_str())
    );

    let (head, rows) = parse_csv(&res.body);
    assert_eq!(head, COLUMNS);
    assert_eq!(
        addresses(&rows),
        [
            "https://example.com/",
            "https://example.com/about",
            "https://example.com/gone",
            "https://example.com/old",
        ]
    );

    let about = &rows[1];
    let title = "/about | A page title that is long enough";
    assert_eq!(about["Status"], "200");
    assert_eq!(about["Indexability"], "Indexable");
    assert_eq!(about["Content type"], "text/html; charset=utf-8");
    assert_eq!(about["Title"], title);
    assert_eq!(about["Title length"], title.chars().count().to_string());
    assert_eq!(about["H1"], "Heading for /about");
    assert_eq!(about["Word count"], "640");
    assert_eq!(about["Depth"], "1");
    assert_eq!(about["Inlinks"], "1");
    assert_eq!(about["Response time (ms)"], "120");
    assert_eq!(about["Size (bytes)"], "24000");
    assert_eq!(about["In sitemap"], "Yes");
    assert_eq!(about["Redirect target"], "");

    let gone = &rows[2];
    assert_eq!(gone["Status"], "404");
    assert_eq!(gone["Indexability"], "Client error");
    let issues: Vec<&str> = gone["Issues"].split("; ").collect();
    assert!(issues.contains(&"http_4xx"), "{issues:?}");

    let old = &rows[3];
    assert_eq!(old["Indexability"], "Redirected");
    assert_eq!(old["Redirect target"], "https://example.com/mid");
}

#[tokio::test]
async fn export_respects_filter_and_q() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, "example.com").await;
    let crawl = small_crawl(&app, &site).await;
    let day = crawl_day(&app, crawl).await;
    let get = |query: &str| {
        let path = format!("/s/{}/export.csv?{query}", site.id);
        let cookie = cookie.clone();
        let app = &app;
        async move { app.get(&path, Some(&cookie)).await }
    };

    let res = get("filter=s4").await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.header("content-disposition"),
        Some(format!("attachment; filename=\"example.com-s4-{day}.csv\"").as_str())
    );
    let (head, rows) = parse_csv(&res.body);
    assert_eq!(head, COLUMNS);
    assert_eq!(addresses(&rows), ["https://example.com/gone"]);

    // A check filter: the ':' becomes '-' in the file name.
    let res = get("filter=check%3Ahttp_4xx").await;
    assert_eq!(
        res.header("content-disposition"),
        Some(format!("attachment; filename=\"example.com-check-http_4xx-{day}.csv\"").as_str())
    );
    assert_eq!(
        addresses(&parse_csv(&res.body).1),
        ["https://example.com/gone"]
    );

    // `q` matches the URL or the title, case-insensitively.
    let res = get("q=ABOUT").await;
    assert_eq!(
        addresses(&parse_csv(&res.body).1),
        ["https://example.com/about"]
    );
    let res = get("filter=s4&q=about").await;
    assert!(parse_csv(&res.body).1.is_empty());

    // LIKE wildcards in `q` match literally: nothing has a '%' or '_' in it.
    for q in ["%25", "_", "%5C"] {
        let res = get(&format!("q={q}")).await;
        assert_eq!(res.status, StatusCode::OK);
        let (head, rows) = parse_csv(&res.body);
        assert_eq!(head, COLUMNS, "header row even with no matches");
        assert!(rows.is_empty(), "q={q} matched {:?}", addresses(&rows));
    }

    // An unknown filter falls back to everything.
    let res = get("filter=nope").await;
    assert_eq!(parse_csv(&res.body).1.len(), 4);
}

#[tokio::test]
async fn export_csv_escaping_round_trips() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, "example.com").await;
    let tricky = "Shoes, \"red\" & blue\nsecond line";
    let mut odd = page("example.com", "/odd");
    odd.fields.title = Some(tricky.to_owned());
    let mut formula = page("example.com", "/formula");
    formula.fields.title = Some("=HYPERLINK(\"https://evil.example\")".to_owned());
    app.finished_crawl(
        &site,
        vec![page("example.com", "/"), with_key(odd), with_key(formula)],
        vec![],
    )
    .await;

    let res = app
        .get(&format!("/s/{}/export.csv", site.id), Some(&cookie))
        .await;
    let (_, rows) = parse_csv(&res.body);
    let odd = rows
        .iter()
        .find(|r| r["Address"] == "https://example.com/odd")
        .expect("odd row");
    assert_eq!(odd["Title"], tricky);
    assert_eq!(odd["Title length"], tricky.chars().count().to_string());

    // A cell a spreadsheet would run as a formula gets a leading apostrophe.
    let formula = rows
        .iter()
        .find(|r| r["Address"] == "https://example.com/formula")
        .expect("formula row");
    assert_eq!(formula["Title"], "'=HYPERLINK(\"https://evil.example\")");
}

#[tokio::test]
async fn export_needs_a_finished_crawl_and_your_own_site() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, "example.com").await;

    let res = app
        .get(&format!("/s/{}/export.csv", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(
        res.body.contains("no finished crawl to export"),
        "{}",
        res.body
    );

    small_crawl(&app, &site).await;
    let (_, other) = app.login("b@x.com").await;
    let res = app
        .get(&format!("/s/{}/export.csv", site.id), Some(&other))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    let res = app.get("/s/not-a-uuid/export.csv", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    let res = app.get(&format!("/s/{}/export.csv", site.id), None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(res.location().unwrap_or("").starts_with("/login"));
}

#[tokio::test]
async fn export_of_50k_pages_streams_in_bounded_frames() {
    const PAGES: usize = 50_000;
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, "big.example").await;

    // Written with SQL rather than through the checks: only the export's reading is under test.
    let crawl_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, started_at, finished_at) \
         VALUES ($1, $2, 'manual', 2, 'done', now() - interval '5 minutes', now()) RETURNING id",
    )
    .bind(site.id)
    .bind(&site.domain)
    .fetch_one(app.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO pages (crawl_id, site_id, url, url_hash, status, response_ms, size_bytes, \
           content_type, depth, in_sitemap, indexability, title, meta_description, h1, \
           word_count, inlinks, issues) \
         SELECT $1, $2, 'https://big.example/page/' || g, g, 200, 90, 18000, 'text/html', 2, \
           true, 'indexable', 'Page ' || g || ', a title', 'Description of page ' || g, \
           jsonb_build_array('Heading ' || g), 480, 3, 4096 \
         FROM generate_series(1, $3) g",
    )
    .bind(crawl_id)
    .bind(site.id)
    .bind(PAGES as i32)
    .execute(app.pool())
    .await
    .unwrap();

    let req = Request::get(format!("/s/{}/export.csv", site.id))
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let res = codoseo_web::app(app.state.clone())
        .oneshot(req)
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let mut body = res.into_body().into_data_stream();
    let (mut frames, mut largest, mut records) = (0usize, 0usize, 0usize);
    let mut first = None;
    while let Some(frame) = body.next().await {
        let frame = frame.expect("body frame");
        frames += 1;
        largest = largest.max(frame.len());
        // Every frame is a whole batch of records, so each one parses on its own.
        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(false)
            .from_reader(&frame[..]);
        for rec in rdr.records() {
            let rec = rec.expect("frames end on a record boundary");
            assert_eq!(rec.len(), COLUMNS.len());
            if first.is_none() {
                first = Some(rec.iter().map(str::to_owned).collect::<Vec<_>>());
            }
            records += 1;
        }
    }
    assert_eq!(first.as_deref(), Some(&COLUMNS.map(str::to_owned)[..]));
    assert_eq!(records, PAGES + 1, "header plus one row per page");
    assert!(frames >= 50, "streamed in batches, got {frames} frames");
    assert!(largest <= 1 << 20, "largest frame {largest} bytes");
}

// ── ⌘K search ────────────────────────────────────────────

/// The `href`s of the returned palette items, in order.
fn hrefs(body: &str) -> Vec<String> {
    body.split("href=\"")
        .skip(1)
        .filter_map(|s| s.split('"').next())
        .filter(|h| h.contains("/explorer?sel="))
        .map(str::to_owned)
        .collect()
}

fn sel(site: &Site, domain: &str, path: &str) -> String {
    let url = Url::parse(&format!("https://{domain}{path}")).unwrap();
    format!(
        "/s/{}/explorer?sel={:016x}",
        site.id,
        codoseo_core::url::url_hash(&url)
    )
}

async fn search(app: &TestApp, site: &Site, q: &str, cookie: &str) -> support::TestResponse {
    app.get_hx(
        &format!(
            "/s/{}/search?q={}",
            site.id,
            codoseo_web::auth::urlencode(q)
        ),
        Some(cookie),
    )
    .await
}

#[tokio::test]
async fn search_matches_path_and_title_prefixes() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, "example.com").await;
    let mut reach = page("example.com", "/about/reach");
    reach.fields.title = Some("Contact <us> today".to_owned());
    let mut untitled = page("example.com", "/blog/untitled?page=2");
    untitled.fields.title = None;
    app.finished_crawl(
        &site,
        vec![
            page("example.com", "/"),
            page("example.com", "/blog/first-post"),
            with_key(reach),
            with_key(untitled),
        ],
        vec![],
    )
    .await;

    // Path prefix, with or without the leading slash.
    for q in ["blog/f", "/blog/f"] {
        let res = search(&app, &site, q, &cookie).await;
        assert_eq!(res.status, StatusCode::OK);
        assert_eq!(
            hrefs(&res.body),
            [sel(&site, "example.com", "/blog/first-post")],
            "q={q}"
        );
    }

    // Title prefix; the title is escaped, and the item has the palette's markup.
    let res = search(&app, &site, "contact", &cookie).await;
    assert_eq!(
        hrefs(&res.body),
        [sel(&site, "example.com", "/about/reach")]
    );
    assert!(res.body.contains("class=\"pal-item\""), "{}", res.body);
    assert!(res.body.contains("role=\"option\""));
    assert!(res.body.contains("<use href=\"#i-explorer\"/>"));
    assert!(
        res.body
            .contains("<span class=\"pal-main mono\">/about/reach</span>")
    );
    assert!(
        res.body.contains("Contact &#60;us&#62; today")
            || res.body.contains("Contact &lt;us&gt; today")
    );
    assert!(!res.body.contains("<us>"));

    // The query string is part of the path shown; a missing title shows a dash.
    let res = search(&app, &site, "untitled", &cookie).await;
    assert!(
        res.body
            .contains("<span class=\"pal-main mono\">/blog/untitled?page=2</span>"),
        "{}",
        res.body
    );
    assert!(res.body.contains("<span class=\"pal-sub\">—</span>"));

    // Typing the host works too, and the full URL.
    let res = search(&app, &site, "example.com/about", &cookie).await;
    assert_eq!(
        hrefs(&res.body),
        [sel(&site, "example.com", "/about/reach")]
    );
    let res = search(&app, &site, "https://example.com/blog/first", &cookie).await;
    assert_eq!(
        hrefs(&res.body),
        [sel(&site, "example.com", "/blog/first-post")]
    );
}

#[tokio::test]
async fn search_ranks_prefix_matches_first_then_shorter_paths() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, "example.com").await;
    app.finished_crawl(
        &site,
        vec![
            page("example.com", "/"),
            page("example.com", "/a-pricing"),
            page("example.com", "/pricing-plans-for-teams"),
            page("example.com", "/pricing/enterprise"),
            page("example.com", "/x/pricing"),
        ],
        vec![],
    )
    .await;

    let res = search(&app, &site, "PRICING", &cookie).await;
    assert_eq!(
        hrefs(&res.body),
        [
            // Prefix matches, shorter path first...
            sel(&site, "example.com", "/pricing/enterprise"),
            sel(&site, "example.com", "/pricing-plans-for-teams"),
            // ...then substring matches, shorter path first.
            sel(&site, "example.com", "/a-pricing"),
            sel(&site, "example.com", "/x/pricing"),
        ]
    );
}

#[tokio::test]
async fn search_is_capped_case_insensitive_and_literal() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, "example.com").await;
    let mut pages = vec![page("example.com", "/")];
    pages.extend((1..=12).map(|i| page("example.com", &format!("/Docs/{i}"))));
    app.finished_crawl(&site, pages, vec![]).await;

    let res = search(&app, &site, "docs", &cookie).await;
    let found = hrefs(&res.body);
    assert_eq!(found.len(), 8, "{found:?}");
    // Shorter paths first: /Docs/1 .. /Docs/9 are one character shorter than /Docs/10.
    assert_eq!(found[0], sel(&site, "example.com", "/Docs/1"));

    // Wildcards match literally, and no match is an empty body.
    for q in ["%", "_", "\\", "d_cs", "nothing-here"] {
        let res = search(&app, &site, q, &cookie).await;
        assert_eq!(res.status, StatusCode::OK);
        assert_eq!(res.body.trim(), "", "q={q}");
    }
}

#[tokio::test]
async fn search_empty_query_or_no_crawl_is_empty_and_others_sites_404() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, "example.com").await;

    // No finished crawl yet: nothing to find.
    let res = search(&app, &site, "about", &cookie).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body.trim(), "");

    small_crawl(&app, &site).await;
    for q in ["", "   "] {
        let res = search(&app, &site, q, &cookie).await;
        assert_eq!(res.status, StatusCode::OK);
        assert_eq!(res.body, "", "q={q:?}");
    }
    let res = app
        .get_hx(&format!("/s/{}/search", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body, "");

    let (_, other) = app.login("b@x.com").await;
    let res = search(&app, &site, "about", &other).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert!(!res.body.contains("pal-item"));
}
