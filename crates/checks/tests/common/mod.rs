//! Builders shared by the whole-crawl check tests.
#![allow(dead_code)]

use codoseo_core::check::IssueBits;
use codoseo_core::crawl::SitemapSummary;
use codoseo_core::output::{CrawlOutput, Edge, LinkGraph, StopReason};
use codoseo_core::page::{Indexability, JsonLdStatus, OgTags, PageFields, PageRecord};
use url::Url;

pub fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

/// `base` padded with `fill` to `len` characters, so every page gets its own text of a
/// healthy length.
fn padded(base: &str, fill: char, len: usize) -> String {
    let mut s = base.to_string();
    while s.chars().count() < len {
        s.push(fill);
    }
    s
}

/// A healthy page at `https://e.com{path}`: nothing fires on it, and its title,
/// description, H1 and content hash are unique to the path.
pub fn pg(path: &str) -> PageRecord {
    let url = u(&format!("https://e.com{path}"));
    let mut p = PageRecord {
        url: url.clone(),
        url_hash: 1,
        status: 200,
        redirect_chain: vec![],
        response_ms: 120,
        size_bytes: 10_000,
        content_type: Some("text/html; charset=utf-8".into()),
        depth: Some(1),
        in_sitemap: true,
        indexability: Indexability::Indexable,
        fields: PageFields {
            title: Some(padded(&format!("T{path}"), 't', 45)),
            title_count: 1,
            meta_description: Some(padded(&format!("D{path}"), 'd', 120)),
            canonical: Some(url.clone()),
            hreflang: vec![("en".into(), url)],
            h1: vec![format!("Heading {path}")],
            word_count: 400,
            content_hash: xxh(path),
            og: OgTags {
                title: Some("Title".into()),
                description: None,
                image: Some("https://e.com/i.png".into()),
            },
            jsonld: JsonLdStatus::Valid(1),
            ..PageFields::default()
        },
        inlinks: 0,
        outlinks_internal: 3,
        outlinks_external: 0,
        issues: IssueBits(0),
        key_hash: 0,
        redirect_target: None,
        outlinks_nofollow: 0,
        error: None,
    };
    p.key_hash = p.compute_key_hash();
    p
}

/// A cheap stable hash, so each path gets a different `content_hash`.
fn xxh(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

/// The home page: the origin, at depth 0.
pub fn home() -> PageRecord {
    let mut p = pg("/");
    p.depth = Some(0);
    p
}

/// A crawl of `pages` with one edge per `(from, to)` pair and a 3-URL sitemap.
pub fn out(pages: Vec<PageRecord>, edges: &[(u32, u32)]) -> CrawlOutput {
    CrawlOutput {
        origin: u("https://e.com/"),
        pages,
        links: LinkGraph {
            edges: edges
                .iter()
                .map(|&(from, to)| Edge {
                    from,
                    to,
                    anchor: 0,
                    nofollow: false,
                })
                .collect(),
            anchors: vec!["link".into()],
        },
        robots: None,
        sitemap: SitemapSummary {
            url_count: 3,
            complete: true,
            ..SitemapSummary::default()
        },
        stop: StopReason::Completed,
        duration_ms: 1_000,
    }
}

/// Four healthy pages in a chain: the home page and three below it.
pub fn clean_site() -> Vec<PageRecord> {
    let mut pages = vec![home(), pg("/a"), pg("/b"), pg("/c")];
    for (i, p) in pages.iter_mut().enumerate() {
        p.depth = Some(i as u16);
    }
    pages
}

pub fn chain_edges() -> Vec<(u32, u32)> {
    vec![(0, 1), (1, 2), (2, 3)]
}
