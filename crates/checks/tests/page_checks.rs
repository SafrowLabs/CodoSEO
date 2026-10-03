use std::collections::HashSet;

use codoseo_checks::{CHECKS, Scope, def, page_issues};
use codoseo_core::check::{CheckId, IssueBits, Severity};
use codoseo_core::page::{
    FetchFailure, Indexability, JsonLdStatus, OgTags, PageFields, PageRecord,
};
use url::Url;

type Case = (CheckId, fn(&mut PageRecord));

fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

/// A healthy page: nothing fires on it.
fn clean() -> PageRecord {
    let url = u("https://e.com/a");
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
            title: Some("t".repeat(45)),
            title_count: 1,
            meta_description: Some("d".repeat(120)),
            canonical: Some(url.clone()),
            hreflang: vec![("en".into(), url)],
            h1: vec!["Heading".into()],
            word_count: 400,
            og: OgTags {
                title: Some("Title".into()),
                description: None,
                image: Some("https://e.com/i.png".into()),
            },
            jsonld: JsonLdStatus::Valid(1),
            ..PageFields::default()
        },
        inlinks: 2,
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

fn has(p: &PageRecord, id: CheckId) -> bool {
    page_issues(p).has_check(id)
}

#[test]
fn registry_lists_every_check_once_in_order() {
    assert_eq!(CHECKS.len(), CheckId::ALL.len());
    for (i, d) in CHECKS.iter().enumerate() {
        assert_eq!(d.id as usize, i);
        assert!(!d.title.is_empty());
    }
    assert_eq!(def(CheckId::Http4xx).severity, Severity::Critical);
    assert_eq!(def(CheckId::TitleDuplicate).scope, Scope::Site);
    assert_eq!(def(CheckId::SitemapMissing).scope, Scope::SiteWide);
}

#[test]
fn clean_page_has_no_issues() {
    assert_eq!(page_issues(&clean()), IssueBits(0));
}

#[test]
fn each_page_check_triggers_on_its_fixture() {
    let cases: Vec<Case> = vec![
        (CheckId::Http4xx, |p| {
            p.status = 404;
            p.indexability = Indexability::ClientError
        }),
        (CheckId::Http5xx, |p| {
            p.status = 502;
            p.indexability = Indexability::ServerError
        }),
        (CheckId::FetchFailed, |p| {
            p.status = 0;
            p.error = Some(FetchFailure::Timeout)
        }),
        (CheckId::RedirectLoop, |p| {
            p.status = 301;
            p.error = Some(FetchFailure::RedirectLoop)
        }),
        (CheckId::Redirected, |p| {
            p.status = 301;
            p.indexability = Indexability::Redirected
        }),
        (CheckId::RedirectChain, |p| {
            p.status = 301;
            p.indexability = Indexability::Redirected;
            p.redirect_chain = vec![(301, u("https://e.com/a")), (302, u("https://e.com/b"))]
        }),
        (CheckId::Noindex, |p| p.indexability = Indexability::Noindex),
        (CheckId::Canonicalised, |p| {
            p.indexability = Indexability::Canonicalised
        }),
        (CheckId::CanonicalMissing, |p| p.fields.canonical = None),
        (CheckId::BlockedByRobots, |p| {
            p.status = 0;
            p.indexability = Indexability::BlockedByRobots
        }),
        (CheckId::TitleMissing, |p| p.fields.title = None),
        (CheckId::TitleTooLong, |p| {
            p.fields.title = Some("x".repeat(61))
        }),
        (CheckId::TitleTooShort, |p| {
            p.fields.title = Some("Short".into())
        }),
        (CheckId::TitleMultiple, |p| p.fields.title_count = 2),
        (CheckId::DescriptionMissing, |p| {
            p.fields.meta_description = Some("  ".into())
        }),
        (CheckId::DescriptionTooLong, |p| {
            p.fields.meta_description = Some("x".repeat(161))
        }),
        (CheckId::DescriptionTooShort, |p| {
            p.fields.meta_description = Some("Short".into())
        }),
        (CheckId::H1Missing, |p| p.fields.h1 = vec!["  ".into()]),
        (CheckId::H1Multiple, |p| {
            p.fields.h1 = vec!["One".into(), "Two".into()]
        }),
        (CheckId::ThinContent, |p| p.fields.word_count = 50),
        (CheckId::ImagesMissingAlt, |p| {
            p.fields.images_missing_alt = 1
        }),
        (CheckId::NoInternalOutlinks, |p| p.outlinks_internal = 0),
        (CheckId::NofollowInternalLinks, |p| p.outlinks_nofollow = 1),
        (CheckId::DeepPage, |p| p.depth = Some(4)),
        (CheckId::MixedContent, |p| p.fields.mixed_content = 2),
        (CheckId::NotHttps, |p| p.url = u("http://e.com/a")),
        (CheckId::SlowResponse, |p| p.response_ms = 1_001),
        (CheckId::OgMissing, |p| p.fields.og.image = None),
        (CheckId::JsonldInvalid, |p| {
            p.fields.jsonld = JsonLdStatus::Invalid
        }),
        (CheckId::HreflangMissingSelf, |p| {
            p.fields.hreflang = vec![("de".into(), u("https://e.com/de"))]
        }),
    ];
    for (id, mutate) in &cases {
        let mut p = clean();
        mutate(&mut p);
        assert!(has(&p, *id), "{id:?} should trigger");
    }
    let tested: HashSet<CheckId> = cases.iter().map(|c| c.0).collect();
    let page_scope: HashSet<CheckId> = CHECKS
        .iter()
        .filter(|d| d.scope == Scope::Page)
        .map(|d| d.id)
        .collect();
    assert_eq!(
        tested, page_scope,
        "every page check needs a trigger fixture"
    );
}

