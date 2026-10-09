//! Hand-built crawls for the access-report tests.
#![allow(dead_code)]

use codoseo_core::check::IssueBits;
use codoseo_core::crawl::{RobotsFile, SitemapSummary};
use codoseo_core::output::{CrawlOutput, LinkGraph, SiteSignals, StopReason, WellKnownFile};
use codoseo_core::page::{AiMeta, Indexability, PageFields, PageRecord};
use codoseo_core::url::url_hash;
use url::Url;

pub const ORIGIN: &str = "https://example.com/";

pub fn url(path: &str) -> Url {
    Url::parse(&format!("https://example.com{path}")).expect("url")
}

/// One crawled page: 200, HTML, 100 words, no AI markup until the builder methods add some.
pub fn page(path: &str, inlinks: u32) -> PageRecord {
    let url = url(path);
    PageRecord {
        url_hash: url_hash(&url),
        url,
        status: 200,
        redirect_chain: Vec::new(),
        response_ms: 10,
        size_bytes: 1000,
        content_type: Some("text/html; charset=utf-8".to_owned()),
        depth: Some(1),
        in_sitemap: false,
        indexability: Indexability::Indexable,
        fields: PageFields {
            word_count: 100,
            ..PageFields::default()
        },
        inlinks,
        outlinks_internal: 0,
        outlinks_external: 0,
        issues: IssueBits::default(),
        key_hash: 0,
        redirect_target: None,
        outlinks_nofollow: 0,
        error: None,
    }
}

pub trait PageExt {
    fn robots_meta(self, value: &str) -> Self;
    fn x_robots(self, value: &str) -> Self;
    fn bot_meta(self, name: &str, value: &str) -> Self;
    fn nosnippet_words(self, words: u32) -> Self;
    fn status(self, status: u16) -> Self;
}

impl PageExt for PageRecord {
    fn robots_meta(mut self, value: &str) -> Self {
        self.fields.meta_robots = Some(value.to_owned());
        self
    }
    fn x_robots(mut self, value: &str) -> Self {
        self.fields.x_robots_tag = Some(value.to_owned());
        self
    }
    fn bot_meta(mut self, name: &str, value: &str) -> Self {
        self.fields
            .ai
            .bot_meta
            .push((name.to_owned(), value.to_owned()));
        self
    }
    fn nosnippet_words(mut self, words: u32) -> Self {
        self.fields.ai = AiMeta {
            nosnippet_words: words,
            ..self.fields.ai
        };
        self
    }
    fn status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }
}

/// A crawl of `example.com` with the pages given, `/` first. Defaults to a 200 robots.txt that
/// allows everything.
pub fn crawl(pages: Vec<PageRecord>) -> CrawlOutput {
    CrawlOutput {
        origin: Url::parse(ORIGIN).expect("origin"),
        pages,
        links: LinkGraph::default(),
        robots: Some(RobotsFile {
            status: 200,
            body: "User-agent: *\nAllow: /\n".to_owned(),
            hash: 7,
        }),
        sitemap: SitemapSummary::default(),
        stop: StopReason::Completed,
        duration_ms: 1,
        signals: SiteSignals::default(),
    }
}

pub fn with_robots(mut out: CrawlOutput, status: u16, body: &str) -> CrawlOutput {
    out.robots = Some(RobotsFile {
        status,
        body: body.to_owned(),
        hash: 9,
    });
    out
}

pub fn with_tdmrep(mut out: CrawlOutput, status: u16, body: &str) -> CrawlOutput {
    out.signals.tdmrep = Some(WellKnownFile {
        status,
        body: body.to_owned(),
    });
    out
}

/// A home page plus `n - 1` others, every page given the same treatment by `f`.
pub fn site(n: u32, f: impl Fn(PageRecord) -> PageRecord) -> CrawlOutput {
    let mut pages = vec![f(page("/", n))];
    for i in 1..n {
        pages.push(f(page(&format!("/p{i}"), n - i)));
    }
    crawl(pages)
}
