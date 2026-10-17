//! The site graph's queries over `pages` and `inlinks` (sitemap overlap, top folders, click
//! depth, broken pages and redirect chains, a crawl diffed against an earlier one), against
//! crawls written by the real `finalize`.

mod support;

use codoseo_core::check::{CheckId, IssueBits};
use codoseo_core::crawl::SitemapSummary;
use codoseo_core::output::{CrawlOutput, Edge, LinkGraph, SiteSignals, StopReason};
use codoseo_core::page::{Indexability, PageFields, PageRecord};
use codoseo_core::report::{CrawlReport, CrawlSummary};
use codoseo_core::url::url_hash;
use codoseo_store::explorer::PageFilter;
use codoseo_store::finalize::finalize;
use codoseo_store::site_graph::{self, top_folder};
use sqlx::PgPool;
use support::TestDb;
use url::Url;
use uuid::Uuid;

const WORKER: &str = "test-worker";

fn url(path: &str) -> Url {
    Url::parse(&format!("https://example.com{path}")).unwrap()
}

fn page(path: &str, depth: Option<u16>, inlinks: u32) -> PageRecord {
    let url = url(path);
    PageRecord {
        url_hash: url_hash(&url),
        url,
        status: 200,
        redirect_chain: Vec::new(),
        response_ms: 10,
        size_bytes: 100,
        content_type: Some("text/html".to_owned()),
        depth,
        in_sitemap: true,
        indexability: Indexability::Indexable,
        fields: PageFields::default(),
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

fn broken(path: &str, depth: Option<u16>, inlinks: u32) -> PageRecord {
    PageRecord {
        status: 404,
        indexability: Indexability::ClientError,
        ..page(path, depth, inlinks)
    }
}

async fn site(pool: &PgPool) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO sites (domain, start_url) VALUES ('example.com', 'https://example.com/') \
         RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A finished crawl of `pages` with links `(from, to, anchor)`.
async fn crawl(pool: &PgPool, site: Uuid, pages: Vec<PageRecord>, links: &[(u32, u32)]) -> Uuid {
    let crawl: Uuid = sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, trigger, priority, status, worker_id) \
         VALUES ($1, 'example.com', 'manual', 2, 'running', $2) RETURNING id",
    )
    .bind(site)
    .bind(WORKER)
    .fetch_one(pool)
    .await
    .unwrap();
    let out = CrawlOutput {
        origin: url("/"),
        pages,
        links: LinkGraph {
            edges: links
                .iter()
                .enumerate()
                .map(|(i, &(from, to))| Edge {
                    from,
                    to,
                    anchor: i as u32,
                    nofollow: false,
                })
                .collect(),
            anchors: (0..links.len()).map(|i| format!("anchor {i}")).collect(),
        },
        robots: None,
        sitemap: SitemapSummary::default(),
        sitemap_files: Vec::new(),
        stop: StopReason::Completed,
        duration_ms: 500,
        signals: SiteSignals::default(),
    };
    let report = CrawlReport {
        health_score: 90,
        checks_passed: 1,
        checks_total: 1,
        counts: Vec::new(),
        inlink_samples: Vec::new(),
        summary: CrawlSummary::default(),
        excluded: Default::default(),
    };
    finalize(pool, crawl, site, WORKER, &out, &report, &[], None)
        .await
        .unwrap();
    crawl
}

#[tokio::test]
async fn overlap_folders_and_depths_count_the_pages() {
    let db = TestDb::new().await;
    let site = site(&db.pool).await;
    let mut orphan = page("/lp/a", None, 0);
    orphan.issues.set_check(CheckId::Orphan);
    let pages = vec![
        page("/", Some(0), 0),
        page("/blog/", Some(1), 1),
        page("/blog/a", Some(4), 1),
        PageRecord {
            in_sitemap: false,
            ..page("/about", Some(1), 1)
        },
        PageRecord {
            indexability: Indexability::Noindex,
            ..page("/hidden", Some(2), 1)
        },
        orphan,
        broken("/blog/gone", Some(2), 2),
    ];
    let c = crawl(&db.pool, site, pages, &[]).await;

    let o = site_graph::sitemap_overlap(&db.pool, c).await.unwrap();
    assert_eq!((o.in_sitemap, o.linked, o.in_both), (6, 6, 5));
    assert_eq!(
        (o.only_sitemap, o.only_linked, o.only_linked_indexable),
        (1, 1, 1)
    );
    assert_eq!(o.both_not_indexable, 2);
    assert_eq!((o.sitemap_noindex, o.sitemap_broken), (1, 1));
    assert_eq!(o.sitemap_not_indexable(), 2);
    assert_eq!(o.orphans, 1);

    // The SQL folder matches `top_folder`: `/blog/` is in /blog, `/about` at the top level.
    let folders = site_graph::top_folders(&db.pool, c, PageFilter::All, 5)
        .await
        .unwrap();
    assert_eq!(
        folders,
        [
            ("".to_owned(), 3),
            ("/blog".to_owned(), 3),
            ("/lp".to_owned(), 1)
        ]
    );
    assert_eq!(top_folder("/blog/"), "/blog");

    let buckets = site_graph::depth_buckets(&db.pool, c).await.unwrap();
    let depths: Vec<_> = buckets.iter().map(|b| (b.depth, b.total())).collect();
    assert_eq!(
        depths,
        [
            (Some(0), 1),
            (Some(1), 2),
            (Some(2), 2),
            (Some(4), 1),
            (None, 1)
        ]
    );
    assert_eq!((buckets[2].noindex, buckets[2].broken), (1, 1));

    let grid = site_graph::folder_depths(&db.pool, c, 3).await.unwrap();
    assert!(grid.contains(&("/blog".to_owned(), 3, 1)), "{grid:?}");
    assert!(grid.contains(&("".to_owned(), 1, 1)));

    let deep = site_graph::deep_pages(&db.pool, c, 4, 10).await.unwrap();
    assert_eq!(deep.len(), 1);
    assert_eq!(deep[0].url, "https://example.com/blog/a");
    let urls = site_graph::urls_by_hash(&db.pool, c, &[deep[0].url_hash])
        .await
        .unwrap();
    assert_eq!(urls[&deep[0].url_hash], "https://example.com/blog/a");
}

