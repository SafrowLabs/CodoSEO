//! T5.4: the URL explorer. Filters, search, keyset pagination, the detail tabs, key-page
//! stars, ownership checks and the empty state, against crawls written through the real
//! checks and `finalize`.

mod support;

use axum::http::StatusCode;
use codoseo_core::page::{Indexability, PageFields, PageRecord};
use codoseo_store::sites::Site;
use support::{TestApp, page};
use url::Url;

const DOMAIN: &str = "example.com";

fn hash_hex(path: &str) -> String {
    let url = Url::parse(&format!("https://{DOMAIN}{path}")).unwrap();
    format!("{:016x}", codoseo_core::url::url_hash(&url))
}

fn rehash(mut p: PageRecord) -> PageRecord {
    p.key_hash = p.compute_key_hash();
    p
}

/// A page with no HTML fields (an image, a PDF, a redirect).
fn bare(path: &str) -> PageRecord {
    PageRecord {
        fields: PageFields::default(),
        ..page(DOMAIN, path)
    }
}

/// Ten pages covering every fixed filter: 2xx, a 301 (two hops), a 404, a 503, a noindex
/// page, an image, a PDF, a page without a title and a title with a literal `%`.
fn fixture() -> Vec<PageRecord> {
    let url = |p: &str| Url::parse(&format!("https://{DOMAIN}{p}")).unwrap();
    let mut sale = page(DOMAIN, "/sale");
    sale.fields.title = Some("Sale: 50% off everything this weekend only".to_owned());
    let mut private = page(DOMAIN, "/private");
    private.fields.meta_robots = Some("noindex".to_owned());
    private.indexability = Indexability::Noindex;
    let mut untitled = page(DOMAIN, "/untitled");
    untitled.fields.title = None;
    vec![
        page(DOMAIN, "/"),
        page(DOMAIN, "/about"),
        PageRecord {
            status: 301,
            indexability: Indexability::Redirected,
            content_type: None,
            size_bytes: 0,
            redirect_chain: vec![(301, url("/old")), (302, url("/mid"))],
            redirect_target: Some(url("/new")),
            ..bare("/old")
        },
        PageRecord {
            status: 404,
            indexability: Indexability::ClientError,
            ..bare("/gone")
        },
        PageRecord {
            status: 503,
            indexability: Indexability::ServerError,
            response_ms: 2_400,
            ..bare("/down")
        },
        private,
        PageRecord {
            content_type: Some("image/jpeg".to_owned()),
            ..bare("/photo.jpg")
        },
        PageRecord {
            content_type: Some("application/pdf".to_owned()),
            ..bare("/guide.pdf")
        },
        untitled,
        sale,
    ]
    .into_iter()
    .map(rehash)
    .collect()
}

async fn setup() -> (TestApp, Site, String) {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    app.finished_crawl(&site, fixture(), vec![]).await;
    (app, site, cookie)
}

