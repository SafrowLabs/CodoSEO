//! Check thresholds and exceptions: the default policy gives exactly today's reports, other
//! thresholds flag and unflag pages, exceptions and kept-out pages leave the health score, a
//! stored crawl judged again agrees with a fresh one, and the save preview counts what changes.

use std::collections::HashSet;

use codoseo_checks::{Thresholds, health_score, run_checks};
use codoseo_core::check::CheckId;
use codoseo_core::crawl::SitemapSummary;
use codoseo_core::output::{CrawlOutput, LinkGraph, SiteSignals, StopReason};
use codoseo_core::page::{Indexability, PageFields, PageRecord};
use codoseo_core::report::CrawlReport;
use codoseo_core::url::url_hash;
use codoseo_policy::{
    Exception, IndexRule, IndexStance, PathPattern, PolicyInput, SitePolicy, preview, recheck,
    rescore_stored, run_checks_with, scored_counts,
};
use proptest::prelude::*;
use url::Url;

fn url(path: &str) -> Url {
    Url::parse(&format!("https://shop.example.com{path}")).unwrap()
}

/// A healthy HTML page whose title, description and text are unique to `path`.
fn page(path: &str) -> PageRecord {
    let url = url(path);
    let pad = |base: String, len: usize| {
        let mut s = base;
        while s.chars().count() < len {
            s.push('x');
        }
        s
    };
    PageRecord {
        url_hash: url_hash(&url),
        status: 200,
        redirect_chain: Vec::new(),
        response_ms: 120,
        size_bytes: 1000,
        content_type: Some("text/html".to_owned()),
        depth: Some(1),
        in_sitemap: true,
        indexability: Indexability::Indexable,
        fields: PageFields {
            title: Some(pad(format!("Title {path} "), 45)),
            title_count: 1,
            meta_description: Some(pad(format!("Description {path} "), 120)),
            canonical: Some(url.clone()),
            h1: vec![format!("Heading {path}")],
            word_count: 400,
            content_hash: url_hash(&url),
            ..PageFields::default()
        },
        url,
        inlinks: 1,
        outlinks_internal: 2,
        outlinks_external: 0,
        issues: Default::default(),
        key_hash: 0,
        redirect_target: None,
        outlinks_nofollow: 0,
        error: None,
    }
}

fn with_title(path: &str, chars: usize) -> PageRecord {
    let mut p = page(path);
    let mut t = format!("{path} ");
    while t.chars().count() < chars {
        t.push('t');
    }
    p.fields.title = Some(t);
    p
}

fn with_words(path: &str, words: u32) -> PageRecord {
    let mut p = page(path);
    p.fields.word_count = words;
    p
}

fn out(pages: Vec<PageRecord>) -> CrawlOutput {
    CrawlOutput {
        origin: url("/"),
        pages,
        links: LinkGraph::default(),
        robots: None,
        sitemap: SitemapSummary::default(),
        sitemap_files: Vec::new(),
        stop: StopReason::Completed,
        duration_ms: 1,
        signals: SiteSignals::default(),
    }
}

/// A crawl with something for every threshold check, on both sides of each default.
fn fixture() -> Vec<PageRecord> {
    let mut slow = page("/slow");
    slow.response_ms = 1_400;
    let mut slower = page("/slower");
    slower.response_ms = 2_400;
    let mut hop2 = page("/hops/two");
    hop2.redirect_chain = vec![(301, url("/a")), (301, url("/b"))];
    let mut hop3 = page("/hops/three");
    hop3.redirect_chain = vec![(301, url("/a")), (301, url("/b")), (302, url("/c"))];
    let mut alt = page("/products/shirt");
    alt.fields.images_missing_alt = 3;
    let mut long_desc = page("/long-description");
    long_desc.fields.meta_description = Some("d".repeat(170));
    let mut short_desc = page("/short-description");
    short_desc.fields.meta_description = Some("Short one".to_owned());
    let mut tag = with_words("/blog/tag/linen", 50);
    tag.indexability = Indexability::Noindex;
    tag.fields.meta_robots = Some("noindex".to_owned());
    let mut gone = page("/gone");
    gone.status = 404;
    gone.indexability = Indexability::ClientError;
    let mut home = page("/");
    home.depth = Some(0);
    vec![
        home,
        with_title("/title/62", 62),
        with_title("/title/70", 70),
        with_title("/title/20", 20),
        with_words("/stores/paris", 150),
        with_words("/stores/rome", 90),
        with_words("/blog/short", 100),
        slow,
        slower,
        hop2,
        hop3,
        alt,
        long_desc,
        short_desc,
        tag,
        gone,
    ]
}

