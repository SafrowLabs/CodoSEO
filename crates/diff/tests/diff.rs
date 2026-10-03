use std::collections::HashSet;

use codoseo_core::change::{Change, ChangeKind};
use codoseo_core::check::{IssueBits, Severity};
use codoseo_core::crawl::{RobotsFile, SitemapSummary};
use codoseo_core::output::StopReason;
use codoseo_core::page::{FetchFailure, Indexability, PageFields, PageRecord};
use codoseo_core::snapshot::Snapshot;
use codoseo_core::url::url_hash;
use codoseo_diff::{diff, key_pages, robots_fingerprint};
use url::Url;

fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn none() -> HashSet<u64> {
    HashSet::new()
}

fn rehash(mut p: PageRecord) -> PageRecord {
    p.key_hash = p.compute_key_hash();
    p
}

/// A clean page: indexable for 2xx, with the given title.
fn page(url: &str, status: u16, title: &str) -> PageRecord {
    let url = u(url);
    let indexability = match status {
        0 => Indexability::ServerError,
        300..=399 => Indexability::Redirected,
        400..=499 => Indexability::ClientError,
        500.. => Indexability::ServerError,
        _ => Indexability::Indexable,
    };
    rehash(PageRecord {
        url_hash: url_hash(&url),
        url,
        status,
        redirect_chain: vec![],
        response_ms: 10,
        size_bytes: 100,
        content_type: Some("text/html".into()),
        depth: Some(1),
        in_sitemap: false,
        indexability,
        fields: PageFields {
            title: if title.is_empty() {
                None
            } else {
                Some(title.into())
            },
            ..PageFields::default()
        },
        inlinks: 1,
        outlinks_internal: 0,
        outlinks_external: 0,
        issues: IssueBits::default(),
        key_hash: 0,
        redirect_target: None,
        outlinks_nofollow: 0,
        error: None,
    })
}

fn snap(origin: &str, pages: Vec<PageRecord>) -> Snapshot {
    Snapshot {
        origin: u(origin),
        stop: StopReason::Completed,
        pages,
        robots: None,
        sitemap: SitemapSummary {
            complete: true,
            ..SitemapSummary::default()
        },
    }
}

/// An origin page plus /a and /b.
fn site_at(origin: &str) -> Snapshot {
    snap(
        origin,
        vec![
            page(origin, 200, "Home"),
            page(&format!("{origin}a"), 200, "A"),
            page(&format!("{origin}b"), 200, "B"),
        ],
    )
}

/// An origin page plus `n` pages `/p0`, `/p1`, ...
fn site_with(n: usize, stop: StopReason) -> Snapshot {
    let mut pages = vec![page("https://e.com/", 200, "Home")];
    pages.extend((0..n).map(|i| page(&format!("https://e.com/p{i}"), 200, &format!("P{i}"))));
    Snapshot {
        stop,
        ..snap("https://e.com/", pages)
    }
}

/// Edits the page at `url` and recomputes its key hash.
fn edit(s: &mut Snapshot, url: &str, f: impl FnOnce(&mut PageRecord)) {
    let p = s.pages.iter_mut().find(|p| p.url.as_str() == url).unwrap();
    f(p);
    p.key_hash = p.compute_key_hash();
}

fn kinds(changes: &[Change]) -> Vec<ChangeKind> {
    changes.iter().map(|c| c.kind).collect()
}

fn only(changes: Vec<Change>) -> Change {
    assert_eq!(changes.len(), 1, "{changes:?}");
    changes.into_iter().next().unwrap()
}

fn robots(status: u16, body: &str) -> Option<RobotsFile> {
    Some(RobotsFile {
        status,
        body: body.into(),
        hash: 0,
    })
}

fn sitemap(count: u32) -> SitemapSummary {
    SitemapSummary {
        url_count: count,
        complete: true,
        ..SitemapSummary::default()
    }
}