#[tokio::test]
async fn broken_pages_and_redirect_chains_carry_their_links() {
    let db = TestDb::new().await;
    let site = site(&db.pool).await;
    let pages = vec![
        page("/", Some(0), 0),
        page("/docs/a", Some(1), 1),
        broken("/docs/rate-limits", Some(1), 2),
        page("/docs/limits", Some(1), 0),
        PageRecord {
            status: 301,
            indexability: Indexability::Redirected,
            redirect_chain: vec![(301, url("/old")), (302, url("/older"))],
            redirect_target: Some(url("/docs/rate-limits")),
            ..page("/old", Some(1), 1)
        },
        PageRecord {
            status: 301,
            indexability: Indexability::Redirected,
            redirect_chain: vec![(301, url("/moved"))],
            redirect_target: Some(url("/docs/a")),
            ..page("/moved", Some(1), 0)
        },
    ];
    let c = crawl(&db.pool, site, pages, &[(0, 2), (1, 2), (0, 4)]).await;

    let t = site_graph::link_totals(&db.pool, c).await.unwrap();
    assert_eq!((t.broken, t.broken_links), (1, 2));
    assert_eq!((t.redirects, t.redirect_links), (2, 1));
    assert_eq!((t.longest_chain, t.ends_broken), (2, 1));

    let b = site_graph::broken_pages(&db.pool, c, 5).await.unwrap();
    assert_eq!(b[0].status, 404);
    let sources = site_graph::link_sources(&db.pool, c, b[0].url_hash, 5)
        .await
        .unwrap();
    let from: Vec<_> = sources.iter().map(|s| s.from_url.as_str()).collect();
    assert_eq!(from, ["https://example.com/", "https://example.com/docs/a"]);
    assert_eq!(sources[0].anchor.as_deref(), Some("anchor 0"));

    let stand_ins = site_graph::stand_ins(&db.pool, c, "https://example.com/docs/", "x_y", 10)
        .await
        .unwrap();
    assert_eq!(
        stand_ins,
        [
            "https://example.com/docs/a",
            "https://example.com/docs/limits"
        ]
    );

    let chains = site_graph::redirect_chains(&db.pool, c, 5).await.unwrap();
    assert_eq!(chains.len(), 2);
    assert_eq!(chains[0].url, "https://example.com/old");
    assert_eq!(
        chains[0].hops[1],
        (302, "https://example.com/older".to_owned())
    );
    assert_eq!(chains[0].target_status, Some(404));
    assert_eq!(chains[1].target_status, Some(200));
}

#[tokio::test]
async fn page_diffs_pair_urls_across_two_crawls() {
    let db = TestDb::new().await;
    let site = site(&db.pool).await;
    let before = crawl(
        &db.pool,
        site,
        vec![
            page("/", Some(0), 0),
            page("/same", Some(1), 1),
            page("/deeper", Some(2), 1),
            page("/gone", Some(1), 1),
        ],
        &[],
    )
    .await;
    let after = crawl(
        &db.pool,
        site,
        vec![
            page("/", Some(0), 0),
            page("/same", Some(1), 1),
            page("/deeper", Some(4), 1),
            broken("/new", None, 0),
        ],
        &[],
    )
    .await;
    assert_eq!(site_graph::page_count(&db.pool, before).await.unwrap(), 4);

    let diffs = site_graph::page_diffs(&db.pool, before, after, 100)
        .await
        .unwrap();
    let urls: Vec<_> = diffs.iter().map(|d| d.url.as_str()).collect();
    assert_eq!(
        urls,
        [
            "https://example.com/deeper",
            "https://example.com/gone",
            "https://example.com/new"
        ]
    );
    let deeper = &diffs[0];
    assert_eq!(deeper.before.unwrap().depth, Some(2));
    assert_eq!(deeper.after.unwrap().depth, Some(4));
    assert!(diffs[1].after.is_none() && diffs[1].before.is_some());
    let new = diffs[2].after.unwrap();
    assert!(diffs[2].before.is_none());
    assert_eq!(
        (new.status, new.indexability),
        (404, Indexability::ClientError)
    );
}
