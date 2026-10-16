//! Markdown for AI agents during a crawl: the home page asked for with `Accept: text/markdown`,
//! then the Markdown alternate it advertises, at most two requests.

use std::time::Duration;

use codoseo_core::Url;
use codoseo_core::crawl::{AddressPolicy, CrawlConfig, CrawlLimits, Politeness, USER_AGENT};
use codoseo_core::output::{AlternateSource, MarkdownProbe};
use codoseo_crawler::crawl::crawl;
use codoseo_geo::markdown::{MarkdownState, assess};
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
        extra_seeds: Vec::new(),
    }
}

fn markdown(body: &str) -> Page {
    Page::html(body).header("content-type", "text/markdown; charset=utf-8")
}

fn home_html() -> Page {
    Page::html(&html_page("Home", &[]))
}

async fn probe(start: Url) -> MarkdownProbe {
    crawl(cfg(start), |_| {})
        .await
        .unwrap()
        .signals
        .markdown
        .expect("the home page was asked for as Markdown")
}

#[tokio::test]
async fn markdown_served_by_content_negotiation() {
    let site = SiteBuilder::new()
        .page(
            "/",
            Page::by_accept(
                vec![("text/markdown", markdown("# Home\n\nHello.\n"))],
                home_html(),
            ),
        )
        .start()
        .await;
    let out = crawl(cfg(site.url("/")), |_| {}).await.unwrap();
    let p = out.signals.markdown.expect("asked");
    assert_eq!(p.negotiated.status, 200);
    assert_eq!(
        p.negotiated.content_type.as_deref(),
        Some("text/markdown; charset=utf-8")
    );
    assert!(p.negotiated.body_bytes > 0);
    assert_eq!(p.alternate, None);
    assert_eq!(assess(&p).state, MarkdownState::Negotiated);
    // The crawl's own fetch of the home page still got HTML.
    assert_eq!(out.pages.len(), 1);
    assert_eq!(
        out.pages[0].content_type.as_deref(),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        site.path_hits("/"),
        2,
        "one crawl fetch, one Markdown request"
    );
}

#[tokio::test]
async fn html_only_is_not_served() {
    let site = SiteBuilder::new().page("/", home_html()).start().await;
    let p = probe(site.url("/")).await;
    assert_eq!(p.negotiated.status, 200);
    assert_eq!(
        p.negotiated.content_type.as_deref(),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        (p.alternate.clone(), p.alternate_fetch.clone()),
        (None, None)
    );
    let r = assess(&p);
    assert_eq!(r.state, MarkdownState::NotServed);
    assert_eq!(r.describe(), "HTTP 200 text/html");
}

#[tokio::test]
async fn an_advertised_alternate_link_counts() {
    let head = r#"<!doctype html><html><head><title>Home</title>
        <link rel="alternate" type="text/markdown" href="/index.md"></head><body><h1>Home</h1></body></html>"#;
    let site = SiteBuilder::new()
        .page("/", Page::html(head))
        .page("/index.md", markdown("# Home\n"))
        .start()
        .await;
    let p = probe(site.url("/")).await;
    assert_eq!(
        p.alternate,
        Some(("/index.md".to_owned(), AlternateSource::LinkTag))
    );
    let alt = p.alternate_fetch.as_ref().expect("the alternate was asked");
    assert_eq!(alt.status, 200);
    assert_eq!(assess(&p).state, MarkdownState::Alternate);
    assert_eq!(site.path_hits("/index.md"), 1);
}

#[tokio::test]
async fn a_link_header_alternate_counts_and_an_empty_one_does_not() {
    let site = SiteBuilder::new()
        .page(
            "/",
            home_html().header(
                "link",
                r#"</home.md>; rel="alternate"; type="text/markdown""#,
            ),
        )
        .page("/home.md", markdown("  \n"))
        .start()
        .await;
    let p = probe(site.url("/")).await;
    assert_eq!(
        p.alternate,
        Some(("/home.md".to_owned(), AlternateSource::LinkHeader))
    );
    // It answers Markdown, but empty.
    assert_eq!(p.alternate_fetch.as_ref().unwrap().body_bytes, 0);
    let r = assess(&p);
    assert_eq!(r.state, MarkdownState::NotServed);
    assert!(
        r.describe().contains("text/markdown (empty)"),
        "{}",
        r.describe()
    );
}

#[tokio::test]
async fn an_alternate_on_another_origin_is_not_requested() {
    let site = SiteBuilder::new()
        .page(
            "/",
            home_html().header(
                "link",
                r#"<https://elsewhere.invalid/a.md>; rel="alternate"; type="text/markdown""#,
            ),
        )
        .start()
        .await;
    let p = probe(site.url("/")).await;
    assert!(p.alternate.is_some());
    assert_eq!(p.alternate_fetch, None);
    assert_eq!(assess(&p).state, MarkdownState::NotServed);
}

#[tokio::test]
async fn not_acceptable_and_server_errors_are_not_served() {
    for status in [406, 500] {
        let site = SiteBuilder::new()
            .page(
                "/",
                Page::by_accept(
                    vec![("text/markdown", Page::status(status, "no"))],
                    home_html(),
                ),
            )
            .start()
            .await;
        let p = probe(site.url("/")).await;
        assert_eq!(p.negotiated.status, status);
        let r = assess(&p);
        assert_eq!(r.state, MarkdownState::NotServed, "{status}");
        assert_eq!(r.describe(), format!("HTTP {status} text/html"));
    }
}

#[tokio::test]
async fn nothing_is_asked_without_site_signals() {
    let site = SiteBuilder::new().page("/", home_html()).start().await;
    let out = crawl(
        CrawlConfig {
            site_signals: false,
            ..cfg(site.url("/"))
        },
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(out.signals.markdown, None);
    assert_eq!(site.path_hits("/"), 1);
}