fn judged(policy: &SitePolicy, pages: Vec<PageRecord>) -> (CrawlOutput, CrawlReport) {
    let mut o = out(pages);
    let r = run_checks_with(policy, &mut o, &HashSet::new(), &[]);
    (o, r)
}

fn flagged(o: &CrawlOutput, id: CheckId) -> Vec<&str> {
    o.pages
        .iter()
        .filter(|p| p.issues.has_check(id))
        .map(|p| p.url.path())
        .collect()
}

fn count(r: &CrawlReport, id: CheckId) -> u32 {
    r.counts
        .iter()
        .find(|(c, _)| *c == id)
        .map_or(0, |&(_, n)| n)
}

#[test]
fn the_default_policy_gives_byte_identical_reports() {
    let mut plain = out(fixture());
    let before = run_checks(&mut plain);
    let (with, after) = judged(&SitePolicy::default(), fixture());
    assert_eq!(
        serde_json::to_string(&after).unwrap(),
        serde_json::to_string(&before).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&with.pages).unwrap(),
        serde_json::to_string(&plain.pages).unwrap()
    );
    // Nothing excluded, so the report has no `excluded` key at all.
    assert!(!serde_json::to_string(&after).unwrap().contains("excluded"));
    // An explicit default threshold is the same as none.
    let mut p = SitePolicy::default();
    p.seo.thresholds.title_max = Some(60);
    p.seo.thresholds.slow_ms = Some(1000);
    assert_eq!(judged(&p, fixture()).1, before);
}

proptest! {
    #[test]
    fn the_default_policy_matches_run_checks_on_any_page(
        title in 0usize..120,
        description in 0usize..300,
        words in 0u32..600,
        ms in 0u32..5000,
        hops in 0usize..4,
        missing_alt in 0u32..3,
    ) {
        let mut p = with_title("/p", title.max(3));
        if title < 3 {
            p.fields.title = None;
        }
        p.fields.meta_description = Some("d".repeat(description));
        p.fields.word_count = words;
        p.response_ms = ms;
        p.redirect_chain = (0..hops).map(|i| (301, url(&format!("/r{i}")))).collect();
        p.fields.images_missing_alt = missing_alt;
        let mut plain = out(vec![page("/"), p.clone()]);
        let before = run_checks(&mut plain);
        let (with, after) = judged(&SitePolicy::default(), vec![page("/"), p]);
        prop_assert_eq!(after, before);
        prop_assert_eq!(with.pages, plain.pages);
    }
}

