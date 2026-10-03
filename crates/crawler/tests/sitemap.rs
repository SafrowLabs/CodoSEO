mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use codoseo_core::crawl::AddressPolicy;
use codoseo_crawler::fetch::{Fetcher, FetcherConfig};
use codoseo_crawler::sitemap::{SitemapKind, discover, parse_sitemap};
use support::server::{TestServer, gzip};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

#[test]
fn parses_a_urlset_with_entities_and_whitespace() {
    let doc = parse_sitemap(&fixture("urlset.xml")).unwrap();
    assert_eq!(doc.kind, SitemapKind::UrlSet);
    assert_eq!(
        doc.locs,
        [
            "https://northwind.test/",
            "https://northwind.test/tents",
            "https://northwind.test/search?q=tent&page=2"
        ]
    );
}

#[test]
fn parses_a_sitemap_index_including_cdata() {
    let doc = parse_sitemap(&fixture("index.xml")).unwrap();
    assert_eq!(doc.kind, SitemapKind::Index);
    assert_eq!(
        doc.locs,
        [
            "https://northwind.test/sitemap-pages.xml",
            "https://northwind.test/sitemap-posts.xml"
        ]
    );
}

#[test]
fn detects_gzip_by_magic_bytes() {
    assert_eq!(
        parse_sitemap(&fixture("urlset.xml.gz")).unwrap().locs.len(),
        3
    );
}

#[test]
fn rejects_documents_that_are_not_sitemaps() {
    assert!(parse_sitemap(&fixture("not_a_sitemap.xml")).is_err());
    assert!(parse_sitemap(b"\x00\x01garbage").is_err());
}

fn xml(body: String) -> axum::response::Response {
    ([(header::CONTENT_TYPE, "application/xml")], body).into_response()
}

fn urlset(paths: &[&str]) -> String {
    let urls: String = paths
        .iter()
        .map(|p| format!("<url><loc>{p}</loc></url>"))
        .collect();
    format!(r#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">{urls}</urlset>"#)
}

fn index(paths: &[&str]) -> String {
    let maps: String = paths
        .iter()
        .map(|p| format!("<sitemap><loc>{p}</loc></sitemap>"))
        .collect();
    format!(
        r#"<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">{maps}</sitemapindex>"#
    )
}

fn routes() -> Router {
    Router::new()
        .route(
            "/index.xml",
            get(|| async {
                xml(index(&[
                    "/sm1.xml",
                    "/sm2.xml.gz",
                    "/index.xml",
                    "/nested.xml",
                    "/missing.xml",
                ]))
            }),
        )
        .route(
            "/sm1.xml",
            get(|| async { xml(urlset(&["/a", "/b", "/a"])) }),
        )
        .route(
            "/sm2.xml.gz",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/gzip")],
                    gzip(urlset(&["/d", "/e"]).as_bytes()),
                )
                    .into_response()
            }),
        )
        .route("/nested.xml", get(|| async { xml(index(&["/deep.xml"])) }))
        .route(
            "/deep.xml",
            get(|| async { xml(index(&["/too-deep.xml"])) }),
        )
        .route("/too-deep.xml", get(|| async { xml(urlset(&["/never"])) }))
        .route(
            "/huge.xml",
            get(|| async {
                let paths: Vec<String> = (0..60_000).map(|i| format!("/p/{i}")).collect();
                let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
                xml(urlset(&refs))
            }),
        )
}

fn far() -> tokio::time::Instant {
    tokio::time::Instant::now() + std::time::Duration::from_secs(60)
}

fn fetcher() -> Fetcher {
    Fetcher::new(FetcherConfig::new(AddressPolicy::AllowPrivate)).unwrap()
}

