//! The SEO rules against a crawl: one test per rule's edge, a mockup-like fixture with a rule
//! met, one with gaps and one partly met, and the policy pass over a crawl report.

use std::collections::HashSet;

use codoseo_core::check::{CheckId, IssueBits, Severity};
use codoseo_core::crawl::SitemapSummary;
use codoseo_core::output::{CrawlOutput, LinkGraph, SiteSignals, StopReason};
use codoseo_core::page::{FetchFailure, Indexability, PageFields, PageRecord};
use codoseo_core::report::{CrawlReport, CrawlSummary, InlinkSample};
use codoseo_core::url::url_hash;
use codoseo_policy::seo::{Preferred, must_work_rows};
use codoseo_policy::{
    Address, GapKind, HostPref, IndexRule, IndexStance, PathPattern, PolicyInput, RuleId,
    SitePolicy, Slash, Status, apply_to_crawl, declared_checks, evaluate, policy_bits,
};
use url::Url;

fn url(s: &str) -> Url {
    if s.starts_with("http") {
        Url::parse(s).unwrap()
    } else {
        Url::parse(&format!("https://shop.example.com{s}")).unwrap()
    }
}

fn page(path: &str) -> PageRecord {
    let url = url(path);
    PageRecord {
        url_hash: url_hash(&url),
        url,
        status: 200,
        redirect_chain: Vec::new(),
        response_ms: 10,
        size_bytes: 1000,
        content_type: Some("text/html".to_owned()),
        depth: Some(1),
        in_sitemap: false,
        indexability: Indexability::Indexable,
        fields: PageFields::default(),
        inlinks: 1,
        outlinks_internal: 0,
        outlinks_external: 0,
        issues: IssueBits::default(),
        key_hash: 0,
        redirect_target: None,
        outlinks_nofollow: 0,
        error: None,
    }
}

fn noindex(path: &str) -> PageRecord {
    let mut p = page(path);
    p.indexability = Indexability::Noindex;
    p.fields.meta_robots = Some("noindex".to_owned());
    p
}

fn blocked(path: &str) -> PageRecord {
    let mut p = page(path);
    p.status = 0;
    p.content_type = None;
    p.indexability = Indexability::BlockedByRobots;
    p
}

fn canonical(path: &str, to: &str) -> PageRecord {
    let mut p = page(path);
    p.fields.canonical = Some(url(to));
    p.indexability = Indexability::Canonicalised;
    p
}

fn redirect(path: &str, to: &str) -> PageRecord {
    let mut p = page(path);
    p.status = 301;
    p.content_type = None;
    p.indexability = Indexability::Redirected;
    p.redirect_target = Some(url(to));
    p
}

fn status(path: &str, code: u16) -> PageRecord {
    let mut p = page(path);
    p.status = code;
    p.indexability = if code >= 500 {
        Indexability::ServerError
    } else {
        Indexability::ClientError
    };
    p
}

fn rule(pattern: &str, stance: IndexStance) -> IndexRule {
    IndexRule {
        pattern: PathPattern(pattern.to_owned()),
        stance,
    }
}

fn input(pages: &[PageRecord]) -> PolicyInput<'_> {
    PolicyInput {
        pages,
        ..PolicyInput::default()
    }
}

fn bits_of(policy: &SitePolicy, pages: &[PageRecord], id: CheckId) -> Vec<String> {
    policy_bits(policy, &input(pages))
        .iter()
        .zip(pages)
        .filter(|(b, _)| b.has_check(id))
        .map(|(_, p)| p.url.path().to_owned())
        .collect()
}

// ---- index -----------------------------------------------------------------------------------

#[test]
fn the_first_matching_index_rule_wins() {
    let mut p = SitePolicy::default();
    p.seo.index = vec![
        rule("/search*", IndexStance::KeepOut),
        rule("/search/special", IndexStance::Index),
    ];
    p.seo.index_default = IndexStance::Index;
    let pages = [page("/"), noindex("/search/special"), page("/search?q=a")];
    let r = evaluate(&p, &input(&pages));
    let idx = r.rule(RuleId::Index).unwrap();
    // /search/special is under /search* (keep out), where noindex is right; /search?q=a is
    // indexable there, the one gap.
    assert_eq!(idx.status, Status::Gaps);
    assert_eq!(idx.gaps, 1);
    assert_eq!(idx.parts[0].subject, "/search*");
    assert_eq!(idx.parts[0].note, "2 pages");
    assert_eq!(idx.parts[0].status, Status::Gaps);
    assert_eq!(
        idx.parts[1].status,
        Status::NotChecked,
        "nothing left for it"
    );
    assert_eq!(idx.parts[2].subject, "Everything else");
    assert_eq!(idx.parts[2].status, Status::Met);
    assert_eq!(
        bits_of(&p, &pages, CheckId::PolicyIndexMismatch),
        ["/search"]
    );
}