#[test]
fn identical_snapshots_have_no_changes() {
    let s = site_at("https://e.com/");
    assert!(diff(&s, &s.clone(), &none()).is_empty());
}

#[test]
fn status_200_to_404_is_a_warning() {
    let prev = site_at("https://e.com/");
    let mut curr = prev.clone();
    edit(&mut curr, "https://e.com/a", |p| {
        p.status = 404;
        p.indexability = Indexability::ClientError;
    });
    let c = only(diff(&prev, &curr, &none()));
    assert_eq!(
        (c.kind, c.severity),
        (ChangeKind::StatusChanged, Severity::Warning)
    );
    assert_eq!(c.url, Some(u("https://e.com/a")));
    assert_eq!((c.before.as_str(), c.after.as_str()), ("200", "404"));
}

#[test]
fn status_change_on_a_key_page_is_critical() {
    let prev = site_at("https://e.com/");
    let mut curr = prev.clone();
    edit(&mut curr, "https://e.com/a", |p| {
        p.status = 404;
        p.indexability = Indexability::ClientError;
    });
    let key: HashSet<u64> = [url_hash(&u("https://e.com/a"))].into();
    let c = only(diff(&prev, &curr, &key));
    assert_eq!(
        (c.kind, c.severity),
        (ChangeKind::StatusChanged, Severity::Critical)
    );
}

#[test]
fn the_origin_page_is_always_a_key_page() {
    let prev = site_at("https://e.com/");
    let mut curr = prev.clone();
    edit(&mut curr, "https://e.com/", |p| {
        p.status = 500;
        p.indexability = Indexability::ServerError;
    });
    let c = only(diff(&prev, &curr, &none()));
    assert_eq!(c.severity, Severity::Critical);
}

#[test]
fn status_change_that_is_not_into_an_error_is_a_notice() {
    let prev = site_at("https://e.com/");
    let mut curr = prev.clone();
    edit(&mut curr, "https://e.com/a", |p| {
        p.status = 301;
        p.indexability = Indexability::Redirected;
    });
    // 404 to 500 stays an error, and is a notice too.
    let mut prev2 = prev.clone();
    edit(&mut prev2, "https://e.com/b", |p| p.status = 404);
    let mut curr2 = prev.clone();
    edit(&mut curr2, "https://e.com/b", |p| p.status = 500);
    assert_eq!(only(diff(&prev, &curr, &none())).severity, Severity::Notice);
    assert_eq!(
        only(diff(&prev2, &curr2, &none())).severity,
        Severity::Notice
    );
}

#[test]
fn robots_blocked_record_is_not_an_error_state() {
    // Status 0 without an error is a robots-blocked record: moving into 404 raises a warning.
    let mut prev = site_at("https://e.com/");
    edit(&mut prev, "https://e.com/a", |p| {
        p.status = 0;
        p.indexability = Indexability::BlockedByRobots;
    });
    let mut curr = prev.clone();
    edit(&mut curr, "https://e.com/a", |p| p.status = 404);
    assert_eq!(
        only(diff(&prev, &curr, &none())).severity,
        Severity::Warning
    );
    // And status 0 with a fetch error is an error: 200 to failed is a warning.
    let prev = site_at("https://e.com/");
    let mut curr = prev.clone();
    edit(&mut curr, "https://e.com/a", |p| {
        p.status = 0;
        p.error = Some(FetchFailure::Timeout);
    });
    let c = only(diff(&prev, &curr, &none()));
    assert_eq!((c.severity, c.after.as_str()), (Severity::Warning, "0"));
}

#[test]
fn became_noindex() {
    let prev = site_at("https://e.com/");
    let mut curr = prev.clone();
    edit(&mut curr, "https://e.com/a", |p| {
        p.indexability = Indexability::Noindex;
        p.fields.meta_robots = Some("noindex".into());
    });
    let c = only(diff(&prev, &curr, &none()));
    assert_eq!(
        (c.kind, c.severity),
        (ChangeKind::BecameNoindex, Severity::Warning)
    );
    // Going the other way is not reported.
    assert!(diff(&curr, &prev, &none()).is_empty());
}

