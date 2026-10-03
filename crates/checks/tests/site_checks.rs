mod common;

use codoseo_checks::{MAX_INLINK_SAMPLES, check_page, run_checks};
use codoseo_core::check::{CheckId, IssueBits};
use codoseo_core::output::{Edge, StopReason};
use codoseo_core::page::{FetchFailure, Indexability};
use common::{home, out, pg, u};

fn broken(path: &str) -> codoseo_core::page::PageRecord {
    let mut p = pg(path);
    p.status = 404;
    p.indexability = Indexability::ClientError;
    p
}

#[test]
fn inlinks_count_unique_sources_without_self_links() {
    let mut o = out(
        vec![home(), pg("/a"), pg("/b")],
        &[(0, 1), (0, 1), (2, 1), (1, 1)],
    );
    run_checks(&mut o);
    assert_eq!(o.pages[1].inlinks, 2);
}

#[test]
fn duplicate_titles_flag_every_page_in_the_group() {
    let a = pg("/a");
    let mut b = pg("/b");
    let mut noindex_copy = pg("/c");
    let shared = a.fields.title.clone();
    b.fields.title = shared.clone();
    noindex_copy.fields.title = shared;
    noindex_copy.indexability = Indexability::Noindex;
    let mut o = out(vec![home(), a, b, noindex_copy], &[(0, 1), (0, 2), (0, 3)]);
    run_checks(&mut o);
    assert!(o.pages[1].issues.has_check(CheckId::TitleDuplicate));
    assert!(o.pages[2].issues.has_check(CheckId::TitleDuplicate));
    assert!(!o.pages[3].issues.has_check(CheckId::TitleDuplicate));
    assert!(!o.pages[0].issues.has_check(CheckId::TitleDuplicate));
}

#[test]
fn duplicate_title_ignores_case_and_surrounding_space() {
    let a = pg("/a");
    let mut b = pg("/b");
    b.fields.title = a
        .fields
        .title
        .as_ref()
        .map(|t| format!("  {}  ", t.to_uppercase()));
    let mut o = out(vec![home(), a, b], &[(0, 1), (0, 2)]);
    run_checks(&mut o);
    assert!(o.pages[1].issues.has_check(CheckId::TitleDuplicate));
    assert!(o.pages[2].issues.has_check(CheckId::TitleDuplicate));
}

#[test]
fn duplicate_description_h1_and_content() {
    let a = pg("/a");
    let mut b = pg("/b");
    b.fields.meta_description = a.fields.meta_description.clone();
    b.fields.h1 = vec![format!("  {}", a.fields.h1[0].to_uppercase())];
    b.fields.content_hash = a.fields.content_hash;
    let mut o = out(vec![home(), a, b], &[(0, 1), (0, 2)]);
    run_checks(&mut o);
    for i in [1, 2] {
        let bits = o.pages[i].issues;
        assert!(bits.has_check(CheckId::DescriptionDuplicate));
        assert!(bits.has_check(CheckId::H1Duplicate));
        assert!(bits.has_check(CheckId::ContentDuplicate));
        assert!(!bits.has_check(CheckId::TitleDuplicate));
    }
}

#[test]
fn link_to_404_flags_the_linking_page() {
    let mut o = out(vec![home(), broken("/gone")], &[(0, 1)]);
    run_checks(&mut o);
    assert!(o.pages[0].issues.has_check(CheckId::LinksToBroken));
    assert!(o.pages[1].issues.has_check(CheckId::Http4xx));
    assert!(!o.pages[1].issues.has_check(CheckId::LinksToBroken));
}

#[test]
fn link_to_failed_fetch_or_5xx_is_broken_and_to_redirect_is_not() {
    let mut failed = pg("/failed");
    failed.status = 0;
    failed.error = Some(FetchFailure::Timeout);
    let mut server = pg("/server");
    server.status = 503;
    let mut moved = pg("/moved");
    moved.status = 301;
    moved.indexability = Indexability::Redirected;
    let mut o = out(
        vec![home(), failed, server, moved, pg("/x"), pg("/y"), pg("/z")],
        &[(0, 1), (4, 2), (5, 3), (6, 6)],
    );
    run_checks(&mut o);
    assert!(o.pages[0].issues.has_check(CheckId::LinksToBroken));
    assert!(o.pages[4].issues.has_check(CheckId::LinksToBroken));
    assert!(!o.pages[5].issues.has_check(CheckId::LinksToBroken));
    assert!(o.pages[5].issues.has_check(CheckId::LinksToRedirect));
    assert!(!o.pages[0].issues.has_check(CheckId::LinksToRedirect));
    assert!(!o.pages[6].issues.has_check(CheckId::LinksToBroken));
}

#[test]
fn orphan_is_a_sitemap_page_with_no_inlinks() {
    let mut orphan = pg("/orphan");
    orphan.depth = None;
    let mut o = out(vec![home(), pg("/a"), orphan], &[(0, 1)]);
    run_checks(&mut o);
    assert!(o.pages[2].issues.has_check(CheckId::Orphan));
    assert!(
        !o.pages[0].issues.has_check(CheckId::Orphan),
        "the origin page is never an orphan"
    );
    assert!(!o.pages[1].issues.has_check(CheckId::Orphan));
}