#[test]
fn keep_out_by_robots_txt_alone_is_partly() {
    let mut p = SitePolicy::default();
    p.seo.index = vec![rule("/account/*", IndexStance::KeepOut)];
    let pages = [
        page("/"),
        blocked("/account/orders"),
        blocked("/account/me"),
    ];
    let r = evaluate(&p, &input(&pages));
    let idx = r.rule(RuleId::Index).unwrap();
    assert_eq!(idx.status, Status::Partly);
    assert_eq!(idx.gaps, 0);
    let part = &idx.parts[0];
    assert_eq!(part.status, Status::Partly);
    assert_eq!(part.note, "2 URLs linked");
    assert!(
        part.text
            .contains("Google can still index the URLs from links"),
        "{}",
        part.text
    );
    assert_eq!(
        bits_of(&p, &pages, CheckId::PolicyKeepOutPartial),
        ["/account/orders", "/account/me"]
    );
    assert!(bits_of(&p, &pages, CheckId::PolicyIndexMismatch).is_empty());
    let g: Vec<_> = r.gaps_of(GapKind::Seo).collect();
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].severity, Severity::Notice);
}

#[test]
fn keep_out_with_noindex_is_met_and_index_with_noindex_is_critical() {
    let mut p = SitePolicy::default();
    p.seo.index = vec![rule("/checkout/*", IndexStance::KeepOut)];
    p.seo.index_default = IndexStance::Index;
    let mut sale = noindex("/collections/sale");
    sale.in_sitemap = true;
    let pages = [
        page("/"),
        noindex("/checkout/cart"),
        noindex("/checkout/pay"),
        sale,
    ];
    let r = evaluate(&p, &input(&pages));
    let idx = r.rule(RuleId::Index).unwrap();
    assert_eq!(idx.parts[0].status, Status::Met);
    assert_eq!(
        idx.parts[0].text,
        "All 2 carry noindex and are out of the sitemap."
    );
    assert_eq!(idx.parts[1].status, Status::Gaps);
    assert_eq!(
        idx.parts[1].text,
        "1 indexable. /collections/sale carries noindex."
    );
    let gap = r.gaps_of(GapKind::Seo).next().unwrap();
    assert_eq!(gap.severity, Severity::Critical);
    assert!(gap.text.starts_with("Everything else: "), "{}", gap.text);
}

#[test]
fn keep_out_gaps_note_the_sitemap() {
    let mut p = SitePolicy::default();
    p.seo.index = vec![rule("/search*", IndexStance::KeepOut)];
    let mut a = page("/search?q=a");
    a.in_sitemap = true;
    let pages = [a, page("/search?q=b")];
    let r = evaluate(&p, &input(&pages));
    let part = &r.rule(RuleId::Index).unwrap().parts[0];
    assert_eq!(part.gaps, 2);
    assert_eq!(
        part.text,
        "Indexable today: no noindex, and 1 is in the sitemap."
    );
}

// ---- parameters ------------------------------------------------------------------------------

#[test]
fn ignored_parameters_match_globs_and_skip_pagination() {
    let mut p = SitePolicy::default();
    p.seo.ignore_params = vec!["utm_*".to_owned(), "sort".to_owned()];
    p.seo.pagination_param = Some("page".to_owned());
    let pages = [
        page("/c"),
        // Pagination alone: not this rule's business.
        page("/c?page=2"),
        // An ignored parameter next to the page number: should canonicalise to ?page=2.
        page("/c?page=2&UTM_Source=x"),
        canonical("/c?utm_medium=mail", "/c"),
        canonical("/c?sort=price", "/c?sort=price&x=1"),
        noindex("/c?sort=name"),
    ];
    let r = evaluate(&p, &input(&pages));
    let params = r.rule(RuleId::Params).unwrap();
    assert_eq!(params.status, Status::Gaps);
    assert_eq!(params.gaps, 2);
    assert!(
        params
            .text
            .starts_with("4 URLs carry these parameters; 2 canonicalise to the clean URL."),
        "{}",
        params.text
    );
    assert_eq!(
        bits_of(&p, &pages, CheckId::PolicyParamCanonical),
        ["/c", "/c"],
        "?page=2&UTM_Source=x and ?sort=price"
    );
    let texts: Vec<&str> = r.gaps_of(GapKind::Seo).map(|g| g.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "1 ?UTM_Source= page doesn't canonicalise to the clean URL",
            "1 ?sort= page doesn't canonicalise to the clean URL"
        ]
    );
}

