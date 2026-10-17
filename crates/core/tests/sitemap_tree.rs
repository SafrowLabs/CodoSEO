mod common;

use codoseo_core::Url;
use codoseo_core::crawl::SitemapSummary;
use codoseo_core::output::{CrawlOutput, LinkGraph, SiteSignals, StopReason};
use codoseo_core::page::{Indexability, PageRecord};
use codoseo_core::sitemap::{
    LastmodStats, SitemapBucket, SitemapBuckets, SitemapFile, SitemapKind, SitemapTree, classify,
};
use codoseo_core::url::url_hash;
use common::sample_record;

fn page(path: &str, indexability: Indexability, status: u16, depth: Option<u16>) -> PageRecord {
    let url = Url::parse(&format!("https://e.com{path}")).unwrap();
    PageRecord {
        url_hash: url_hash(&url),
        url,
        status,
        depth,
        indexability,
        ..sample_record()
    }
}

fn file(path: &str, kind: SitemapKind, parent: Option<u32>, urls: &[&str]) -> SitemapFile {
    SitemapFile {
        url: Url::parse(&format!("https://e.com{path}")).unwrap(),
        kind: Some(kind),
        parent,
        failed: false,
        listed: urls.len() as u32,
        lastmod: LastmodStats::default(),
        urls: urls.iter().map(|p| format!("https://e.com{p}")).collect(),
    }
}

#[test]
fn each_listed_url_falls_in_one_bucket() {
    use Indexability as I;
    let cases = [
        (page("/", I::Indexable, 200, Some(0)), SitemapBucket::Ok),
        (page("/o", I::Indexable, 200, None), SitemapBucket::Orphan),
        (page("/n", I::Noindex, 200, Some(1)), SitemapBucket::Noindex),
        (
            page("/c", I::Canonicalised, 200, None),
            SitemapBucket::Noindex,
        ),
        (
            page("/r", I::BlockedByRobots, 0, Some(1)),
            SitemapBucket::Noindex,
        ),
        (
            page("/m", I::Redirected, 301, Some(1)),
            SitemapBucket::Redirect,
        ),
        (page("/g", I::ClientError, 404, None), SitemapBucket::Broken),
        (
            page("/s", I::ServerError, 0, Some(2)),
            SitemapBucket::Broken,
        ),
    ];
    for (p, bucket) in &cases {
        assert_eq!(classify(Some(p)), *bucket, "{}", p.url);
    }
    assert_eq!(classify(None), SitemapBucket::NotCrawled);
}

#[test]
fn a_tree_classifies_urls_and_sums_indexes_from_their_children() {
    use Indexability as I;
    let pages = vec![
        page("/", I::Indexable, 200, Some(0)),
        page("/orphan", I::Indexable, 200, None),
        page("/gone", I::ClientError, 404, Some(1)),
        page("/moved", I::Redirected, 301, Some(1)),
        page("/hidden", I::Noindex, 200, Some(1)),
    ];
    let mut failed = file("/missing.xml", SitemapKind::UrlSet, Some(1), &[]);
    failed.kind = None;
    failed.failed = true;
    let files = vec![
        file("/index.xml", SitemapKind::Index, None, &[]),
        file("/nested.xml", SitemapKind::Index, Some(0), &[]),
        file(
            "/a.xml",
            SitemapKind::UrlSet,
            Some(1),
            &["/", "/orphan", "/", "/never"],
        ),
        file(
            "/b.xml",
            SitemapKind::UrlSet,
            Some(0),
            &["/gone", "/moved", "/hidden"],
        ),
        failed,
    ];
    let tree = SitemapTree::build(&files, &pages);
    let a = &tree.files[2];
    assert_eq!(
        a.buckets,
        SitemapBuckets {
            ok: 1,
            orphan: 1,
            not_crawled: 1,
            ..Default::default()
        },
        "a URL listed twice counts once"
    );
    assert_eq!(a.listed, 4);
    assert_eq!(a.samples.orphan, ["https://e.com/orphan"]);
    assert_eq!(a.samples.not_crawled, ["https://e.com/never"]);
    let b = &tree.files[3];
    assert_eq!(
        (b.buckets.broken, b.buckets.redirect, b.buckets.noindex),
        (1, 1, 1)
    );
    assert_eq!(b.samples.broken, ["https://e.com/gone"]);
    assert_eq!(tree.files[1].buckets, a.buckets);
    let root = &tree.files[0].buckets;
    assert_eq!(
        (
            root.ok,
            root.orphan,
            root.broken,
            root.redirect,
            root.noindex,
            root.not_crawled
        ),
        (1, 1, 1, 1, 1, 1)
    );
    assert!(tree.files[4].failed && tree.files[4].kind.is_none());

    let json = serde_json::to_value(&tree).unwrap();
    assert_eq!(json["files"][0]["kind"], "index");
    assert_eq!(json["files"][2]["kind"], "urlset");
    assert_eq!(json["files"][2]["parent"], 1);
    assert!(json["files"][0]["parent"].is_null());
    assert_eq!(json["files"][2]["buckets"]["not_crawled"], 1);
    assert_eq!(serde_json::from_value::<SitemapTree>(json).unwrap(), tree);
}

#[test]
fn samples_stop_at_ten_per_bucket() {
    let urls: Vec<String> = (0..25).map(|i| format!("/p/{i}")).collect();
    let refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    let tree = SitemapTree::build(&[file("/s.xml", SitemapKind::UrlSet, None, &refs)], &[]);
    assert_eq!(tree.files[0].buckets.not_crawled, 25);
    assert_eq!(tree.files[0].samples.not_crawled.len(), 10);
}

#[test]
fn an_output_without_sitemap_files_still_reads() {
    let out = CrawlOutput {
        origin: Url::parse("https://e.com/").unwrap(),
        pages: Vec::new(),
        links: LinkGraph::default(),
        robots: None,
        sitemap: SitemapSummary::default(),
        sitemap_files: vec![file("/s.xml", SitemapKind::UrlSet, None, &["/a"])],
        stop: StopReason::Completed,
        duration_ms: 1,
        signals: SiteSignals::default(),
    };
    let mut json = serde_json::to_value(&out).unwrap();
    assert!(json["sitemap_files"][0].get("urls").is_none());
    json.as_object_mut().unwrap().remove("sitemap_files");
    let read: CrawlOutput = serde_json::from_value(json).unwrap();
    assert!(read.sitemap_files.is_empty());
}

#[test]
fn a_file_reads_without_lastmod_and_never_carries_its_urls() {
    let mut json = serde_json::json!({
        "url": "https://e.com/sitemap.xml",
        "kind": "urlset",
        "parent": null,
        "failed": false,
        "listed": 2
    });
    let f: SitemapFile = serde_json::from_value(json.clone()).unwrap();
    assert!(f.urls.is_empty() && f.lastmod == LastmodStats::default());
    json["urls"] = serde_json::json!(["https://e.com/a"]);
    let f: SitemapFile = serde_json::from_value(json).unwrap();
    assert!(f.urls.is_empty(), "the URLs are never serialized");
}
