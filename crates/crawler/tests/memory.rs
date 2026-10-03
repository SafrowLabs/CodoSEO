//! Peak-memory checks for inputs a hostile site can send (M1 review).
//! One binary with a counting allocator; a lock keeps measurements apart.

mod support;

use std::sync::Mutex;
use std::time::Duration;

use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use bytes::Bytes;
use codoseo_core::Url;
use codoseo_core::crawl::AddressPolicy;
use codoseo_core::page::JsonLdStatus;
use codoseo_crawler::extract::extract;
use codoseo_crawler::fetch::{Fetcher, FetcherConfig};
use codoseo_crawler::robots::RobotsRules;
use codoseo_crawler::sitemap::{discover, parse_sitemap};
use peak_alloc::PeakAlloc;
use support::server::{TestServer, gzip};

#[global_allocator]
static PEAK: PeakAlloc = PeakAlloc;
static LOCK: Mutex<()> = Mutex::new(());
const MB: usize = 1024 * 1024;

fn peak_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
    PEAK.reset_peak_usage();
    let base = PEAK.current_usage();
    let out = f();
    (out, PEAK.peak_usage().saturating_sub(base))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn fetcher() -> Fetcher {
    Fetcher::new(FetcherConfig::new(AddressPolicy::AllowPrivate)).unwrap()
}

#[test]
fn hostile_robots_txt_parses_in_little_memory() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut body = b"User-agent: *\n".to_vec();
    let mut i = 0;
    while body.len() < 500 * 1024 {
        body.extend_from_slice(format!("Disallow: /*a*b*c*d*e*f*g*h*{i}$\n").as_bytes());
        i += 1;
    }
    let (rules, peak) = peak_during(|| RobotsRules::parse(&body, "CodoSEObot"));
    assert!(peak < 32 * MB, "peak {} MB", peak / MB);
    drop(rules);
}

#[test]
fn gzip_sitemaps_are_parsed_as_a_stream() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut xml = String::from(
        r#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><url><loc>https://e.test/a</loc></url>"#,
    );
    while xml.len() < 20 * MB {
        xml.push_str("<!-- padding -->\n");
    }
    xml.push_str("<url><loc>https://e.test/b</loc></url></urlset>");
    let gz = gzip(xml.as_bytes());
    drop(xml);
    let (doc, peak) = peak_during(|| parse_sitemap(&gz).unwrap());
    assert_eq!(doc.locs.len(), 2);
    assert!(peak < 4 * MB, "peak {} MB", peak / MB);
}

#[test]
fn capped_bodies_never_hold_more_than_the_cap() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let rt = runtime();
    let body = Bytes::from(vec![b'x'; 6 * MB]);
    let router = Router::new().route(
        "/big",
        get(move || {
            let body = body.clone();
            async move { ([(header::CONTENT_TYPE, "text/html")], body).into_response() }
        }),
    );
    let srv = rt.block_on(TestServer::start(router));
    let f = fetcher();
    let url = srv.url("/big");
    let (res, peak) = peak_during(|| rt.block_on(f.fetch(&url)).unwrap());
    assert!(res.truncated);
    assert!(peak < 6 * MB + MB / 2, "peak {} MB", peak / MB);
}

#[test]
fn big_json_ld_is_checked_without_building_a_tree() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let html = format!(
        r#"<html><head><script type="application/ld+json">[{}[]]</script></head><body>x</body></html>"#,
        "[],".repeat(1_300_000)
    );
    let url = Url::parse("https://e.test/").unwrap();
    let (e, peak) = peak_during(|| extract(&url, Some("text/html"), html.as_bytes()));
    assert_eq!(e.fields.jsonld, JsonLdStatus::TooLarge);
    assert!(peak < 8 * MB, "peak {} MB", peak / MB);
}

fn xml(body: String) -> axum::response::Response {
    ([(header::CONTENT_TYPE, "application/xml")], body).into_response()
}

#[test]
fn sitemap_index_fan_out_keeps_memory_flat() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let rt = runtime();
    let router = Router::new()
        .route(
            "/index.xml",
            get(|| async {
                let maps: String = (0..200)
                    .map(|i| format!("<sitemap><loc>/c/{i}.xml</loc></sitemap>"))
                    .collect();
                xml(format!("<sitemapindex>{maps}</sitemapindex>"))
            }),
        )
        .route(
            "/c/{name}",
            get(
                |axum::extract::Path(name): axum::extract::Path<String>| async move {
                    let maps: String = (0..20_000)
                        .map(|j| format!("<sitemap><loc>/g/{name}/{j}.xml</loc></sitemap>"))
                        .collect();
                    xml(format!("<sitemapindex>{maps}</sitemapindex>"))
                },
            ),
        );
    let srv = rt.block_on(TestServer::start(router));
    let f = fetcher();
    let seeds = [srv.url("/index.xml")];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let (found, peak) = peak_during(|| rt.block_on(discover(&f, &seeds, 50_000, deadline)));
    assert!(found.urls.is_empty());
    assert!(peak < 40 * MB, "peak {} MB", peak / MB);
}
