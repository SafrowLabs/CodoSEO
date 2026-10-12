//! `/llms.txt` during a crawl: one robots-gated fetch, then a bounded check of its links that
//! reuses what the crawl already fetched.

use std::time::{Duration, Instant};

use codoseo_core::Url;
use codoseo_core::crawl::{AddressPolicy, CrawlConfig, CrawlLimits, Politeness, USER_AGENT};
use codoseo_core::output::{LinkStatus, LlmsTxtFetch};
use codoseo_crawler::crawl::crawl;
use codoseo_geo::llms::{LlmsState, MAX_BYTES, state_of};
use codoseo_testkit::{Page, SiteBuilder, html_page};

fn cfg(start: Url) -> CrawlConfig {
    CrawlConfig {
        start_url: start,
        limits: CrawlLimits {
            request_timeout: Duration::from_secs(2),
            max_duration: Duration::from_secs(30),
            ..CrawlLimits::default()
        },
        politeness: Politeness {
            requests_per_sec: 1000.0,
            ..Politeness::default()
        },
        address_policy: AddressPolicy::AllowPrivate,
        user_agent: USER_AGENT.to_owned(),
        site_signals: true,
    }
}

fn text(body: &str) -> Page {
    Page::html(body).header("content-type", "text/plain; charset=utf-8")
}

fn llms_with(links: &[&str]) -> String {
    let items: String = links
        .iter()
        .map(|l| format!("- [{l}]({l}): a page\n"))
        .collect();
    format!("# Test Site\n\n> A site for tests.\n\n## Docs\n\n{items}")
}

async fn run(c: CrawlConfig) -> LlmsTxtFetch {
    crawl(c, |_| {})
        .await
        .unwrap()
        .signals
        .llms_txt
        .expect("llms.txt was fetched")
}

fn status_of<'a>(f: &'a LlmsTxtFetch, path: &str) -> Option<&'a LinkStatus> {
    f.links.iter().find(|l| l.url.ends_with(path))
}

#[tokio::test]
async fn fetches_llms_txt_with_its_content_type_and_checks_its_links() {
    let site = SiteBuilder::new()
        .html("/", "Home", &["/crawled", "/gone"])
        .html("/crawled", "Crawled", &[])
        .page("/gone", Page::status(404, "nope"))
        .html("/only-listed", "Listed", &[])
        .page(
            "/llms.txt",
            text(&llms_with(&[
                "/crawled",
                "/gone",
                "/only-listed",
                "/missing",
                "https://elsewhere.test/a.md",
                "/crawled",
            ])),
        )
        .start()
        .await;
    let out = crawl(cfg(site.url("/")), |_| {}).await.unwrap();
    let f = out.signals.llms_txt.expect("fetched");

    assert_eq!(f.status, 200);
    assert_eq!(f.error, None);
    assert_eq!(f.content_type.as_deref(), Some("text/plain; charset=utf-8"));
    assert!(f.body.starts_with("# Test Site"));
    assert!(!f.truncated);
    assert_eq!(state_of(&f), LlmsState::Available);
    assert_eq!(site.path_hits("/llms.txt"), 1);
    assert_eq!(out.pages.len(), 3, "llms.txt and its links are not pages");

    // Crawled links take the crawl's status and cost no request.
    let crawled = status_of(&f, "/crawled").unwrap();
    assert_eq!((crawled.status, crawled.from_crawl), (Some(200), true));
    let gone = status_of(&f, "/gone").unwrap();
    assert_eq!((gone.status, gone.from_crawl), (Some(404), true));
    assert_eq!(site.path_hits("/crawled"), 1);
    assert_eq!(site.path_hits("/gone"), 1);
    // The others on the site are requested once each.
    let listed = status_of(&f, "/only-listed").unwrap();
    assert_eq!((listed.status, listed.from_crawl), (Some(200), false));
    assert_eq!(status_of(&f, "/missing").unwrap().status, Some(404));
    assert_eq!(site.path_hits("/only-listed"), 1);
    // Duplicates are checked once; other origins are only counted.
    assert_eq!(f.links.len(), 4);
    assert_eq!(f.external_unchecked, 1);
}