#[test]
fn page_outside_the_sitemap_is_not_an_orphan() {
    let mut p = pg("/a");
    p.in_sitemap = false;
    let mut o = out(vec![home(), p], &[]);
    run_checks(&mut o);
    assert!(!o.pages[1].issues.has_check(CheckId::Orphan));
}

#[test]
fn sitemap_page_that_redirects() {
    let mut p = pg("/a");
    p.status = 301;
    p.indexability = Indexability::Redirected;
    let mut o = out(vec![home(), p], &[(0, 1)]);
    run_checks(&mut o);
    assert!(o.pages[1].issues.has_check(CheckId::SitemapNon200));
}

#[test]
fn sitemap_page_that_is_noindex_or_canonicalised() {
    let mut noindex = pg("/n");
    noindex.indexability = Indexability::Noindex;
    let mut canon = pg("/c");
    canon.indexability = Indexability::Canonicalised;
    let mut o = out(vec![home(), noindex, canon], &[(0, 1), (0, 2)]);
    run_checks(&mut o);
    assert!(o.pages[1].issues.has_check(CheckId::SitemapNoindex));
    assert!(!o.pages[1].issues.has_check(CheckId::SitemapCanonicalised));
    assert!(o.pages[2].issues.has_check(CheckId::SitemapCanonicalised));
    assert!(!o.pages[2].issues.has_check(CheckId::SitemapNoindex));
}

#[test]
fn indexable_page_missing_from_a_non_empty_sitemap() {
    let mut p = pg("/a");
    p.in_sitemap = false;
    let mut o = out(vec![home(), p], &[(0, 1)]);
    run_checks(&mut o);
    assert!(o.pages[1].issues.has_check(CheckId::NotInSitemap));
    assert!(!o.pages[0].issues.has_check(CheckId::NotInSitemap));

    o.sitemap.url_count = 0;
    run_checks(&mut o);
    assert!(!o.pages[1].issues.has_check(CheckId::NotInSitemap));
}

#[test]
fn canonical_to_404() {
    let mut p = pg("/a");
    p.fields.canonical = Some(u("https://e.com/gone"));
    p.indexability = Indexability::Canonicalised;
    let mut o = out(vec![home(), p, broken("/gone")], &[(0, 1), (0, 2)]);
    run_checks(&mut o);
    assert!(o.pages[1].issues.has_check(CheckId::CanonicalToNon200));
    assert!(!o.pages[2].issues.has_check(CheckId::CanonicalToNon200));
}

#[test]
fn canonical_to_a_good_or_uncrawled_page_is_fine() {
    let mut p = pg("/a");
    p.fields.canonical = Some(u("https://e.com/b"));
    p.indexability = Indexability::Canonicalised;
    let mut q = pg("/q");
    q.fields.canonical = Some(u("https://e.com/never-crawled"));
    q.indexability = Indexability::Canonicalised;
    let mut o = out(vec![home(), p, pg("/b"), q], &[(0, 1), (0, 2), (0, 3)]);
    run_checks(&mut o);
    assert!(!o.pages[1].issues.has_check(CheckId::CanonicalToNon200));
    assert!(!o.pages[3].issues.has_check(CheckId::CanonicalToNon200));
}

#[test]
fn site_wide_checks() {
    let mut o = out(vec![home()], &[]);
    o.stop = StopReason::RobotsBlocked;
    o.sitemap.url_count = 0;
    let r = run_checks(&mut o);
    assert!(r.counts.contains(&(CheckId::RobotsBlocksSite, 1)));
    assert!(r.counts.contains(&(CheckId::SitemapMissing, 1)));
    assert_eq!(
        o.pages[0].issues.0 & ((1 << 11) | (1 << 37)),
        0,
        "site-wide checks set no page bit"
    );
}

#[test]
fn samples_cap_at_20_but_keep_every_broken_link() {
    // page 0 = home, 1 = /popular (200), 2 = /gone (404), then 30 linkers to each
    let mut pages = vec![home(), pg("/popular"), broken("/gone")];
    let mut edges = vec![];
    for i in 0..60u32 {
        pages.push(pg(&format!("/l{i}")));
        let from = 3 + i;
        edges.push((from, if i < 30 { 1 } else { 2 }));
    }
    let mut o = out(pages, &edges);
    let r = run_checks(&mut o);
    assert_eq!(MAX_INLINK_SAMPLES, 20);
    assert_eq!(
        r.inlink_samples.iter().filter(|s| s.target == 1).count(),
        20
    );
    assert_eq!(
        r.inlink_samples.iter().filter(|s| s.target == 2).count(),
        30
    );
}