#[test]
fn title_changed_and_removed() {
    let prev = site_at("https://e.com/");
    let mut curr = prev.clone();
    edit(&mut curr, "https://e.com/a", |p| {
        p.fields.title = Some("New A".into())
    });
    edit(&mut curr, "https://e.com/b", |p| p.fields.title = None);
    let c = diff(&prev, &curr, &none());
    assert_eq!(
        kinds(&c),
        vec![ChangeKind::TitleRemoved, ChangeKind::TitleChanged]
    );
    assert_eq!(c[0].severity, Severity::Warning);
    assert_eq!((c[0].before.as_str(), c[0].after.as_str()), ("B", ""));
    assert_eq!(c[1].severity, Severity::Notice);
    assert_eq!((c[1].before.as_str(), c[1].after.as_str()), ("A", "New A"));
}

#[test]
fn blank_title_counts_as_absent_and_whitespace_is_ignored() {
    let prev = site_at("https://e.com/");
    let mut curr = prev.clone();
    edit(&mut curr, "https://e.com/a", |p| {
        p.fields.title = Some("  ".into())
    });
    edit(&mut curr, "https://e.com/b", |p| {
        p.fields.title = Some(" B ".into())
    });
    assert_eq!(
        kinds(&diff(&prev, &curr, &none())),
        vec![ChangeKind::TitleRemoved]
    );
    // A title appearing where there was none is not a change of kind TitleChanged.
    assert!(
        diff(&curr, &prev, &none())
            .iter()
            .all(|c| c.kind != ChangeKind::TitleChanged)
    );
}

#[test]
fn canonical_changed() {
    let prev = site_at("https://e.com/");
    let mut curr = prev.clone();
    edit(&mut curr, "https://e.com/a", |p| {
        p.fields.canonical = Some(u("https://e.com/b"))
    });
    let c = only(diff(&prev, &curr, &none()));
    assert_eq!(
        (c.kind, c.severity),
        (ChangeKind::CanonicalChanged, Severity::Notice)
    );
    assert_eq!(
        (c.before.as_str(), c.after.as_str()),
        ("none", "https://e.com/b")
    );
}

#[test]
fn redirect_chain_grew() {
    let mut prev = site_at("https://e.com/");
    edit(&mut prev, "https://e.com/a", |p| {
        p.status = 301;
        p.redirect_chain = vec![(301, u("https://e.com/a"))];
        p.redirect_target = Some(u("https://e.com/b"));
    });
    let mut curr = prev.clone();
    edit(&mut curr, "https://e.com/a", |p| {
        p.redirect_chain = vec![(301, u("https://e.com/a")), (301, u("https://e.com/x"))];
        p.redirect_target = Some(u("https://e.com/c"));
    });
    let c = diff(&prev, &curr, &none());
    let c = c
        .iter()
        .find(|c| c.kind == ChangeKind::RedirectChainGrew)
        .unwrap();
    assert_eq!(c.severity, Severity::Notice);
    assert_eq!((c.before.as_str(), c.after.as_str()), ("1", "2"));
    // A shorter chain is not reported.
    assert!(
        diff(&curr, &prev, &none())
            .iter()
            .all(|c| c.kind != ChangeKind::RedirectChainGrew)
    );
}