#[tokio::test]
async fn robots_disallow_means_no_request() {
    let site = SiteBuilder::new()
        .robots(200, "User-agent: *\nDisallow: /llms.txt\n")
        .html("/", "Home", &[])
        .page("/llms.txt", text(&llms_with(&["/a"])))
        .start()
        .await;
    let out = crawl(cfg(site.url("/")), |_| {}).await.unwrap();
    assert_eq!(out.signals.llms_txt, None);
    assert_eq!(site.path_hits("/llms.txt"), 0);
}

#[tokio::test]
async fn links_robots_disallows_are_not_requested() {
    let site = SiteBuilder::new()
        .robots(200, "User-agent: *\nDisallow: /private\n")
        .html("/", "Home", &[])
        .html("/private", "Private", &[])
        .html("/public", "Public", &[])
        .page("/llms.txt", text(&llms_with(&["/private", "/public"])))
        .start()
        .await;
    let f = run(cfg(site.url("/"))).await;
    assert_eq!(site.path_hits("/private"), 0);
    assert!(status_of(&f, "/private").is_none());
    assert_eq!(status_of(&f, "/public").unwrap().status, Some(200));
}

#[tokio::test]
async fn nothing_is_fetched_when_site_signals_are_off() {
    let site = SiteBuilder::new()
        .html("/", "Home", &[])
        .page("/llms.txt", text(&llms_with(&["/a"])))
        .start()
        .await;
    let c = CrawlConfig {
        site_signals: false,
        ..cfg(site.url("/"))
    };
    let out = crawl(c, |_| {}).await.unwrap();
    assert_eq!(out.signals.llms_txt, None);
    assert_eq!(site.path_hits("/llms.txt"), 0);
}

#[tokio::test]
async fn link_checks_stop_at_the_cap() {
    let paths = ["/l1", "/l2", "/l3", "/l4", "/l5"];
    let mut b = SiteBuilder::new()
        .html("/", "Home", &[])
        .page("/llms.txt", text(&llms_with(&paths)));
    for p in paths {
        b = b.html(p, p, &[]);
    }
    let site = b.start().await;
    let mut c = cfg(site.url("/"));
    c.limits.llms_link_checks = 2;
    let f = run(c).await;
    assert_eq!(f.links.len(), 2);
    assert!(f.links.iter().all(|l| !l.from_crawl));
    let requested: usize = paths.iter().map(|p| site.path_hits(p)).sum();
    assert_eq!(requested, 2);
    assert_eq!(
        site.path_hits("/l1") + site.path_hits("/l2"),
        2,
        "file order"
    );
}

#[tokio::test]
async fn a_missing_file_is_recorded_with_its_status_and_no_links() {
    let site = SiteBuilder::new().html("/", "Home", &[]).start().await;
    let f = run(cfg(site.url("/"))).await;
    assert_eq!((f.status, f.body.as_str()), (404, ""));
    assert!(f.links.is_empty());
    assert_eq!(state_of(&f), LlmsState::Absent);
}

#[tokio::test]
async fn a_soft_404_gets_no_link_checks() {
    let site = SiteBuilder::new()
        .html("/", "Home", &[])
        .page(
            "/llms.txt",
            Page::html(&html_page("Not found", &["/somewhere"])),
        )
        .start()
        .await;
    let f = run(cfg(site.url("/"))).await;
    assert_eq!(state_of(&f), LlmsState::SoftNotFound);
    assert!(f.links.is_empty());
    assert_eq!(site.path_hits("/somewhere"), 0);
}

#[tokio::test]
async fn a_server_error_keeps_its_status() {
    let site = SiteBuilder::new()
        .html("/", "Home", &[])
        .page("/llms.txt", Page::status(503, "busy"))
        .start()
        .await;
    let f = run(cfg(site.url("/"))).await;
    assert_eq!((f.status, f.error.as_deref()), (503, None));
    assert_eq!(state_of(&f), LlmsState::Unavailable);
    assert_eq!(site.path_hits("/llms.txt"), 2, "retried once");
}

#[tokio::test]
async fn one_server_error_is_retried_and_the_file_read() {
    let site = SiteBuilder::new()
        .html("/", "Home", &[])
        .page(
            "/llms.txt",
            Page::sequence(vec![Page::status(503, "busy"), text(&llms_with(&[]))]),
        )
        .start()
        .await;
    let f = run(cfg(site.url("/"))).await;
    assert_eq!(f.status, 200);
    assert_eq!(state_of(&f), LlmsState::Available);
    assert_eq!(site.path_hits("/llms.txt"), 2);
}

