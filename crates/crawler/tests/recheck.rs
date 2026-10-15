//! The fetches of an AI access recheck: robots.txt first, only the named pages CodoSEObot may
//! fetch, `/llms.txt` without link checks, everything by the deadline.

use std::time::Duration;

use codoseo_core::Url;
use codoseo_core::crawl::{AddressPolicy, Politeness};
use codoseo_crawler::recheck::{RecheckFetch, RecheckFetchError, recheck_fetch};
use codoseo_testkit::{Page, SiteBuilder, html_page};
use tokio::time::Instant;

fn cfg(site: Url, pages: Vec<Url>, llms: bool) -> RecheckFetch {
    RecheckFetch {
        site,
        pages,
        llms,
        markdown: false,
        address_policy: AddressPolicy::AllowPrivate,
        politeness: Politeness {
            requests_per_sec: 1000.0,
            ..Politeness::default()
        },
        request_timeout: Duration::from_secs(2),
        deadline: Instant::now() + Duration::from_secs(20),
    }
}

#[tokio::test]
async fn fetches_robots_then_only_the_allowed_pages() {
    let site = SiteBuilder::new()
        .robots(200, "User-agent: *\nDisallow: /private\n")
        .html("/", "Home", &["/a"])
        .html("/a", "A", &[])
        .html("/private", "Private", &[])
        .page(
            "/llms.txt",
            Page::status(200, "# Site\n\n- [A](/a)\n").header("content-type", "text/plain"),
        )
        .start()
        .await;
    let got = recheck_fetch(cfg(
        site.url("/"),
        vec![site.url("/a"), site.url("/private")],
        true,
    ))
    .await
    .unwrap();
    assert_eq!(got.robots.status, 200);
    assert_eq!(got.pages.len(), 1);
    assert_eq!(got.pages[0].url, site.url("/a"));
    assert!(got.pages[0].is_html_ok());
    assert_eq!(got.robots_skipped, vec![site.url("/private")]);
    assert_eq!(got.llms.unwrap().unwrap().status, 200);
    assert_eq!(site.path_hits("/private"), 0);
    assert_eq!(site.path_hits("/"), 0, "not asked for");
    // robots.txt, /a and /llms.txt: no link checks.
    assert_eq!(site.hits(), 3);
    assert_eq!(got.requests, 3);
    assert!(
        site.requests().iter().all(|r| r
            .user_agent
            .as_deref()
            .unwrap_or("")
            .contains("CodoSEObot"))
    );
}

#[tokio::test]
async fn a_robots_txt_with_no_answer_is_an_error_and_nothing_else_is_asked() {
    let site = SiteBuilder::new()
        .page("/robots.txt", Page::drop_connection())
        .html("/a", "A", &[])
        .start()
        .await;
    let err = recheck_fetch(cfg(site.url("/"), vec![site.url("/a")], false))
        .await
        .unwrap_err();
    assert!(matches!(err, RecheckFetchError::Unreachable(_)), "{err:?}");
    assert_eq!(site.path_hits("/a"), 0);
}

#[tokio::test]
async fn a_failing_robots_txt_keeps_crawler_off_every_page() {
    let site = SiteBuilder::new()
        .robots(503, "down")
        .page("/a", Page::html(&html_page("A", &[])))
        .start()
        .await;
    let got = recheck_fetch(cfg(site.url("/"), vec![site.url("/a")], true))
        .await
        .unwrap();
    assert_eq!(got.robots.status, 503);
    assert!(got.pages.is_empty());
    assert_eq!(got.robots_skipped, vec![site.url("/a")]);
    assert!(got.llms.unwrap().is_err(), "llms.txt is off limits too");
    assert_eq!(site.path_hits("/a"), 0);
}

#[tokio::test]
async fn nothing_starts_after_the_deadline() {
    let site = SiteBuilder::new()
        .robots(200, "User-agent: *\nAllow: /\n")
        .page("/slow", Page::slow(Duration::from_secs(5)))
        .html("/b", "B", &[])
        .start()
        .await;
    let mut c = cfg(
        site.url("/"),
        vec![site.url("/slow"), site.url("/b")],
        false,
    );
    c.request_timeout = Duration::from_secs(10);
    c.deadline = Instant::now() + Duration::from_millis(800);
    let started = Instant::now();
    let got = recheck_fetch(c).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(got.pages.len(), 1);
    assert!(!got.pages[0].is_html_ok(), "cut short");
    assert_eq!(got.not_reached, vec![site.url("/b")]);
    assert_eq!(site.path_hits("/b"), 0);
}

#[tokio::test]
async fn asks_for_the_home_page_as_markdown_when_told_to() {
    let site = SiteBuilder::new()
        .page(
            "/",
            Page::by_accept(
                vec![(
                    "text/markdown",
                    Page::html("# Home\n").header("content-type", "text/markdown"),
                )],
                Page::html("<title>Home</title>"),
            ),
        )
        .start()
        .await;
    let got = recheck_fetch(RecheckFetch {
        markdown: true,
        ..cfg(site.url("/"), Vec::new(), false)
    })
    .await
    .unwrap();
    let probe = got.markdown.expect("asked").expect("made");
    assert_eq!(probe.negotiated.status, 200);
    assert_eq!(
        codoseo_geo::markdown::assess(&probe).state,
        codoseo_geo::markdown::MarkdownState::Negotiated
    );
    assert_eq!(site.path_hits("/"), 1);
}
