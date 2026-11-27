//! The status checks after the crawl: links to other sites, images, scripts and stylesheets.

use std::time::Duration;

use codoseo_core::Url;
use codoseo_core::crawl::{AddressPolicy, CrawlConfig, CrawlLimits, Politeness, USER_AGENT};
use codoseo_core::output::{BrokenTarget, OutboundKind};
use codoseo_crawler::CrawlOutput;
use codoseo_crawler::crawl::crawl;
use codoseo_testkit::{Page, SiteBuilder, TestSite};

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
        site_signals: false,
        extra_seeds: Vec::new(),
    }
}

fn png() -> Page {
    Page::status(200, "png").header("content-type", "image/png")
}

/// Another site (another port is another site): `/alive` answers, `/walled` turns bots away,
/// everything else is 404.
async fn other_site() -> TestSite {
    SiteBuilder::new()
        .html("/alive", "Alive", &[])
        .page("/walled", Page::status(403, "no bots"))
        .page("/down", Page::status(503, "later"))
        .start()
        .await
}

fn broken<'a>(out: &'a CrawlOutput, path: &str) -> Option<&'a BrokenTarget> {
    out.outbound.broken.iter().find(|t| t.url.path() == path)
}

fn paths(out: &CrawlOutput, t: &BrokenTarget) -> Vec<String> {
    t.from
        .iter()
        .map(|&i| out.pages[i as usize].url.path().to_owned())
        .collect()
}

#[tokio::test]
async fn dead_links_images_scripts_and_stylesheets_are_found_once_each() {
    let other = other_site().await;
    let home = format!(
        concat!(
            r#"<link rel="stylesheet" href="/gone.css"><link rel="stylesheet" href="/ok.css">"#,
            r#"<script src="/gone.js"></script>"#,
            r#"<img src="/ok.png" alt=""><img src="/gone.png" alt="">"#,
            r#"<img src="data:image/png;base64,AAAA" alt="">"#,
            r#"<a href="/about">About</a>"#,
            r#"<a href="{0}">Alive</a><a href="{1}">Dead</a>"#,
            r#"<a href="{2}">Walled</a><a href="{3}">Down</a>"#,
        ),
        other.url("/alive"),
        other.url("/dead"),
        other.url("/walled"),
        other.url("/down"),
    );
    let about = format!(
        r#"<img src="/gone.png" alt=""><a href="{}">Dead again</a>"#,
        other.url("/dead")
    );
    let site = SiteBuilder::new()
        .page("/", Page::html(&home))
        .page("/about", Page::html(&about))
        .page("/ok.png", png())
        .page(
            "/ok.css",
            Page::status(200, "a{}").header("content-type", "text/css"),
        )
        .start()
        .await;
    let out = crawl(cfg(site.url("/")), |_| {}).await.unwrap();

    // Images, scripts and stylesheets are not pages of the crawl.
    let crawled: Vec<&str> = out.pages.iter().map(|p| p.url.path()).collect();
    assert_eq!(crawled, ["/", "/about"]);

    let mut kinds: Vec<(&str, OutboundKind, u16)> = out
        .outbound
        .broken
        .iter()
        .map(|t| (t.url.path(), t.kind, t.status))
        .collect();
    kinds.sort_by_key(|k| k.0);
    assert_eq!(
        kinds,
        [
            ("/dead", OutboundKind::Link, 404),
            ("/gone.css", OutboundKind::Stylesheet, 404),
            ("/gone.js", OutboundKind::Script, 404),
            ("/gone.png", OutboundKind::Image, 404),
        ],
        "a 403 or 503 from another site is not proof the page is gone"
    );
    // Every page that uses a dead address is named, and the address is asked for once.
    assert_eq!(
        paths(&out, broken(&out, "/gone.png").unwrap()),
        ["/", "/about"]
    );
    assert_eq!(paths(&out, broken(&out, "/dead").unwrap()), ["/", "/about"]);
    assert_eq!(site.path_hits("/gone.png"), 1);
    assert_eq!(other.path_hits("/dead"), 1);
    assert_eq!(other.path_hits("/alive"), 1);
    // ok.png, ok.css, gone.css, gone.js, gone.png, and four addresses on the other site.
    assert_eq!(out.outbound.checked, 9);
    assert_eq!(out.outbound.unchecked, 0);
}

#[tokio::test]
async fn the_cap_and_robots_txt_leave_addresses_unchecked() {
    let other = other_site().await;
    let home = format!(
        concat!(
            r#"<img src="/private/gone.png" alt="">"#,
            r#"<a href="{0}">One</a><a href="{1}">Two</a><a href="{2}">Three</a>"#,
        ),
        other.url("/one"),
        other.url("/two"),
        other.url("/three"),
    );
    let site = SiteBuilder::new()
        .robots(200, "User-agent: *\nDisallow: /private/\n")
        .page("/", Page::html(&home))
        .start()
        .await;
    let mut c = cfg(site.url("/"));
    c.limits.outbound_checks = 2;
    let out = crawl(c, |_| {}).await.unwrap();
    assert_eq!(site.path_hits("/private/gone.png"), 0, "robots.txt applies");
    assert_eq!(out.outbound.checked, 2);
    assert_eq!(
        out.outbound.unchecked, 2,
        "one disallowed, one past the cap"
    );
    assert_eq!(out.outbound.broken.len(), 2);

    // Switched off: no requests and nothing reported.
    let mut c = cfg(site.url("/"));
    c.limits.outbound_checks = 0;
    let before = other.hits();
    let out = crawl(c, |_| {}).await.unwrap();
    assert_eq!(other.hits(), before);
    assert!(out.outbound.is_empty());
}

#[tokio::test]
async fn a_host_that_refuses_connections_is_reported_with_no_status() {
    // A port nothing listens on: bind one, note it, drop it.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let home = format!(r#"<a href="http://127.0.0.1:{port}/x">Nobody home</a>"#);
    let site = SiteBuilder::new()
        .page("/", Page::html(&home))
        .start()
        .await;
    let out = crawl(cfg(site.url("/")), |_| {}).await.unwrap();
    let t = broken(&out, "/x").expect("reported");
    assert_eq!((t.kind, t.status), (OutboundKind::Link, 0));
}
