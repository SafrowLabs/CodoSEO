mod common;

use std::collections::HashSet;

use codoseo_core::output::CrawlOutput;
use codoseo_core::page::{JsonLdStatus, MountPoint, PageRecord, RawSignals};
use codoseo_geo::content::{ContentVerdict, classify};
use codoseo_geo::report::{AccessReport, build_report, important_urls};
use common::*;

fn report_for(out: &CrawlOutput) -> AccessReport {
    let important = important_urls(&out.pages, &out.origin, &HashSet::new());
    build_report(out, &important)
}

/// A page with `words` words and these raw signals; no H1, no links, 1000 bytes.
fn raw(words: u32, raw: RawSignals) -> PageRecord {
    let mut p = page("/", 1);
    p.fields.word_count = words;
    p.fields.raw = raw;
    p
}

fn mount_empty() -> RawSignals {
    RawSignals {
        mount: Some(MountPoint::Root),
        mount_empty: true,
        ..RawSignals::default()
    }
}

fn noscript() -> RawSignals {
    RawSignals {
        noscript_js: true,
        ..RawSignals::default()
    }
}

fn script_bytes(n: u32) -> RawSignals {
    RawSignals {
        script_bytes: n,
        ..RawSignals::default()
    }
}

fn external(n: u16) -> RawSignals {
    RawSignals {
        scripts_external: n,
        ..RawSignals::default()
    }
}

fn with_h1(mut p: PageRecord) -> PageRecord {
    p.fields.h1 = vec!["Hello".to_owned()];
    p
}

fn with_internal_links(mut p: PageRecord, n: u32) -> PageRecord {
    p.outlinks_internal = n;
    p
}