#[tokio::test]
async fn a_429_is_retried_after_its_retry_after() {
    let site = SiteBuilder::new()
        .html("/", "Home", &[])
        .page(
            "/llms.txt",
            Page::sequence(vec![
                Page::status(429, "slow down").header("retry-after", "1"),
                text(&llms_with(&[])),
            ]),
        )
        .start()
        .await;
    let f = run(cfg(site.url("/"))).await;
    assert_eq!(f.status, 200);
    assert_eq!(site.path_hits("/llms.txt"), 2);
}

#[tokio::test]
async fn a_retry_after_past_the_cap_is_not_waited_for() {
    let site = SiteBuilder::new()
        .html("/", "Home", &[])
        .page(
            "/llms.txt",
            Page::sequence(vec![
                Page::status(429, "slow down").header("retry-after", "3600"),
                text(&llms_with(&[])),
            ]),
        )
        .start()
        .await;
    let started = Instant::now();
    let f = run(cfg(site.url("/"))).await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(f.status, 429);
    assert_eq!(site.path_hits("/llms.txt"), 1);
}

#[tokio::test]
async fn a_timeout_is_retried_once() {
    let site = SiteBuilder::new()
        .html("/", "Home", &[])
        .page(
            "/llms.txt",
            Page::sequence(vec![
                text(&llms_with(&[])).delayed(Duration::from_secs(5)),
                text(&llms_with(&[])),
            ]),
        )
        .start()
        .await;
    let mut c = cfg(site.url("/"));
    c.limits.request_timeout = Duration::from_millis(300);
    let f = run(c).await;
    assert_eq!(f.status, 200);
    assert_eq!(site.path_hits("/llms.txt"), 2);
}

#[tokio::test]
async fn a_timeout_is_kept_as_unavailable() {
    let site = SiteBuilder::new()
        .html("/", "Home", &[])
        .page(
            "/llms.txt",
            text(&llms_with(&[])).delayed(Duration::from_secs(5)),
        )
        .start()
        .await;
    let mut c = cfg(site.url("/"));
    c.limits.request_timeout = Duration::from_millis(300);
    let f = run(c).await;
    assert_eq!(f.status, 0);
    assert_eq!(f.error.as_deref(), Some("request timed out"));
    assert_eq!(state_of(&f), LlmsState::Unavailable);
}

#[tokio::test]
async fn a_dropped_connection_is_kept_as_unavailable() {
    let site = SiteBuilder::new()
        .html("/", "Home", &[])
        .page("/llms.txt", Page::drop_connection())
        .start()
        .await;
    let f = run(cfg(site.url("/"))).await;
    assert_eq!(f.status, 0);
    assert!(f.error.is_some());
    assert_eq!(state_of(&f), LlmsState::Unavailable);
}

#[tokio::test]
async fn link_checks_keep_to_the_crawl_deadline() {
    let site = SiteBuilder::new()
        .html("/", "Home", &[])
        .page("/slow", Page::slow(Duration::from_secs(10)))
        .html("/after", "After", &[])
        .page("/llms.txt", text(&llms_with(&["/slow", "/after"])))
        .start()
        .await;
    let mut c = cfg(site.url("/"));
    c.limits.request_timeout = Duration::from_secs(20);
    c.limits.max_duration = Duration::from_secs(2);
    let started = Instant::now();
    let f = run(c).await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert!(
        f.links.is_empty(),
        "nothing finished before the deadline: {:?}",
        f.links
    );
    assert_eq!(site.path_hits("/after"), 0);
}

#[tokio::test]
async fn a_large_file_is_cut_at_512_kib() {
    let mut body = llms_with(&[]);
    while body.len() <= MAX_BYTES + 4096 {
        body.push_str("Some more prose about the site that goes on and on.\n");
    }
    let total = body.len() as u64;
    let site = SiteBuilder::new()
        .html("/", "Home", &[])
        .page("/llms.txt", text(&body))
        .start()
        .await;
    let f = run(cfg(site.url("/"))).await;
    assert!(f.truncated);
    assert_eq!(f.body.len(), MAX_BYTES);
    assert_eq!(f.bytes, total);
}