#[test]
fn redirect_chain_grew_with_the_same_target_and_key_hash() {
    let mut prev = site_at("https://e.com/");
    edit(&mut prev, "https://e.com/a", |p| {
        p.status = 301;
        p.redirect_chain = vec![(301, u("https://e.com/a"))];
        p.redirect_target = Some(u("https://e.com/b"));
    });
    let mut curr = prev.clone();
    edit(&mut curr, "https://e.com/a", |p| {
        p.redirect_chain = vec![(301, u("https://e.com/a")), (301, u("https://e.com/x"))];
    });
    // The chain length is not part of the key hash.
    assert_eq!(prev.pages[1].key_hash, curr.pages[1].key_hash);
    let c = only(diff(&prev, &curr, &none()));
    assert_eq!(c.kind, ChangeKind::RedirectChainGrew);
    assert_eq!((c.before.as_str(), c.after.as_str()), ("1", "2"));
}

#[test]
fn new_and_removed_urls() {
    let prev = site_at("https://e.com/");
    let mut curr = prev.clone();
    curr.pages.retain(|p| p.url.path() != "/b");
    curr.pages.push(page("https://e.com/c", 200, "C"));
    let c = diff(&prev, &curr, &none());
    assert_eq!(kinds(&c), vec![ChangeKind::NewUrl, ChangeKind::RemovedUrl]);
    assert!(c.iter().all(|c| c.severity == Severity::Notice));
    assert_eq!(c[0].url, Some(u("https://e.com/c")));
    assert_eq!(c[1].url, Some(u("https://e.com/b")));
}

#[test]
fn new_or_removed_key_page_is_a_warning() {
    let prev = site_at("https://e.com/");
    let mut curr = prev.clone();
    curr.pages.retain(|p| p.url.path() != "/b");
    let key: HashSet<u64> = [url_hash(&u("https://e.com/b"))].into();
    let c = only(diff(&prev, &curr, &key));
    assert_eq!(
        (c.kind, c.severity),
        (ChangeKind::RemovedUrl, Severity::Warning)
    );
}

#[test]
fn early_stop_does_not_flood_removed_urls() {
    let prev = site_with(50, StopReason::Completed);
    let curr = site_with(10, StopReason::PageLimit);
    assert!(
        diff(&prev, &curr, &none())
            .iter()
            .all(|c| c.kind != ChangeKind::RemovedUrl)
    );
    assert!(
        diff(&curr, &prev, &none())
            .iter()
            .all(|c| c.kind != ChangeKind::NewUrl)
    );
    // The complete-to-complete comparison does report them.
    let curr = site_with(10, StopReason::Completed);
    assert_eq!(
        diff(&prev, &curr, &none())
            .iter()
            .filter(|c| c.kind == ChangeKind::RemovedUrl)
            .count(),
        40
    );
}

#[test]
fn robots_comment_or_sitemap_line_change_is_ignored() {
    assert_eq!(
        robots_fingerprint("User-agent: *\nDisallow: /a # old\nSitemap: https://a/s.xml"),
        robots_fingerprint("user-agent: *\n\nDisallow: /a\nSitemap: https://b/s.xml")
    );
}

#[test]
fn robots_rule_change_changes_the_fingerprint() {
    assert_ne!(
        robots_fingerprint("User-agent: *\nDisallow: /a"),
        robots_fingerprint("User-agent: *\nDisallow: /b")
    );
    // Values keep their case.
    assert_ne!(
        robots_fingerprint("User-agent: *\nDisallow: /A"),
        robots_fingerprint("User-agent: *\nDisallow: /a")
    );
}

#[test]
fn robots_rule_change_is_reported() {
    let mut prev = site_at("https://e.com/");
    prev.robots = robots(200, "User-agent: *\nDisallow: /a\n");
    let mut curr = prev.clone();
    curr.robots = robots(200, "User-agent: *\nDisallow: /\n");
    let c = only(diff(&prev, &curr, &none()));
    assert_eq!(
        (c.kind, c.severity),
        (ChangeKind::RobotsTxtChanged, Severity::Warning)
    );
    assert_eq!(c.url, None);
    assert_eq!(
        (c.before.as_str(), c.after.as_str()),
        ("200", "200 (rules changed)")
    );
    // Comment-only edits are not reported.
    curr.robots = robots(
        200,
        "# hi\nUser-agent: *\nDisallow: /a\nSitemap: https://e.com/s.xml",
    );
    assert!(diff(&prev, &curr, &none()).is_empty());
}