#[test]
fn no_url_with_the_parameters_is_met() {
    let mut p = SitePolicy::default();
    p.seo.ignore_params = vec!["gclid".to_owned()];
    let pages = [page("/"), page("/a?page=2")];
    let r = evaluate(&p, &input(&pages));
    assert_eq!(r.rule(RuleId::Params).unwrap().status, Status::Met);
}

// ---- address ---------------------------------------------------------------------------------

#[test]
fn preferred_address_forms() {
    let origin = url("https://www.shop.example.com/");
    let apex = Preferred::new(&origin, HostPref::Apex, Slash::Without);
    assert_eq!(apex.base(), "https://shop.example.com");
    assert!(apex.matches(&url("https://shop.example.com/a")));
    assert!(apex.matches(&url("https://shop.example.com/")));
    assert!(apex.matches(&url("https://shop.example.com/feed.xml")));
    assert!(!apex.matches(&url("https://shop.example.com/a/")));
    assert!(!apex.matches(&url("http://shop.example.com/a")));
    assert!(!apex.matches(&url("https://www.shop.example.com/a")));
    let www = Preferred::new(
        &url("https://shop.example.com/"),
        HostPref::Www,
        Slash::With,
    );
    assert!(www.matches(&url("https://www.shop.example.com/a/")));
    assert!(!www.matches(&url("https://www.shop.example.com/a")));
    let either = Preferred::new(&origin, HostPref::Www, Slash::Either);
    assert!(either.matches(&url("https://www.shop.example.com/a")));
    assert!(either.matches(&url("https://www.shop.example.com/a/")));
}

#[test]
fn the_address_rule_counts_pages_canonicals_redirects_and_links() {
    let mut p = SitePolicy::default();
    p.seo.address = Some(Address {
        host: HostPref::Apex,
        trailing_slash: Slash::Without,
    });
    let pages = [
        page("/"),
        page("/about"),
        page("/a/"),
        canonical("/b", "https://www.shop.example.com/b"),
        redirect("http://shop.example.com/about", "/about"),
        redirect("/old", "/c/"),
        page("/c/"),
    ];
    // The home page links to the http:// address twice (one link counted), /about once.
    let inlinks = [
        InlinkSample {
            target: 4,
            source: 0,
            anchor: String::new(),
            nofollow: false,
        },
        InlinkSample {
            target: 4,
            source: 0,
            anchor: String::new(),
            nofollow: false,
        },
        InlinkSample {
            target: 4,
            source: 1,
            anchor: String::new(),
            nofollow: false,
        },
    ];
    let inp = PolicyInput {
        pages: &pages,
        inlinks: &inlinks,
        ..PolicyInput::default()
    };
    let r = evaluate(&p, &inp);
    let a = r.rule(RuleId::Address).unwrap();
    assert_eq!(a.status, Status::Gaps);
    // /, /about (links), /a/ and /c/ (served), /b (canonical), /old (redirect).
    assert_eq!(a.gaps, 6);
    assert_eq!(
        a.text,
        "3 of 5 pages match. 2 pages are served on an address with a trailing slash · 1 page \
         canonicalises to another address · 1 URL redirects to another address · 2 internal \
         links use http://, each a redirect hop."
    );
    let mut capped = inp;
    capped.inlinks_capped = true;
    let r = evaluate(&p, &capped);
    assert!(
        r.gaps
            .iter()
            .any(|g| g.text == "at least 2 internal links use http://, each a redirect hop")
    );
}

#[test]
fn a_site_on_its_preferred_address_meets_the_rule() {
    let mut p = SitePolicy::default();
    p.seo.address = Some(Address {
        host: HostPref::Apex,
        trailing_slash: Slash::Without,
    });
    let pages = [
        page("/"),
        page("/a"),
        redirect("http://shop.example.com/a", "/a"),
    ];
    let a = evaluate(&p, &input(&pages));
    let a = a.rule(RuleId::Address).unwrap();
    assert_eq!(a.status, Status::Met);
    assert_eq!(
        a.text,
        "All 2 pages use https://shop.example.com, no trailing slash."
    );
    // Switching to www makes every page a gap.
    p.seo.address = Some(Address {
        host: HostPref::Www,
        trailing_slash: Slash::Without,
    });
    let r = evaluate(&p, &input(&pages));
    assert_eq!(r.rule(RuleId::Address).unwrap().gaps, 3);
}