#[tokio::test]
async fn follows_indexes_to_depth_2_and_dedupes() {
    let srv = TestServer::start(routes()).await;
    let found = discover(&fetcher(), &[srv.url("/index.xml")], 50_000, far()).await;
    let mut paths: Vec<&str> = found.urls.iter().map(|u| u.path()).collect();
    paths.sort();
    assert_eq!(paths, ["/a", "/b", "/d", "/e"]);
    assert_eq!(found.summary.url_count, 4);
    assert_eq!(
        found.summary.files.len(),
        5,
        "index, sm1, sm2, nested, deep: {:?}",
        found.summary.files
    );
    assert!(!found.summary.truncated);
    assert_ne!(found.summary.hash, 0);
}

#[tokio::test]
async fn stops_at_the_url_cap() {
    let srv = TestServer::start(routes()).await;
    let found = discover(&fetcher(), &[srv.url("/huge.xml")], 50_000, far()).await;
    assert_eq!(found.urls.len(), 50_000);
    assert!(found.summary.truncated);
}

#[tokio::test]
async fn missing_sitemaps_give_nothing() {
    let srv = TestServer::start(routes()).await;
    let found = discover(&fetcher(), &[srv.url("/missing.xml")], 50_000, far()).await;
    assert!(found.urls.is_empty());
    assert!(found.summary.files.is_empty());
}

// Fixes from the M1 review.

#[test]
fn image_locs_are_not_page_urls() {
    let xml = br#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:image="http://www.google.com/schemas/sitemap-image/1.1">
      <url><loc>https://e.test/post</loc><image:image><image:loc>https://e.test/a.jpg</image:loc></image:image></url>
    </urlset>"#;
    assert_eq!(parse_sitemap(xml).unwrap().locs, ["https://e.test/post"]);
}

#[tokio::test]
async fn failed_children_count_toward_the_file_cap() {
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&requests);
    let children: Vec<String> = (0..3_000).map(|i| format!("/nope/{i}.xml")).collect();
    let fan = index(&children.iter().map(String::as_str).collect::<Vec<_>>());
    let router = Router::new()
        .route(
            "/fan.xml",
            get(move || {
                let fan = fan.clone();
                async move { xml(fan) }
            }),
        )
        .route(
            "/nope/{i}",
            get(move |Path(_i): Path<String>| {
                counter.fetch_add(1, Ordering::SeqCst);
                async { StatusCode::NOT_FOUND }
            }),
        );
    let srv = TestServer::start(router).await;
    let found = discover(&fetcher(), &[srv.url("/fan.xml")], 50_000, far()).await;
    assert!(
        requests.load(Ordering::SeqCst) < 100,
        "{} child requests",
        requests.load(Ordering::SeqCst)
    );
    assert!(found.summary.failed_files > 0);
    assert!(!found.summary.complete);
}

#[tokio::test]
async fn a_deadline_returns_partial_results() {
    let router = routes()
        .route(
            "/slow-index.xml",
            get(|| async { xml(index(&["/sm1.xml", "/slow.xml"])) }),
        )
        .route(
            "/slow.xml",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                xml(urlset(&["/late"]))
            }),
        );
    let srv = TestServer::start(router).await;
    let started = std::time::Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(800);
    let found = discover(&fetcher(), &[srv.url("/slow-index.xml")], 50_000, deadline).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );
    let paths: Vec<&str> = found.urls.iter().map(|u| u.path()).collect();
    assert_eq!(paths, ["/a", "/b"]);
    assert!(!found.summary.complete);
}

#[tokio::test]
async fn non_utf8_sitemaps_still_yield_urls() {
    let mut body = br#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><url><loc>/a</loc></url><url><loc>/caf"#.to_vec();
    body.push(0xE9);
    body.extend_from_slice(b"</loc></url><url><loc>/b</loc></url></urlset>");
    let router = Router::new().route(
        "/latin1.xml",
        get(move || {
            let body = body.clone();
            async move { ([(header::CONTENT_TYPE, "application/xml")], body).into_response() }
        }),
    );
    let srv = TestServer::start(router).await;
    let found = discover(&fetcher(), &[srv.url("/latin1.xml")], 50_000, far()).await;
    let paths: Vec<&str> = found.urls.iter().map(|u| u.path()).collect();
    assert!(paths.contains(&"/a") && paths.contains(&"/b"), "{paths:?}");
}