#[test]
fn thresholds_flag_and_unflag_pages() {
    let (o, r) = judged(&SitePolicy::default(), fixture());
    assert_eq!(
        flagged(&o, CheckId::TitleTooLong),
        ["/title/62", "/title/70"]
    );
    assert_eq!(
        flagged(&o, CheckId::ThinContent),
        ["/stores/paris", "/stores/rome", "/blog/short"]
    );
    assert_eq!(flagged(&o, CheckId::SlowResponse), ["/slow", "/slower"]);
    assert_eq!(
        flagged(&o, CheckId::RedirectChain),
        ["/hops/two", "/hops/three"]
    );
    assert_eq!(r.checks_total, 44);

    let mut p = SitePolicy::default();
    let t = &mut p.seo.thresholds;
    t.title_max = Some(65);
    t.title_min = Some(25);
    t.thin_words = Some(120);
    t.slow_ms = Some(1_500);
    t.max_redirect_hops = Some(2);
    t.image_alt = Some(false);
    t.description_max = Some(180);
    t.description_min = Some(5);
    let (o, r) = judged(&p, fixture());
    assert_eq!(flagged(&o, CheckId::TitleTooLong), ["/title/70"]);
    assert_eq!(flagged(&o, CheckId::TitleTooShort), ["/title/20"]);
    assert_eq!(
        flagged(&o, CheckId::ThinContent),
        ["/stores/rome", "/blog/short"]
    );
    assert_eq!(flagged(&o, CheckId::SlowResponse), ["/slower"]);
    assert_eq!(flagged(&o, CheckId::RedirectChain), ["/hops/three"]);
    assert!(flagged(&o, CheckId::ImagesMissingAlt).is_empty());
    assert!(flagged(&o, CheckId::DescriptionTooLong).is_empty());
    assert!(flagged(&o, CheckId::DescriptionTooShort).is_empty());
    // The image alt check is off, so it is not one of the checks run.
    assert_eq!(r.checks_total, 43);
    assert_eq!(
        usize::from(r.checks_passed) + r.counts.len(),
        usize::from(r.checks_total)
    );

    // Stricter numbers flag more.
    let mut strict = SitePolicy::default();
    strict.seo.thresholds.slow_ms = Some(100);
    strict.seo.thresholds.thin_words = Some(500);
    let (o, _) = judged(&strict, fixture());
    assert_eq!(
        flagged(&o, CheckId::SlowResponse).len(),
        16,
        "every page answered in 120 ms or more"
    );
    assert!(flagged(&o, CheckId::ThinContent).contains(&"/"));
}

fn exception(check: CheckId, scope: &str, note: &str) -> Exception {
    Exception {
        check,
        scope: PathPattern(scope.to_owned()),
        note: note.to_owned(),
    }
}

#[test]
fn exceptions_keep_the_issue_but_leave_the_score() {
    let mut p = SitePolicy::default();
    p.seo.exceptions = vec![exception(
        CheckId::ThinContent,
        "/stores/*",
        "Store pages are short by design",
    )];
    let (o, r) = judged(&p, fixture());
    // The pages keep the issue …
    assert_eq!(
        flagged(&o, CheckId::ThinContent),
        ["/stores/paris", "/stores/rome", "/blog/short"]
    );
    assert_eq!(count(&r, CheckId::ThinContent), 3);
    // … and two of them leave its count in the score; the page count stays.
    assert_eq!(r.excluded.of(CheckId::ThinContent), 2);
    assert_eq!(r.excluded.pages, 0);
    assert_eq!(r.scored_pages(), 16);
    let scored = r.scored_counts();
    assert_eq!(
        scored.iter().find(|(c, _)| *c == CheckId::ThinContent),
        Some(&(CheckId::ThinContent, 1))
    );
    assert_eq!(r.health_score, health_score(&scored, 16));
    let (_, plain) = judged(&SitePolicy::default(), fixture());
    assert!(r.health_score >= plain.health_score);

    // Excepting every page that fails a check makes the check pass.
    p.seo
        .exceptions
        .push(exception(CheckId::SlowResponse, "/slow*", ""));
    let (_, r) = judged(&p, fixture());
    assert_eq!(count(&r, CheckId::SlowResponse), 2);
    assert!(
        !r.scored_counts()
            .iter()
            .any(|(c, _)| *c == CheckId::SlowResponse)
    );
    assert_eq!(
        usize::from(r.checks_passed) + r.scored_counts().len(),
        usize::from(r.checks_total)
    );
}