// ---- must work -------------------------------------------------------------------------------

#[test]
fn must_work_pages_answer_200_and_are_indexable() {
    let mut p = SitePolicy::default();
    p.seo.must_work = vec![
        "/pricing".to_owned(),
        "/checkout/cart".to_owned(),
        "/gone".to_owned(),
        "/moved".to_owned(),
    ];
    let starred_page = noindex("/sale");
    let starred: HashSet<u64> = [starred_page.url_hash].into();
    let not_crawled = [url("/never")];
    let mut timeout = page("/gone");
    timeout.status = 0;
    timeout.error = Some(FetchFailure::Timeout);
    timeout.indexability = Indexability::ServerError;
    let pages = [
        page("/"),
        page("/pricing"),
        status("/checkout/cart", 500),
        timeout,
        redirect("/moved", "/moved-here"),
        page("/moved-here"),
        starred_page,
    ];
    let inp = PolicyInput {
        pages: &pages,
        starred: Some(&starred),
        starred_urls: &not_crawled,
        ..PolicyInput::default()
    };
    let rows = must_work_rows(&p.seo, &inp);
    let seen: Vec<(&str, &str, Status)> = rows
        .iter()
        .map(|r| (r.path.as_str(), r.why.label(), r.status))
        .collect();
    assert_eq!(
        seen,
        [
            ("/", "Home", Status::Met),
            ("/pricing", "Listed", Status::Met),
            ("/checkout/cart", "Listed", Status::Gaps),
            ("/gone", "Listed", Status::Gaps),
            ("/moved", "Listed", Status::Met),
            ("/never", "Starred", Status::NotChecked),
            ("/sale", "Starred", Status::Gaps),
        ]
    );
    assert_eq!(rows[2].text, "answers 500.");
    assert_eq!(rows[2].state, "500");
    assert_eq!(rows[3].text, "timed out.");
    assert_eq!(rows[4].text, "301 to /moved-here, then 200, indexable.");
    assert_eq!(rows[5].text, "Not reached on this crawl.");
    let r = evaluate(&p, &inp);
    let m = r.rule(RuleId::MustWork).unwrap();
    assert_eq!((m.status, m.gaps), (Status::Gaps, 3));
    assert!(m.text.ends_with("1 page was not reached."), "{}", m.text);
    let gaps: Vec<&str> = r.gaps_of(GapKind::Seo).map(|g| g.text.as_str()).collect();
    assert_eq!(
        gaps,
        [
            "/checkout/cart answers 500",
            "/gone timed out",
            "/sale carries noindex"
        ]
    );
    assert!(
        r.gaps_of(GapKind::Seo)
            .all(|g| g.severity == Severity::Critical)
    );
    assert_eq!(
        policy_bits(&p, &inp)
            .iter()
            .filter(|b| b.has_check(CheckId::MustWorkFailed))
            .count(),
        3
    );
}

#[test]
fn must_work_is_not_checked_without_listed_pages_or_a_crawl() {
    let mut p = SitePolicy::default();
    let pages = [status("/", 500)];
    let r = evaluate(&p, &input(&pages));
    assert_eq!(r.rule(RuleId::MustWork).unwrap().status, Status::NotChecked);
    assert_eq!(r.seo_gaps, 0);
    p.seo.must_work = vec!["/a".to_owned()];
    let r = evaluate(&p, &input(&[]));
    assert_eq!(r.rule(RuleId::MustWork).unwrap().status, Status::NotChecked);
}

// ---- the whole --------------------------------------------------------------------------------