#[test]
fn robots_status_change_and_presence_are_reported() {
    let mut prev = site_at("https://e.com/");
    prev.robots = robots(200, "User-agent: *\nDisallow:\n");
    let mut curr = prev.clone();
    curr.robots = robots(404, "");
    let c = only(diff(&prev, &curr, &none()));
    assert_eq!(c.kind, ChangeKind::RobotsTxtChanged);
    assert_eq!((c.before.as_str(), c.after.as_str()), ("200", "404"));

    curr.robots = None;
    let c = only(diff(&prev, &curr, &none()));
    assert_eq!((c.before.as_str(), c.after.as_str()), ("200", "none"));

    // Neither side has one: nothing to report.
    prev.robots = None;
    assert!(diff(&prev, &curr, &none()).is_empty());
}

#[test]
fn sitemap_shrank_10_percent() {
    let mut prev = site_at("https://e.com/");
    prev.sitemap = sitemap(100);
    let mut curr = prev.clone();

    curr.sitemap = sitemap(89);
    let c = only(diff(&prev, &curr, &none()));
    assert_eq!(
        (c.kind, c.severity),
        (ChangeKind::SitemapShrank, Severity::Warning)
    );
    assert_eq!((c.before.as_str(), c.after.as_str()), ("100", "89"));

    curr.sitemap = sitemap(90);
    assert_eq!(
        kinds(&diff(&prev, &curr, &none())),
        vec![ChangeKind::SitemapShrank]
    );

    curr.sitemap = sitemap(91);
    assert!(diff(&prev, &curr, &none()).is_empty());

    curr.sitemap = SitemapSummary {
        failed_files: 1,
        ..sitemap(10)
    };
    assert!(diff(&prev, &curr, &none()).is_empty());
    let mut failed_prev = prev.clone();
    failed_prev.sitemap.failed_files = 1;
    curr.sitemap = sitemap(10);
    assert!(diff(&failed_prev, &curr, &none()).is_empty());

    curr.sitemap = SitemapSummary {
        complete: false,
        ..sitemap(10)
    };
    assert!(diff(&prev, &curr, &none()).is_empty());

    // An empty previous sitemap can't shrink.
    prev.sitemap = sitemap(0);
    curr.sitemap = sitemap(0);
    assert!(diff(&prev, &curr, &none()).is_empty());
}

/// `pages` pages in total; the first `errors` of the non-origin pages are 404 in the second crawl.
fn spike_pair(pages: usize, errors: usize) -> (Snapshot, Snapshot) {
    let prev = site_with(pages - 1, StopReason::Completed);
    let mut curr = prev.clone();
    for i in 0..errors {
        edit(&mut curr, &format!("https://e.com/p{i}"), |p| {
            p.status = 404;
            p.indexability = Indexability::ClientError;
        });
    }
    (prev, curr)
}

fn spikes(changes: &[Change]) -> Vec<&Change> {
    changes
        .iter()
        .filter(|c| c.kind == ChangeKind::ErrorSpike)
        .collect()
}

#[test]
fn error_spike_threshold() {
    for (pages, errors, expect) in [
        (100, 2, false),
        (100, 3, true),
        (1000, 19, false),
        (1000, 20, true),
    ] {
        let (prev, curr) = spike_pair(pages, errors);
        let changes = diff(&prev, &curr, &none());
        assert_eq!(
            !spikes(&changes).is_empty(),
            expect,
            "{pages} pages, {errors} errors"
        );
    }
}