#[test]
fn the_threshold_table() {
    use ContentVerdict::*;
    let cases: Vec<(&str, PageRecord, ContentVerdict)> = vec![
        ("no words, empty mount", raw(0, mount_empty()), Shell),
        ("49 words, noscript notice", raw(49, noscript()), Shell),
        // Disqus's "Please enable JavaScript to view the comments" under a short post.
        (
            "noscript notice under an H1",
            with_h1(raw(30, noscript())),
            Thin,
        ),
        ("50 words, empty mount", raw(50, mount_empty()), Present),
        (
            "empty mount under an H1",
            with_h1(raw(10, mount_empty())),
            Shell,
        ),
        ("few words, no signal", raw(10, RawSignals::default()), Thin),
        ("no words at all", raw(0, RawSignals::default()), Thin),
        ("60% inline script", raw(10, script_bytes(600)), LikelyShell),
        ("59% inline script", raw(10, script_bytes(590)), Thin),
        ("3 external scripts", raw(10, external(3)), LikelyShell),
        ("2 external scripts", raw(10, external(2)), Thin),
        ("scripts under an H1", with_h1(raw(10, external(5))), Thin),
        (
            "scripts with an internal link",
            with_internal_links(raw(10, external(5)), 1),
            Thin,
        ),
        ("49 words, many scripts", raw(49, external(9)), LikelyShell),
        ("50 words, many scripts", raw(50, external(9)), Present),
        ("plenty of words", raw(800, RawSignals::default()), Present),
    ];
    let mut wrong = Vec::new();
    for (name, page, want) in &cases {
        let got = classify(page);
        println!("{name:32} {got:?}");
        if got != *want {
            wrong.push(format!("{name}: got {got:?}, want {want:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_server_rendered_page_with_a_big_data_script_is_present() {
    // A Next.js page: most of its bytes are `__NEXT_DATA__` and it loads nine chunks, but the
    // text is in the HTML.
    let mut p = raw(
        320,
        RawSignals {
            main_words: 300,
            script_bytes: 40_000,
            scripts_external: 9,
            mount: Some(MountPoint::Next),
            mount_empty: false,
            noscript_js: false,
        },
    );
    p.size_bytes = 50_000;
    assert_eq!(classify(&p), ContentVerdict::Present);
    // Script share alone never flags, even with a short text.
    let mut short = with_h1(with_internal_links(raw(20, script_bytes(45_000)), 4));
    short.size_bytes = 50_000;
    assert_eq!(classify(&short), ContentVerdict::Thin);
}

#[test]
fn a_page_without_a_size_has_no_script_share() {
    let mut p = raw(0, script_bytes(500));
    p.size_bytes = 0;
    assert_eq!(classify(&p), ContentVerdict::Thin);
}

#[test]
fn non_html_pages_are_present_and_left_out_of_the_report() {
    let mut pdf = page("/guide.pdf", 1);
    pdf.content_type = Some("application/pdf".to_owned());
    pdf.fields.word_count = 0;
    assert_eq!(classify(&pdf), ContentVerdict::Present);

    let mut error = raw(0, mount_empty());
    error.status = 500;
    assert_eq!(classify(&error), ContentVerdict::Present);

    let mut home = page("/", 5);
    home.fields.word_count = 0;
    home.fields.raw = mount_empty();
    let report = report_for(&crawl(vec![home, pdf]));
    let content = report.content.as_ref().expect("the home page is HTML");
    let urls: Vec<&str> = content
        .pages
        .iter()
        .map(|c| report.important[usize::from(c.url)].url.as_str())
        .collect();
    assert_eq!(urls, ["https://example.com/"]);
    assert_eq!(content.pages[0].verdict, ContentVerdict::Shell);
}

#[test]
fn no_html_important_pages_means_no_content_report() {
    let out = crawl(vec![page("/", 1).status(503)]);
    assert!(report_for(&out).content.is_none());
    // An old report without the field reads back as None.
    let mut json = serde_json::to_value(report_for(&site(2, |p| p))).expect("json");
    json.as_object_mut().expect("object").remove("content");
    let back: AccessReport = serde_json::from_value(json).expect("old report");
    assert!(back.content.is_none());
}

#[test]
fn the_report_keeps_each_html_pages_facts() {
    let mut home = page("/", 3);
    home.size_bytes = 2_000;
    home.outlinks_internal = 0;
    home.fields.word_count = 4;
    home.fields.title = Some("App".to_owned());
    home.fields.meta_description = Some("An app".to_owned());
    home.fields.raw = RawSignals {
        main_words: 0,
        script_bytes: 1_300,
        scripts_external: 4,
        mount: Some(MountPoint::App),
        mount_empty: true,
        noscript_js: true,
    };
    let mut about = page("/about", 2);
    about.fields.h1 = vec!["About".to_owned()];
    about.fields.canonical = Some(url("/about"));
    about.fields.meta_robots = Some("max-snippet:50".to_owned());
    about.fields.jsonld = JsonLdStatus::Valid(2);
    about.outlinks_internal = 7;
    about.fields.raw.main_words = 90;
    let report = report_for(&crawl(vec![home, about]));
    let content = report.content.expect("content");
    assert_eq!(content.pages.len(), 2);

    let h = &content.pages[0];
    assert_eq!(h.url, 0);
    assert_eq!(h.verdict, ContentVerdict::Shell);
    assert_eq!((h.words, h.main_words), (4, 0));
    assert!(h.title && h.description && !h.h1 && !h.canonical && !h.robots);
    assert_eq!(h.jsonld, JsonLdStatus::Absent);
    assert_eq!(h.links_internal, 0);
    assert_eq!(h.script_share, 65);
    assert_eq!(h.scripts_external, 4);
    assert_eq!(h.mount, Some(MountPoint::App));
    assert!(h.mount_empty && h.noscript_js);

    let a = &content.pages[1];
    assert_eq!(a.url, 1);
    assert_eq!(a.verdict, ContentVerdict::Present);
    assert!(a.h1 && a.canonical && a.robots && !a.title);
    assert_eq!(a.jsonld, JsonLdStatus::Valid(2));
    assert_eq!((a.words, a.main_words, a.links_internal), (100, 90, 7));
    assert_eq!(a.mount, None);
}