/// The mockup's shop: a keep-out rule met (noindex), one partly met (robots.txt only), one with
/// gaps (indexable search pages), an ignored parameter canonicalising to itself and a must-work
/// page answering 500.
#[test]
fn a_mockup_like_crawl_has_met_gaps_and_partly() {
    let toml = r#"
[policy]
version = 1
[policy.seo.address]
host = "apex"
trailing_slash = "without"
[[policy.seo.index]]
pattern = "/checkout/*"
stance = "keep_out"
[[policy.seo.index]]
pattern = "/account/*"
stance = "keep_out"
[[policy.seo.index]]
pattern = "/search*"
stance = "keep_out"
[policy.seo]
index_default = "index"
ignore_params = ["utm_*", "sort"]
pagination_param = "page"
must_work = ["/", "/pricing", "/checkout/cart"]
"#;
    let p = codoseo_policy::parse_file(toml).unwrap().policy;
    let mut search = page("/search?q=shirt");
    search.in_sitemap = true;
    let pages = [
        page("/"),
        page("/pricing"),
        noindex("/checkout/start"),
        {
            let mut c = status("/checkout/cart", 500);
            c.fields.meta_robots = Some("noindex".to_owned());
            c
        },
        blocked("/account/orders"),
        search,
        page("/search?q=hat"),
        page("/collections/winter?sort=price"),
        page("/collections/winter"),
        page("/collections/winter?page=2"),
    ];
    let r = evaluate(&p, &input(&pages));
    let status_of = |id| r.rule(id).unwrap().status;
    assert_eq!(status_of(RuleId::Address), Status::Met);
    let idx = r.rule(RuleId::Index).unwrap();
    let parts: Vec<(&str, Status, u32)> = idx
        .parts
        .iter()
        .map(|p| (p.subject.as_str(), p.status, p.gaps))
        .collect();
    assert_eq!(
        parts,
        [
            ("/checkout/*", Status::Met, 0),
            ("/account/*", Status::Partly, 0),
            ("/search*", Status::Gaps, 2),
            ("Everything else", Status::Met, 0),
        ]
    );
    assert_eq!(status_of(RuleId::Params), Status::Gaps);
    assert_eq!(status_of(RuleId::MustWork), Status::Gaps);
    let seo: Vec<&str> = r.gaps_of(GapKind::Seo).map(|g| g.text.as_str()).collect();
    assert_eq!(
        seo,
        [
            "/account/*: robots.txt only, so it can still be indexed",
            "/search*: 2 pages indexable today",
            "1 ?sort= page doesn't canonicalise to the clean URL",
            "/checkout/cart answers 500",
        ]
    );
    assert_eq!(r.seo_gaps, 4);
    assert_eq!(
        declared_checks(&p.seo),
        [
            CheckId::PolicyAddress,
            CheckId::PolicyIndexMismatch,
            CheckId::PolicyKeepOutPartial,
            CheckId::PolicyParamCanonical,
            CheckId::MustWorkFailed
        ]
    );
}

fn crawl(pages: Vec<PageRecord>) -> (CrawlOutput, CrawlReport) {
    let out = CrawlOutput {
        origin: url("/"),
        pages,
        links: LinkGraph::default(),
        robots: None,
        sitemap: SitemapSummary::default(),
        stop: StopReason::Completed,
        duration_ms: 1,
        signals: SiteSignals::default(),
    };
    let report = CrawlReport {
        health_score: 100,
        checks_passed: 44,
        checks_total: 44,
        counts: vec![(CheckId::Noindex, 1)],
        inlink_samples: Vec::new(),
        summary: CrawlSummary::default(),
    };
    (out, report)
}

#[test]
fn the_default_policy_leaves_a_crawl_as_it_was() {
    let (mut out, mut report) = crawl(vec![page("/"), status("/x", 500)]);
    let (before_out, before) = (out.clone(), report.clone());
    apply_to_crawl(
        &SitePolicy::default(),
        &mut out,
        &mut report,
        &HashSet::new(),
        &[],
    );
    assert_eq!(out, before_out);
    assert_eq!(report, before);
}

#[test]
fn the_policy_pass_sets_bits_counts_and_rescores() {
    let mut p = SitePolicy::default();
    p.seo.must_work = vec!["/x".to_owned()];
    let (mut out, mut report) = crawl(vec![page("/"), status("/x", 500)]);
    // A stale policy bit from an earlier pass is replaced.
    out.pages[0].issues.set_check(CheckId::PolicyAddress);
    apply_to_crawl(&p, &mut out, &mut report, &HashSet::new(), &[]);
    assert!(out.pages[1].issues.has_check(CheckId::MustWorkFailed));
    assert!(!out.pages[0].issues.has_check(CheckId::PolicyAddress));
    assert_eq!(
        report.counts,
        [(CheckId::Noindex, 1), (CheckId::MustWorkFailed, 1)]
    );
    assert_eq!((report.checks_passed, report.checks_total), (43, 45));
    assert!(report.health_score < 100);
    let again = report.clone();
    apply_to_crawl(&p, &mut out, &mut report, &HashSet::new(), &[]);
    assert_eq!(report, again);
}