/// The URL paths of the grid rows in a response, in order.
fn row_paths(body: &str) -> Vec<String> {
    let marker = r#"<span class="path" role="gridcell" title=""#;
    body.match_indices(marker)
        .map(|(i, _)| {
            let rest = &body[i + marker.len()..];
            let url = &rest[..rest.find('"').unwrap()];
            url.trim_start_matches(&format!("https://{DOMAIN}"))
                .to_owned()
        })
        .collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn strs(v: &[&str]) -> Vec<String> {
    sorted(v.iter().map(|s| (*s).to_owned()).collect())
}

/// The load-more sentinel's URL, if the fragment has one.
fn next_url(body: &str) -> Option<String> {
    let i = body.find(r#"class="load-more" hx-get=""#)?;
    let rest = &body[i + r#"class="load-more" hx-get=""#.len()..];
    Some(
        rest[..rest.find('"').unwrap()]
            .replace("&#38;", "&")
            .replace("&amp;", "&"),
    )
}

#[tokio::test]
async fn each_fixed_filter_returns_exactly_its_rows() {
    let (app, site, cookie) = setup().await;
    let cases: [(&str, &[&str]); 10] = [
        (
            "all",
            &[
                "/",
                "/about",
                "/old",
                "/gone",
                "/down",
                "/private",
                "/photo.jpg",
                "/guide.pdf",
                "/untitled",
                "/sale",
            ],
        ),
        (
            "s2",
            &[
                "/",
                "/about",
                "/private",
                "/photo.jpg",
                "/guide.pdf",
                "/untitled",
                "/sale",
            ],
        ),
        ("s3", &["/old"]),
        ("s4", &["/gone"]),
        ("s5", &["/down"]),
        (
            "ix",
            &[
                "/",
                "/about",
                "/photo.jpg",
                "/guide.pdf",
                "/untitled",
                "/sale",
            ],
        ),
        ("nx", &["/old", "/gone", "/down", "/private"]),
        (
            "html",
            &[
                "/",
                "/about",
                "/gone",
                "/down",
                "/private",
                "/untitled",
                "/sale",
            ],
        ),
        ("img", &["/photo.jpg"]),
        ("oth", &["/old", "/guide.pdf"]),
    ];
    for (key, expected) in cases {
        let res = app
            .get_hx(
                &format!("/s/{}/explorer/rows?filter={key}", site.id),
                Some(&cookie),
            )
            .await;
        assert_eq!(res.status, StatusCode::OK, "{key}: {}", res.body);
        assert_eq!(sorted(row_paths(&res.body)), strs(expected), "filter {key}");
        assert!(!res.body.contains("<html"), "a fragment");
        // The full page agrees, and marks the filter active.
        let page = app
            .get(
                &format!("/s/{}/explorer?filter={key}", site.id),
                Some(&cookie),
            )
            .await;
        assert_eq!(sorted(row_paths(&page.body)), strs(expected), "page {key}");
        assert!(
            page.body.contains(&format!(
                r#"class="filter is-active" href="/s/{}/explorer?filter={key}""#,
                site.id
            )),
            "{key} is active"
        );
    }
}

#[tokio::test]
async fn the_page_has_filter_counts_issues_and_a_selection() {
    let (app, site, cookie) = setup().await;
    let res = app
        .get(&format!("/s/{}/explorer", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let b = &res.body;
    assert!(b.contains(r#"class="panel fill""#));
    assert!(b.contains("All URLs · 10 shown of 10"), "{b}");
    assert!(b.contains("data-search-input"));
    assert!(b.contains(r#"placeholder="Filter by URL or title""#));
    // No page failed to respond, so the No response filter is hidden.
    assert!(!b.contains("filter=s0"));
    // Issues come from the summary, page-level checks only, labelled with the check title.
    let title_missing = codoseo_checks::def(codoseo_core::check::CheckId::TitleMissing).title;
    assert!(b.contains("filter=check:title_missing"));
    assert!(b.contains(title_missing));
    assert!(!b.contains("filter=check:sitemap_missing"), "site-wide");
    // Critical issues are listed before warnings.
    let http_4xx = b.find("filter=check:http_4xx").unwrap();
    let missing = b.find("filter=check:title_missing").unwrap();
    assert!(http_4xx < missing);
    // With no `sel`, the first row is selected and its details shown.
    assert_eq!(b.matches(r#"data-row aria-selected="true""#).count(), 1);
    assert!(b.contains(&format!(
        r#"id="ex-sel" name="sel" value="{}""#,
        hash_hex("/")
    )));
    assert!(b.contains("Inlinks / outlinks"));
}

#[tokio::test]
async fn check_filter_returns_only_flagged_pages() {
    let (app, site, cookie) = setup().await;
    let res = app
        .get_hx(
            &format!("/s/{}/explorer/rows?filter=check:title_missing", site.id),
            Some(&cookie),
        )
        .await;
    assert_eq!(row_paths(&res.body), vec!["/untitled".to_owned()]);
    assert!(res.body.contains(">Missing<"), "red Missing title");
    let res = app
        .get(
            &format!("/s/{}/explorer?filter=check:title_missing", site.id),
            Some(&cookie),
        )
        .await;
    assert!(res.body.contains("· 1 shown of 10"), "{}", res.body);
}

#[tokio::test]
async fn search_matches_path_and_title_ignoring_case_and_wildcards() {
    let (app, site, cookie) = setup().await;
    let rows = |q: &str| {
        let path = format!("/s/{}/explorer/rows?filter=all&q={q}", site.id);
        let app = &app;
        let cookie = cookie.clone();
        async move { app.get_hx(&path, Some(&cookie)).await }
    };

    // By path, in any case.
    let res = rows("ABOUT").await;
    assert_eq!(row_paths(&res.body), vec!["/about".to_owned()]);
    // By title.
    let res = rows("everything").await;
    assert_eq!(row_paths(&res.body), vec!["/sale".to_owned()]);
    // `%` and `_` are literal characters, not wildcards.
    let res = rows("50%25").await;
    assert_eq!(row_paths(&res.body), vec!["/sale".to_owned()]);
    let res = rows("%25").await;
    assert_eq!(row_paths(&res.body), vec!["/sale".to_owned()]);
    let res = rows("_").await;
    assert!(row_paths(&res.body).is_empty());
    assert!(res.body.contains("No URLs match this filter."));

    // A search combines with the filter, updates the count out of band and the address bar.
    let res = app
        .get_hx(
            &format!("/s/{}/explorer/rows?filter=s2&q=JPG", site.id),
            Some(&cookie),
        )
        .await;
    assert_eq!(row_paths(&res.body), vec!["/photo.jpg".to_owned()]);
    assert!(res.body.contains(r#"id="ex-info""#));
    assert!(res.body.contains(r#"hx-swap-oob="true""#));
    assert!(
        res.body.contains("2xx Success · 1 shown of 10"),
        "{}",
        res.body
    );
    assert_eq!(
        res.header("hx-replace-url"),
        Some(format!("/s/{}/explorer?filter=s2&q=JPG", site.id).as_str())
    );
    let res = app
        .get_hx(
            &format!("/s/{}/explorer/rows?filter=nx&q=jpg", site.id),
            Some(&cookie),
        )
        .await;
    assert!(row_paths(&res.body).is_empty());
    assert!(res.body.contains("Non-indexable · 0 shown of 10"));

    // The full page takes `q` too.
    let res = app
        .get(
            &format!("/s/{}/explorer?q=Untitled", site.id),
            Some(&cookie),
        )
        .await;
    assert_eq!(row_paths(&res.body), vec!["/untitled".to_owned()]);
    assert!(res.body.contains(r#"value="Untitled""#));
}

#[tokio::test]
async fn pagination_walks_every_row_once() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    let pages = (0..250)
        .map(|i| page(DOMAIN, &format!("/p/{i:03}")))
        .collect::<Vec<_>>();
    app.finished_crawl(&site, pages, vec![]).await;

    let mut url = format!("/s/{}/explorer/rows?filter=all", site.id);
    let mut seen = Vec::new();
    let mut batches = 0;
    loop {
        let res = app.get_hx(&url, Some(&cookie)).await;
        assert_eq!(res.status, StatusCode::OK);
        let paths = row_paths(&res.body);
        batches += 1;
        // Only the first batch (a fresh search) carries the count and a new address.
        assert_eq!(res.body.contains("hx-swap-oob"), batches == 1);
        assert_eq!(res.header("hx-replace-url").is_some(), batches == 1);
        seen.extend(paths.iter().cloned());
        match next_url(&res.body) {
            Some(next) => {
                assert_eq!(paths.len(), 100);
                assert!(next.contains("&after="), "{next}");
                assert!(res.body.contains(r#"hx-trigger="intersect once""#));
                url = next;
            }
            None => {
                assert_eq!(paths.len(), 50, "the last batch has no sentinel");
                break;
            }
        }
    }
    assert_eq!(batches, 3);
    let expected: Vec<String> = (0..250).map(|i| format!("/p/{i:03}")).collect();
    assert_eq!(seen.len(), 250, "no repeats");
    assert_eq!(sorted(seen), expected, "no gaps");

    // The full page shows the first 100 rows and the sentinel.
    let res = app
        .get(&format!("/s/{}/explorer", site.id), Some(&cookie))
        .await;
    assert_eq!(row_paths(&res.body).len(), 100);
    assert!(res.body.contains("All URLs · 250 shown of 250"));
    assert!(next_url(&res.body).is_some());
}

#[tokio::test]
async fn detail_tabs_render_each_view() {
    let (app, site, cookie) = setup().await;
    let detail = |path: &str, tab: &str| {
        let url = format!(
            "/s/{}/explorer/detail?sel={}&tab={tab}&filter=s3",
            site.id,
            hash_hex(path)
        );
        let app = &app;
        let cookie = cookie.clone();
        async move { app.get_hx(&url, Some(&cookie)).await }
    };

    // URL details.
    let res = detail("/old", "details").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let b = &res.body;
    assert!(b.starts_with(r#"<section id="detail""#), "{b}");
    assert!(b.contains("301 Moved Permanently"));
    assert!(b.contains("Redirected"));
    assert!(b.contains("Redirect target / chain"));
    assert!(b.contains("(2 hops)"));
    assert!(b.contains(r#"class="badge sev-"#), "issue chips");
    assert!(b.contains("filter=check:redirect_chain"));
    assert!(b.contains(r#"aria-selected="true" hx-get"#));
    assert_eq!(
        res.header("hx-replace-url"),
        Some(format!("/s/{}/explorer?filter=s3&sel={}", site.id, hash_hex("/old")).as_str())
    );

    // SERP snippet.
    let res = detail("/about", "serp").await;
    let b = &res.body;
    assert!(b.contains("Title width"));
    assert!(b.contains("Description width"));
    assert!(b.contains("/ 580 px"));
    assert!(b.contains("/ 990 px"));
    assert!(b.contains("Fits in results"));
    assert!(b.contains(r#"<div class="crumb">example.com<span> › about</span></div>"#));
    assert!(
        res.header("hx-replace-url")
            .is_some_and(|u| u.ends_with("&tab=serp"))
    );
    let res = detail("/untitled", "serp").await;
    assert!(res.body.contains("Missing title"));
    let res = detail("/photo.jpg", "serp").await;
    assert!(
        res.body
            .contains("No meta description. Google will pick text from the page.")
    );

    // Inlinks: the home page links to every other page.
    let res = detail("/about", "inlinks").await;
    let b = &res.body;
    assert!(
        b.contains(r#"title="https://example.com/">https://example.com/</a>"#),
        "{b}"
    );
    assert!(b.contains("Main navigation"));
    assert!(b.contains("Follow"));
    assert!(b.contains("Up to 20 sample inlinks are kept per page"));
    assert!(b.contains(&format!("/explorer?sel={}", hash_hex("/"))));
    let res = detail("/", "inlinks").await;
    assert!(res.body.contains("No inlinks were kept for this URL."));

    // HTTP headers, rebuilt.
    let res = detail("/old", "headers").await;
    let b = &res.body;
    assert!(b.contains("HTTP/1.1 301"), "{b}");
    assert!(b.contains("location: https://example.com/mid"));
    assert!(b.contains("1. 301 https://example.com/old"));
    assert!(b.contains("2. 302 https://example.com/mid"));
    assert!(b.contains("CodoSEO doesn't store raw headers"));
    let res = detail("/private", "headers").await;
    assert!(res.body.contains("HTTP/1.1 200 OK"));
    assert!(res.body.contains("content-type: text/html; charset=utf-8"));
    assert!(res.body.contains("content-length: 24000"));
    assert!(!res.body.contains("location:"));

    // An unknown page is a 404; a missing selection is a 400.
    let res = app
        .get_hx(
            &format!("/s/{}/explorer/detail?sel=00000000000000ab", site.id),
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let res = app
        .get_hx(&format!("/s/{}/explorer/detail", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);

    // The full page opens on the selected page and tab.
    let res = app
        .get(
            &format!(
                "/s/{}/explorer?sel={}&tab=headers",
                site.id,
                hash_hex("/old")
            ),
            Some(&cookie),
        )
        .await;
    assert!(res.body.contains("location: https://example.com/mid"));
    assert!(
        res.body
            .contains(r#"id="ex-tab" name="tab" value="headers""#)
    );
}

#[tokio::test]
async fn fragment_urls_opened_directly_go_to_the_page() {
    let (app, site, cookie) = setup().await;
    let res = app
        .get(
            &format!("/s/{}/explorer/rows?filter=s4&q=gone", site.id),
            Some(&cookie),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(
        res.location(),
        Some(format!("/s/{}/explorer?filter=s4&q=gone", site.id).as_str())
    );
}

async fn key_pages(app: &TestApp, site: &Site) -> Vec<u64> {
    codoseo_store::sites::get_for_account(app.pool(), site.account_id.unwrap(), site.id)
        .await
        .unwrap()
        .unwrap()
        .key_pages
}

#[tokio::test]
async fn starring_toggles_a_key_page() {
    let (app, site, cookie) = setup().await;
    let key_pages = || key_pages(&app, &site);
    let about = hash_hex("/about");
    let path = format!("/s/{}/key-pages?at=row", site.id);

    let res = app
        .post_hx(&path, &format!("hash={about}"), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains(r#"class="star on pop""#), "{}", res.body);
    assert!(res.body.contains(r#"hx-swap="outerHTML""#));
    let trigger = res.header("hx-trigger").unwrap();
    assert!(trigger.contains("Marked as a key page. Changes to it are alerted first."));
    assert!(trigger.contains(&about));
    let want = u64::from_str_radix(&about, 16).unwrap();
    assert_eq!(key_pages().await, vec![want]);

    // The star shows as on in the grid and the detail panel.
    let res = app
        .get(
            &format!("/s/{}/explorer?sel={about}", site.id),
            Some(&cookie),
        )
        .await;
    assert_eq!(
        res.body
            .matches(&format!(r#"class="star on" data-star="{about}""#))
            .count(),
        2
    );

    let res = app
        .post_hx(&path, &format!("hash={about}"), Some(&cookie))
        .await;
    assert!(res.body.contains(r#"class="star""#));
    assert!(
        res.header("hx-trigger")
            .unwrap()
            .contains("Removed from key pages")
    );
    assert!(key_pages().await.is_empty());

    // Only the site's own pages can be starred.
    let res = app
        .post_hx(&path, "hash=00000000000000ab", Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let res = app.post_hx(&path, "hash=nope", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(key_pages().await.is_empty());
}

#[tokio::test]
async fn another_accounts_site_is_a_404() {
    let (app, site, _) = setup().await;
    let (_, other) = app.login("b@x.com").await;
    let base = format!("/s/{}", site.id);
    let sel = hash_hex("/about");
    for path in [
        format!("{base}/explorer"),
        format!("{base}/explorer?filter=s4"),
        format!("{base}/explorer/rows?filter=all"),
        format!("{base}/explorer/detail?sel={sel}"),
    ] {
        let res = app.get_hx(&path, Some(&other)).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{path}");
    }
    let res = app.get(&format!("{base}/explorer"), Some(&other)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let res = app
        .post_hx(
            &format!("{base}/key-pages"),
            &format!("hash={sel}"),
            Some(&other),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn empty_state_before_the_first_crawl() {
    let app = TestApp::new().await;
    let (account, cookie) = app.login("a@x.com").await;
    let site = app.site(&account, DOMAIN).await;
    let res = app
        .get(&format!("/s/{}/explorer", site.id), Some(&cookie))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("Run your first crawl"));
    assert!(res.body.contains(&format!(
        r#"hx-post="/s/{}/crawls" hx-swap="none""#,
        site.id
    )));
    assert!(!res.body.contains("grid-row"));
}