#[test]
fn page_issues_never_sets_site_scope_checks() {
    let mut p = clean();
    p.status = 404;
    p.indexability = Indexability::ClientError;
    p.fields = PageFields::default();
    let bits = page_issues(&p);
    for d in CHECKS.iter().filter(|d| d.scope != Scope::Page) {
        assert!(!bits.has_check(d.id), "{:?} is not a page check", d.id);
    }
}

#[test]
fn boundaries() {
    let title = |n: usize| {
        let mut p = clean();
        p.fields.title = Some("x".repeat(n));
        p
    };
    for n in [30, 60] {
        let b = page_issues(&title(n));
        assert!(!b.has_check(CheckId::TitleTooLong) && !b.has_check(CheckId::TitleTooShort));
    }
    assert!(has(&title(29), CheckId::TitleTooShort));
    assert!(has(&title(61), CheckId::TitleTooLong));

    let desc = |n: usize| {
        let mut p = clean();
        p.fields.meta_description = Some("x".repeat(n));
        p
    };
    for n in [70, 160] {
        let b = page_issues(&desc(n));
        assert!(
            !b.has_check(CheckId::DescriptionTooLong) && !b.has_check(CheckId::DescriptionTooShort)
        );
    }
    assert!(has(&desc(161), CheckId::DescriptionTooLong));
    assert!(has(&desc(69), CheckId::DescriptionTooShort));

    let mut p = clean();
    p.depth = Some(3);
    assert!(!has(&p, CheckId::DeepPage));
    p.depth = Some(4);
    assert!(has(&p, CheckId::DeepPage));

    let mut p = clean();
    p.response_ms = 1_000;
    assert!(!has(&p, CheckId::SlowResponse));
    p.response_ms = 1_001;
    assert!(has(&p, CheckId::SlowResponse));

    let mut p = clean();
    p.status = 404;
    p.indexability = Indexability::ClientError;
    p.fields.title = None;
    p.fields.h1.clear();
    p.fields.meta_description = None;
    let b = page_issues(&p);
    assert!(b.has_check(CheckId::Http4xx));
    for id in [
        CheckId::TitleMissing,
        CheckId::DescriptionMissing,
        CheckId::H1Missing,
        CheckId::ThinContent,
        CheckId::NoInternalOutlinks,
    ] {
        assert!(!b.has_check(id), "{id:?} must not fire on a 404");
    }
}

#[test]
fn multibyte_titles_count_chars() {
    let mut p = clean();
    p.fields.title = Some("日本語".repeat(20)); // 60 chars, 180 bytes
    assert!(!has(&p, CheckId::TitleTooLong));
}

#[test]
fn redirect_failure_kinds_are_separated() {
    let mut p = clean();
    p.status = 0;
    p.error = Some(FetchFailure::TooManyRedirects);
    assert!(has(&p, CheckId::RedirectLoop) && !has(&p, CheckId::FetchFailed));
    p.error = Some(FetchFailure::RedirectLoop);
    assert!(has(&p, CheckId::RedirectLoop) && !has(&p, CheckId::FetchFailed));
    p.error = Some(FetchFailure::Blocked);
    assert!(has(&p, CheckId::FetchFailed) && !has(&p, CheckId::RedirectLoop));

    let mut p = clean();
    p.status = 301;
    p.indexability = Indexability::Redirected;
    p.redirect_chain = vec![(301, u("https://e.com/a"))];
    assert!(has(&p, CheckId::Redirected) && !has(&p, CheckId::RedirectChain));
}

#[test]
fn indexability_gates_apply() {
    // Noindex pages are not asked for a canonical, og tags or content depth.
    let mut p = clean();
    p.indexability = Indexability::Noindex;
    p.fields.canonical = None;
    p.fields.og = OgTags::default();
    p.fields.word_count = 10;
    let b = page_issues(&p);
    assert!(b.has_check(CheckId::Noindex));
    for id in [
        CheckId::CanonicalMissing,
        CheckId::OgMissing,
        CheckId::ThinContent,
    ] {
        assert!(!b.has_check(id), "{id:?} only applies to indexable pages");
    }
}

#[test]
fn hreflang_absent_is_not_an_issue_and_https_rules() {
    let mut p = clean();
    p.fields.hreflang.clear();
    assert!(!has(&p, CheckId::HreflangMissingSelf));

    let mut p = clean();
    p.url = u("http://e.com/a");
    p.fields.mixed_content = 3;
    assert!(has(&p, CheckId::NotHttps) && !has(&p, CheckId::MixedContent));

    let mut p = clean();
    p.url = u("http://e.com/a");
    p.status = 301;
    p.indexability = Indexability::Redirected;
    assert!(!has(&p, CheckId::NotHttps));
}

#[test]
fn slow_response_ignores_pages_with_no_response() {
    let mut p = clean();
    p.status = 0;
    p.response_ms = 30_000;
    p.error = Some(FetchFailure::Timeout);
    assert!(!has(&p, CheckId::SlowResponse));
}