#[test]
fn error_spike_is_one_critical_change_with_counts() {
    let (prev, mut curr) = spike_pair(100, 3);
    edit(&mut curr, "https://e.com/p50", |p| p.status = 503);
    let changes = diff(&prev, &curr, &none());
    let s = spikes(&changes);
    assert_eq!(s.len(), 1);
    assert_eq!(
        (s[0].severity, s[0].url.clone()),
        (Severity::Critical, None)
    );
    assert_eq!((s[0].before.as_str(), s[0].after.as_str()), ("0", "4"));
    assert_eq!(changes[0].kind, ChangeKind::ErrorSpike);
}

#[test]
fn error_spike_counts_new_failing_urls_only_when_prev_is_complete() {
    let prev = site_with(20, StopReason::Completed);
    let mut curr = prev.clone();
    for i in 0..3 {
        curr.pages
            .push(page(&format!("https://e.com/new{i}"), 500, ""));
    }
    assert_eq!(spikes(&diff(&prev, &curr, &none())).len(), 1);
    let mut partial = prev.clone();
    partial.stop = StopReason::PageLimit;
    assert!(spikes(&diff(&partial, &curr, &none())).is_empty());
}

#[test]
fn error_spike_ignores_pages_that_were_already_failing() {
    let mut prev = site_with(20, StopReason::Completed);
    for i in 0..3 {
        edit(&mut prev, &format!("https://e.com/p{i}"), |p| {
            p.status = 404
        });
    }
    let curr = prev.clone();
    assert!(diff(&prev, &curr, &none()).is_empty());
}

#[test]
fn http_to_https_move_is_one_change() {
    let prev = site_at("http://e.com/");
    let curr = site_at("https://e.com/");
    let c = only(diff(&prev, &curr, &none()));
    assert_eq!(
        (c.kind, c.severity),
        (ChangeKind::SiteMoved, Severity::Critical)
    );
    assert_eq!(c.url, None);
    assert_eq!(
        (c.before.as_str(), c.after.as_str()),
        ("http://e.com/", "https://e.com/")
    );
}

#[test]
fn www_move_is_one_change() {
    let prev = site_at("https://e.com/");
    let curr = site_at("https://www.e.com/");
    let c = only(diff(&prev, &curr, &none()));
    assert_eq!(
        (c.kind, c.severity),
        (ChangeKind::SiteMoved, Severity::Critical)
    );
}

#[test]
fn port_change_is_a_move() {
    let prev = site_at("https://e.com/");
    let curr = site_at("https://e.com:8443/");
    assert_eq!(
        kinds(&diff(&prev, &curr, &none())),
        vec![ChangeKind::SiteMoved]
    );
}

#[test]
fn a_move_with_canonicals_on_the_old_origin_is_still_one_change() {
    let mut prev = site_at("https://e.com/");
    edit(&mut prev, "https://e.com/a", |p| {
        p.fields.canonical = Some(u("https://e.com/a"))
    });
    let mut curr = site_at("https://www.e.com/");
    edit(&mut curr, "https://www.e.com/a", |p| {
        p.fields.canonical = Some(u("https://www.e.com/a"))
    });
    assert_eq!(
        kinds(&diff(&prev, &curr, &none())),
        vec![ChangeKind::SiteMoved]
    );
}

#[test]
fn a_move_still_reports_real_changes() {
    let prev = site_at("https://e.com/");
    let mut curr = site_at("https://www.e.com/");
    edit(&mut curr, "https://www.e.com/a", |p| {
        p.fields.title = Some("New".into())
    });
    curr.pages.push(page("https://www.e.com/c", 200, "C"));
    let c = diff(&prev, &curr, &none());
    assert_eq!(
        kinds(&c),
        vec![
            ChangeKind::SiteMoved,
            ChangeKind::NewUrl,
            ChangeKind::TitleChanged
        ]
    );
}

#[test]
fn off_origin_records_are_ignored() {
    let prev = site_at("https://e.com/");
    let mut curr = prev.clone();
    curr.pages.push(page("http://e.com/", 301, ""));
    assert!(diff(&prev, &curr, &none()).is_empty());
    // And a removed off-origin record is not reported either.
    assert!(diff(&curr, &prev, &none()).is_empty());
}