#[test]
fn kept_out_pages_leave_the_score_entirely() {
    let mut p = SitePolicy::default();
    p.seo.index = vec![IndexRule {
        pattern: PathPattern("/blog/tag/*".to_owned()),
        stance: IndexStance::KeepOut,
    }];
    let (o, r) = judged(&p, fixture());
    let tag = o
        .pages
        .iter()
        .find(|x| x.url.path() == "/blog/tag/linen")
        .unwrap();
    assert!(tag.issues.has_check(CheckId::Noindex));
    // The noindex page is kept out as asked: out of the page count and every count.
    assert_eq!(r.excluded.pages, 1);
    assert_eq!(r.scored_pages(), 15);
    assert_eq!(r.excluded.of(CheckId::Noindex), count(&r, CheckId::Noindex));
    assert!(
        !r.scored_counts()
            .iter()
            .any(|(c, _)| *c == CheckId::Noindex)
    );
    let (scored, pages, excluded) = scored_counts(&p, &o.pages, &r.counts);
    assert_eq!((pages, &excluded), (15, &r.excluded));
    assert_eq!(r.health_score, health_score(&scored, 15));
}

#[test]
fn a_stored_crawl_judged_again_agrees_with_a_fresh_one() {
    let (stored, report) = judged(&SitePolicy::default(), fixture());
    let mut p = SitePolicy::default();
    p.seo.thresholds.title_max = Some(65);
    p.seo.thresholds.thin_words = Some(120);
    p.seo.must_work = vec!["/gone".to_owned()];
    p.seo.exceptions = vec![exception(CheckId::ThinContent, "/stores/*", "")];
    let input = PolicyInput {
        pages: &stored.pages,
        origin: Some(&stored.origin),
        ..PolicyInput::default()
    };
    let bits = recheck(&p, &input);
    let again = rescore_stored(&p, &stored.pages, &bits, &report.counts);
    let (fresh, fresh_report) = judged(&p, fixture());
    let fresh_bits: Vec<_> = fresh.pages.iter().map(|x| x.issues).collect();
    assert_eq!(bits, fresh_bits);
    assert_eq!(again.counts, fresh_report.counts);
    assert_eq!(again.excluded, fresh_report.excluded);
    assert_eq!(again.health_score, fresh_report.health_score);
    assert_eq!(
        (again.checks_passed, again.checks_total),
        (fresh_report.checks_passed, fresh_report.checks_total)
    );
}

#[test]
fn the_preview_counts_what_a_save_changes() {
    let (stored, report) = judged(&SitePolicy::default(), fixture());
    let input = PolicyInput {
        pages: &stored.pages,
        origin: Some(&stored.origin),
        ..PolicyInput::default()
    };
    let old = SitePolicy::default();
    let same = preview(&old, &old, &input, &report.counts);
    assert_eq!((same.muted, same.opened), (0, 0));
    assert_eq!(same.health, (report.health_score, report.health_score));
    assert_eq!(same.geo, None, "no AI access report");

    let mut new = SitePolicy::default();
    // Unflags /title/62 and /stores/paris; excepts /stores/rome's thin content.
    new.seo.thresholds.title_max = Some(65);
    new.seo.thresholds.thin_words = Some(120);
    new.seo.exceptions = vec![exception(CheckId::ThinContent, "/stores/*", "")];
    // Flags every page over 100 ms: 14 more (the 120 ms pages, not the ones already slow).
    new.seo.thresholds.slow_ms = Some(100);
    let p = preview(&old, &new, &input, &report.counts);
    assert_eq!(p.muted, 3);
    assert_eq!(p.opened, 14);
    let (_, fresh) = judged(&new, fixture());
    assert_eq!(p.health, (report.health_score, fresh.health_score));
}

#[test]
fn exceptions_cannot_name_policy_or_site_wide_checks() {
    let mut p = SitePolicy::default();
    p.seo.exceptions = vec![
        exception(CheckId::MustWorkFailed, "/x", ""),
        exception(CheckId::SitemapMissing, "/x", ""),
        exception(CheckId::ThinContent, "/x", ""),
    ];
    let errors = p.validate().unwrap_err();
    let paths: Vec<&str> = errors.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        ["seo.exceptions[0].check", "seo.exceptions[1].check"]
    );
}

#[test]
fn the_default_thresholds_are_the_checks_defaults() {
    assert_eq!(SitePolicy::default().thresholds(), Thresholds::DEFAULT);
}
