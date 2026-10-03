mod support;

use std::time::Duration;

use codoseo_core::crawl::AddressPolicy;
use codoseo_crawler::fetch::{FetchError, Fetcher, FetcherConfig};
use support::server::{TestServer, fetch_routes};

fn fetcher(policy: AddressPolicy) -> Fetcher {
    let mut cfg = FetcherConfig::new(policy);
    cfg.request_timeout = Duration::from_secs(1);
    Fetcher::new(cfg).unwrap()
}

async fn setup() -> (TestServer, Fetcher) {
    (
        TestServer::start(fetch_routes()).await,
        fetcher(AddressPolicy::AllowPrivate),
    )
}

#[tokio::test]
async fn records_the_whole_redirect_chain_in_order() {
    let (srv, f) = setup().await;
    let r = f.fetch(&srv.url("/r1")).await.unwrap();
    assert_eq!(
        r.chain.iter().map(|h| h.status).collect::<Vec<_>>(),
        vec![301, 302, 301]
    );
    assert_eq!(r.chain[0].url, srv.url("/r1"));
    assert_eq!(r.final_url, srv.url("/ok"));
    assert_eq!(r.status, 200);
    assert_eq!(r.x_robots_tag.as_deref(), Some("noindex"));
}

#[tokio::test]
async fn detects_redirect_loops() {
    let (srv, f) = setup().await;
    assert!(matches!(
        f.fetch(&srv.url("/loop-a")).await,
        Err(FetchError::RedirectLoop { .. })
    ));
}

#[tokio::test]
async fn follows_ten_hops_but_not_eleven() {
    let (srv, f) = setup().await;
    assert_eq!(f.fetch(&srv.url("/hop/10")).await.unwrap().chain.len(), 10);
    assert!(matches!(
        f.fetch(&srv.url("/hop/11")).await,
        Err(FetchError::TooManyRedirects { .. })
    ));
}

#[tokio::test]
async fn rejects_redirects_to_non_http_schemes() {
    let (srv, f) = setup().await;
    assert!(matches!(
        f.fetch(&srv.url("/to-mailto")).await,
        Err(FetchError::InvalidRedirect { .. })
    ));
}

#[tokio::test]
async fn caps_html_bodies_at_5_mb() {
    let (srv, f) = setup().await;
    let big = f.fetch(&srv.url("/big")).await.unwrap();
    assert!(big.truncated);
    assert_eq!(big.body.unwrap().len(), 5 * 1024 * 1024);
}

#[tokio::test]
async fn does_not_read_non_html_bodies() {
    let (srv, f) = setup().await;
    let pdf = f.fetch(&srv.url("/doc.pdf")).await.unwrap();
    assert!(pdf.body.is_none());
    assert_eq!(pdf.size_bytes, 2048);
    assert_eq!(pdf.content_type.as_deref(), Some("application/pdf"));
}

#[tokio::test]
async fn reads_any_body_with_fetch_raw() {
    let (srv, f) = setup().await;
    let robots = f.fetch_raw(&srv.url("/robots.txt"), 1024).await.unwrap();
    assert_eq!(robots.body.unwrap().as_ref(), b"User-agent: *\nDisallow:\n");
}

#[tokio::test]
async fn times_out_slow_responses() {
    let (srv, f) = setup().await;
    assert!(matches!(
        f.fetch(&srv.url("/slow")).await,
        Err(FetchError::Timeout)
    ));
}

#[tokio::test]
async fn decompresses_gzip_bodies() {
    let (srv, f) = setup().await;
    let body = f.fetch(&srv.url("/gz")).await.unwrap().body.unwrap();
    assert!(body.starts_with(b"<!doctype html>"), "{body:?}");
}

#[tokio::test]
async fn public_policy_refuses_ip_literals_and_names_resolving_to_them() {
    let srv = TestServer::start(fetch_routes()).await;
    let f = fetcher(AddressPolicy::Public);
    assert!(matches!(
        f.fetch(&srv.url("/ok")).await,
        Err(FetchError::Blocked(_))
    ));
    let by_name = f.fetch(&srv.localhost_url("/ok")).await;
    assert!(
        matches!(by_name, Err(FetchError::Blocked(_))),
        "{by_name:?}"
    );
}