#[test]
fn samples_copy_anchor_and_flags_and_skip_self_links() {
    let mut o = out(vec![home(), pg("/a")], &[]);
    o.links.anchors = vec!["first".into(), "second".into()];
    o.links.edges = vec![
        Edge {
            from: 0,
            to: 1,
            anchor: 1,
            nofollow: true,
        },
        Edge {
            from: 1,
            to: 1,
            anchor: 0,
            nofollow: false,
        },
    ];
    let r = run_checks(&mut o);
    assert_eq!(r.inlink_samples.len(), 1);
    let s = &r.inlink_samples[0];
    assert_eq!(
        (s.source, s.target, s.anchor.as_str(), s.nofollow),
        (0, 1, "second", true)
    );
}

#[test]
fn nav_links_on_every_page_count_once_per_source() {
    // 5 nav pages (1..=5), 200 content pages (6..=205), two edges from each to each nav page
    let mut pages = vec![home()];
    pages.extend((0..5).map(|i| pg(&format!("/nav{i}"))));
    pages.extend((0..200).map(|i| pg(&format!("/c{i}"))));
    let mut edges = vec![];
    for n in 1..=5u32 {
        edges.push((n, n));
    }
    for c in 6..206u32 {
        for n in 1..=5u32 {
            edges.push((c, n));
            edges.push((c, n));
        }
    }
    let mut o = out(pages, &edges);
    let r = run_checks(&mut o);
    for n in 1..=5usize {
        assert_eq!(o.pages[n].inlinks, 200);
    }
    // samples stay bounded however many edges the navigation adds
    assert_eq!(r.inlink_samples.len(), 5 * MAX_INLINK_SAMPLES);
}

#[test]
fn run_checks_is_idempotent() {
    let mut a = pg("/a");
    a.status = 404;
    let mut o = out(vec![home(), a, pg("/b")], &[(0, 1), (2, 1)]);
    let first = run_checks(&mut o);
    let pages = o.pages.clone();
    let second = run_checks(&mut o);
    assert_eq!(first, second);
    assert_eq!(pages, o.pages);
}

#[test]
fn summary_counts_statuses_depth_and_response_time() {
    let mut failed = pg("/failed");
    failed.status = 0;
    failed.error = Some(FetchFailure::Timeout);
    failed.response_ms = 0;
    failed.indexability = Indexability::ServerError;
    let mut blocked = pg("/blocked");
    blocked.status = 0;
    blocked.indexability = Indexability::BlockedByRobots;
    let mut moved = pg("/moved");
    moved.status = 301;
    moved.indexability = Indexability::Redirected;
    moved.response_ms = 200;
    let mut server = pg("/server");
    server.status = 500;
    server.response_ms = 300;
    server.indexability = Indexability::ServerError;
    let mut deep = pg("/deep");
    deep.depth = Some(12);
    let mut nodepth = pg("/nodepth");
    nodepth.depth = None;
    let pages = vec![
        home(),
        broken("/gone"),
        failed,
        blocked,
        moved,
        server,
        deep,
        nodepth,
    ];
    let mut o = out(pages, &[]);
    let s = run_checks(&mut o).summary;
    assert_eq!(s.pages, 8);
    assert_eq!(s.indexable, 3);
    assert_eq!(
        (
            s.status.ok,
            s.status.redirect,
            s.status.client_error,
            s.status.server_error,
            s.status.failed,
            s.status.blocked
        ),
        (3, 1, 1, 1, 1, 1)
    );
    // depths: home 0, six pages at the default depth 1, one at 12 (10+), one without
    let mut expected = vec![0u32; 11];
    expected[0] = 1;
    expected[1] = 5;
    expected[10] = 1;
    assert_eq!(s.depth, expected);
    assert_eq!(s.no_depth, 1);
    // responses: 120 (home) + 120 (gone) + 200 + 300 + 120 (deep) + 120 (nodepth) over 6
    assert_eq!(s.avg_response_ms, (120 + 120 + 200 + 300 + 120 + 120) / 6);
}

#[test]
fn empty_crawl_summary_is_empty() {
    let mut o = out(vec![], &[]);
    let r = run_checks(&mut o);
    assert!(r.summary.depth.is_empty());
    assert_eq!((r.summary.pages, r.summary.avg_response_ms), (0, 0));
}

#[test]
fn counts_are_in_check_id_order_and_checks_passed_follows() {
    let mut a = pg("/a");
    a.status = 404;
    a.indexability = Indexability::ClientError;
    let mut o = out(vec![home(), a], &[(0, 1)]);
    let r = run_checks(&mut o);
    let ids: Vec<u8> = r.counts.iter().map(|(c, _)| c.bit()).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]));
    assert!(r.counts.iter().all(|(_, n)| *n > 0));
    assert_eq!(r.checks_total, 44);
    assert_eq!(usize::from(r.checks_passed), 44 - r.counts.len());
}

#[test]
fn check_page_runs_page_checks_only() {
    let mut p = pg("/a");
    p.status = 404;
    p.issues = IssueBits(1 << 40);
    check_page(&mut p);
    assert!(p.issues.has_check(CheckId::Http4xx));
    assert!(!p.issues.has_check(CheckId::SlowResponse));
}