#[test]
fn off_origin_records_do_not_count_toward_the_error_threshold() {
    let prev = site_with(30, StopReason::Completed);
    let mut curr = prev.clone();
    for i in 0..5 {
        curr.pages
            .push(page(&format!("https://other.com/{i}"), 404, ""));
    }
    assert!(diff(&prev, &curr, &none()).is_empty());
}

#[test]
fn key_pages_are_the_origin_the_top_20_by_inlinks_and_the_starred() {
    let mut s = site_with(30, StopReason::Completed);
    for (i, p) in s.pages.iter_mut().skip(1).enumerate() {
        p.inlinks = 100 + i as u32; // /p29 has the most
    }
    let starred: HashSet<u64> = [url_hash(&u("https://e.com/p0"))].into();
    let keys = key_pages(&s, &starred);
    // origin + 20 + starred p0 (p0 has the fewest inlinks, so it isn't in the top 20)
    assert_eq!(keys.len(), 22);
    assert!(keys.contains(&url_hash(&u("https://e.com/"))));
    assert!(keys.contains(&url_hash(&u("https://e.com/p0"))));
    assert!(keys.contains(&url_hash(&u("https://e.com/p29"))));
    assert!(keys.contains(&url_hash(&u("https://e.com/p10"))));
    assert!(!keys.contains(&url_hash(&u("https://e.com/p9"))));
}

#[test]
fn key_page_ties_are_broken_by_url() {
    let s = site_with(30, StopReason::Completed); // every page has 1 inlink
    let keys = key_pages(&s, &none());
    // By URL string the origin comes first, then p0, p1, p10..p19, p2, p20..p25.
    assert_eq!(keys.len(), 20);
    assert!(keys.contains(&url_hash(&u("https://e.com/"))));
    assert!(keys.contains(&url_hash(&u("https://e.com/p0"))));
    assert!(keys.contains(&url_hash(&u("https://e.com/p25"))));
    assert!(!keys.contains(&url_hash(&u("https://e.com/p26"))));
    assert!(!keys.contains(&url_hash(&u("https://e.com/p9"))));
    assert_eq!(keys, key_pages(&s, &none()));
}

#[test]
fn changes_are_sorted_by_severity_then_kind_then_url() {
    let prev = site_at("https://e.com/");
    let mut curr = prev.clone();
    edit(&mut curr, "https://e.com/b", |p| {
        p.fields.title = Some("B2".into())
    });
    edit(&mut curr, "https://e.com/a", |p| {
        p.fields.title = Some("A2".into())
    });
    edit(&mut curr, "https://e.com/", |p| p.fields.title = None);
    curr.pages.push(page("https://e.com/c", 200, "C"));
    curr.sitemap = sitemap(1);
    let mut prev = prev;
    prev.sitemap = sitemap(100);
    let c = diff(&prev, &curr, &none());
    let order: Vec<(Severity, ChangeKind, Option<&str>)> = c
        .iter()
        .map(|c| (c.severity, c.kind, c.url.as_ref().map(Url::as_str)))
        .collect();
    assert_eq!(
        order,
        vec![
            // The origin page is a key page: TitleRemoved Warning -> Critical.
            (
                Severity::Critical,
                ChangeKind::TitleRemoved,
                Some("https://e.com/")
            ),
            (Severity::Warning, ChangeKind::SitemapShrank, None),
            (
                Severity::Notice,
                ChangeKind::NewUrl,
                Some("https://e.com/c")
            ),
            (
                Severity::Notice,
                ChangeKind::TitleChanged,
                Some("https://e.com/a")
            ),
            (
                Severity::Notice,
                ChangeKind::TitleChanged,
                Some("https://e.com/b")
            ),
        ]
    );
}
