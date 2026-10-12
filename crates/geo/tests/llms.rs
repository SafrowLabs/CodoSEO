mod common;

use std::collections::HashSet;

use codoseo_core::output::{CrawlOutput, LinkStatus, LlmsTxtFetch};
use codoseo_geo::llms::{
    LlmsCheckId, LlmsState, LlmsTxtReport, MIN_CHARS, NOTICE_BYTES, assess, carry_adoption, links,
    parse,
};
use codoseo_geo::report::{AccessReport, build_report, important_urls};
use common::*;

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/llms/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn served(status: u16, content_type: &str, body: &str) -> LlmsTxtFetch {
    LlmsTxtFetch {
        status,
        content_type: (!content_type.is_empty()).then(|| content_type.to_owned()),
        bytes: body.len() as u64,
        body: if (200..300).contains(&status) {
            body.to_owned()
        } else {
            String::new()
        },
        ..LlmsTxtFetch::default()
    }
}

fn file(body: &str) -> LlmsTxtFetch {
    served(200, "text/plain; charset=utf-8", body)
}

fn no_response(error: &str) -> LlmsTxtFetch {
    LlmsTxtFetch {
        error: Some(error.to_owned()),
        ..LlmsTxtFetch::default()
    }
}

fn ok(r: &LlmsTxtReport, id: LlmsCheckId) -> bool {
    r.check(id).unwrap_or_else(|| panic!("{id:?} in {r:?}")).ok
}

fn report_for(out: &CrawlOutput) -> AccessReport {
    let important = important_urls(&out.pages, &out.origin, &HashSet::new());
    build_report(out, &important)
}

// ---- parse ------------------------------------------------------------------------------------

#[test]
fn parses_the_spec_sites_own_file() {
    let doc = parse(&fixture("llmstxt_org.txt"));
    assert_eq!(doc.title.as_deref(), Some("llms.txt"));
    assert!(doc.h1_first);
    assert!(
        doc.summary
            .as_deref()
            .unwrap()
            .starts_with("A proposal that")
    );
    assert_eq!(doc.sections.len(), 1);
    let docs = &doc.sections[0];
    assert_eq!(docs.title, "Docs");
    assert_eq!(docs.other_lines, 0);
    assert_eq!(docs.links.len(), 3);
    assert_eq!(docs.links[0].name, "llms.txt proposal");
    assert_eq!(docs.links[0].url, "https://llmstxt.org/index.md");
    assert!(doc.optional().is_none());
}

#[test]
fn parses_sections_details_and_the_optional_section() {
    let doc = parse(&fixture("docs_site.txt"));
    assert_eq!(doc.title.as_deref(), Some("Acme Docs"));
    let titles: Vec<&str> = doc.sections.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, ["Getting started", "API reference", "Optional"]);
    assert_eq!(doc.optional().unwrap().links.len(), 2);
    assert_eq!(links(&doc).len(), 8);
}

#[test]
fn a_bom_and_crlf_line_ends_are_read_through() {
    let body = "\u{feff}# Site\r\n\r\n> Summary\r\n\r\n## Docs\r\n\r\n- [A](https://a.test/a.md): notes\r\n";
    let doc = parse(body);
    assert_eq!(doc.title.as_deref(), Some("Site"));
    assert!(doc.h1_first);
    assert_eq!(doc.summary.as_deref(), Some("Summary"));
    assert_eq!(doc.sections[0].links[0].url, "https://a.test/a.md");
    assert_eq!(doc.chars, body.chars().count() - 1);
}

#[test]
fn a_multi_line_summary_is_joined_and_a_late_quote_is_not_a_summary() {
    let doc = parse("# S\n> one\n> two\n");
    assert_eq!(doc.summary.as_deref(), Some("one two"));
    let doc = parse("# S\n\nSome text.\n\n> a quote\n");
    assert_eq!(doc.summary, None);
}

#[test]
fn an_h1_after_other_content_is_not_first() {
    let doc = parse("Welcome!\n\n# Site\n");
    assert_eq!(doc.title.as_deref(), Some("Site"));
    assert!(!doc.h1_first);
    // `#Site` without a space is not a heading.
    assert_eq!(parse("#Site\n").title, None);
}

#[test]
fn headings_and_links_inside_code_fences_are_ignored() {
    let doc = parse("# S\n\n## Docs\n\n```\n# not a title\n- [x](https://x.test/)\n```\n");
    assert_eq!(doc.sections.len(), 1);
    assert!(doc.sections[0].links.is_empty());
    assert_eq!(doc.sections[0].other_lines, 4);
    assert!(doc.urls.is_empty());
}

#[test]
fn plain_list_items_and_prose_count_as_other_lines() {
    let doc = parse(
        "# S\n\n## When to use\n\n- Deploy apps\n- Run code\n\nSee also [x](https://x.test/x).\n",
    );
    let s = &doc.sections[0];
    assert!(s.links.is_empty());
    assert_eq!(s.other_lines, 3);
    assert_eq!(doc.urls, ["https://x.test/x"]);
}

#[test]
fn links_keeps_http_and_relative_urls_once_in_file_order() {
    let doc = parse(
        "# S\n\n## Docs\n\n- [A](/a.md)\n- [B](https://b.test/b.md \"Title\")\n- [C](<https://c.test/c (1).md>)\n- [A again](/a.md)\n- [Mail](mailto:x@y.test)\n- [Top](#top)\n- [Paren](https://w.test/Foo_(bar))\n",
    );
    assert_eq!(
        links(&doc),
        [
            "/a.md",
            "https://b.test/b.md",
            "https://c.test/c (1).md",
            "https://w.test/Foo_(bar)"
        ]
    );
}

#[test]
fn list_items_with_star_or_plus_and_dash_notes_are_file_list_entries() {
    let doc = parse("# S\n## D\n* [A](https://a.test/)\n+ [B](https://b.test/) - notes\n");
    assert_eq!(doc.sections[0].links.len(), 2);
    assert_eq!(doc.sections[0].other_lines, 0);
}

// ---- assess: status ---------------------------------------------------------------------------

#[test]
fn status_follows_lighthouse() {
    // Lighthouse: 5xx and 429 fail, any other 4xx is not applicable.
    for (status, state) in [
        (200, LlmsState::Available),
        (204, LlmsState::Available),
        (404, LlmsState::Absent),
        (410, LlmsState::Absent),
        (401, LlmsState::Absent),
        (403, LlmsState::Absent),
        (429, LlmsState::Unavailable),
        (500, LlmsState::Unavailable),
        (503, LlmsState::Unavailable),
        (304, LlmsState::Unavailable),
    ] {
        let r = assess(&served(status, "text/plain", "# S\n"), false);
        assert_eq!(r.state, state, "{status}");
        assert_eq!(r.status, status);
        if state != LlmsState::Available {
            assert!(r.checks.is_empty(), "{status}: no checks without a file");
        }
    }
}

#[test]
fn no_response_is_unavailable_with_its_error() {
    let r = assess(&no_response("request timed out"), false);
    assert_eq!(r.state, LlmsState::Unavailable);
    assert_eq!(
        (r.status, r.error.as_deref()),
        (0, Some("request timed out"))
    );
    assert!(!r.adopted);
}

#[test]
fn an_html_page_served_with_200_is_a_soft_404() {
    let page = "<!DOCTYPE html><html><head><title>Not found</title></head><body>Oops</body></html>";
    for ct in ["text/html; charset=utf-8", "text/plain", ""] {
        let r = assess(&served(200, ct, page), true);
        assert_eq!(r.state, LlmsState::SoftNotFound, "{ct}");
        assert!(!r.adopted, "a soft 404 is no file, whatever came before");
        assert!(r.checks.is_empty());
    }
    // Served as HTML with no Markdown heading at all, even without tags.
    assert_eq!(
        assess(&served(200, "text/html", ""), false).state,
        LlmsState::SoftNotFound
    );
    // A real llms.txt served as text/html is still a file; only the MIME check fails.
    let r = assess(
        &served(200, "text/html", &fixture("llmstxt_org.txt")),
        false,
    );
    assert_eq!(r.state, LlmsState::Available);
    assert!(!ok(&r, LlmsCheckId::Mime));
}

// ---- assess: checks ---------------------------------------------------------------------------

#[test]
fn the_spec_sites_file_passes_every_check() {
    let r = assess(&file(&fixture("llmstxt_org.txt")), false);
    assert_eq!(r.state, LlmsState::Available);
    assert!(r.adopted);
    assert_eq!(r.title.as_deref(), Some("llms.txt"));
    for c in &r.checks {
        assert!(c.ok, "{c:?}");
    }
    assert_eq!(r.checks.len(), 8);
    assert!(!r.optional_section);
}

#[test]
fn a_file_without_h1_or_links_fails_those_checks() {
    let r = assess(
        &file("Just some words about the site, with no heading at all."),
        false,
    );
    assert!(!ok(&r, LlmsCheckId::H1));
    assert!(!ok(&r, LlmsCheckId::Links));
    assert!(!ok(&r, LlmsCheckId::Sections));
    assert!(!ok(&r, LlmsCheckId::Summary));
    assert!(ok(&r, LlmsCheckId::Length));
}

#[test]
fn a_short_file_is_suspiciously_short() {
    let body = "# S\n[a](https://a.test/)";
    assert!(body.chars().count() < MIN_CHARS);
    let r = assess(&file(body), false);
    assert!(!ok(&r, LlmsCheckId::Length));
    assert!(ok(&r, LlmsCheckId::H1));
    assert!(ok(&r, LlmsCheckId::Links));
}

#[test]
fn size_gives_a_notice_past_100_kib_and_says_when_truncated() {
    let mut f = file(&fixture("docs_site.txt"));
    f.bytes = NOTICE_BYTES + 1;
    let r = assess(&f, false);
    let c = r.check(LlmsCheckId::Size).unwrap();
    assert!(!c.ok);
    assert!(c.detail.contains("over 100 KiB"), "{}", c.detail);
    f.bytes = 900 * 1024;
    f.truncated = true;
    let c = assess(&f, false).check(LlmsCheckId::Size).unwrap().clone();
    assert!(!c.ok);
    assert!(c.detail.contains("first 512 KiB"), "{}", c.detail);
    f.bytes = NOTICE_BYTES;
    f.truncated = false;
    assert!(ok(&assess(&f, false), LlmsCheckId::Size));
}

#[test]
fn mime_must_be_plain_text_or_markdown() {
    let body = fixture("llmstxt_org.txt");
    for (ct, good) in [
        ("text/plain; charset=utf-8", true),
        ("text/markdown", true),
        ("Text/Markdown; charset=UTF-8", true),
        ("application/octet-stream", false),
        ("", false),
    ] {
        assert_eq!(
            ok(&assess(&served(200, ct, &body), false), LlmsCheckId::Mime),
            good,
            "{ct}"
        );
    }
}

#[test]
fn link_summary_counts_checked_failing_and_unchecked_links() {
    let mut f = file(&fixture("docs_site.txt"));
    let status = |url: &str, status: Option<u16>, from_crawl: bool| LinkStatus {
        url: url.to_owned(),
        status,
        from_crawl,
    };
    f.links = vec![
        status("https://docs.acme.test/quickstart.md", Some(200), true),
        status("https://docs.acme.test/auth.md", Some(404), false),
        status("https://docs.acme.test/webhooks.md", None, false),
        status("https://docs.acme.test/api/payments.md", Some(301), false),
    ];
    f.external_unchecked = 1;
    let r = assess(&f, false);
    let s = &r.link_summary;
    assert_eq!((s.checked, s.failing, s.not_checked), (4, 2, 4));
    assert_eq!(
        s.failing_urls,
        [
            "https://docs.acme.test/auth.md",
            "https://docs.acme.test/webhooks.md"
        ]
    );
    let c = r.check(LlmsCheckId::LinksResolve).unwrap();
    assert!(!c.ok);
    assert_eq!(c.detail, "2 of 4 checked links fail.");
    assert!(r.optional_section);
}

#[test]
fn no_checked_links_is_not_a_failure() {
    let r = assess(&file(&fixture("docs_site.txt")), false);
    assert!(ok(&r, LlmsCheckId::LinksResolve));
    assert_eq!(r.link_summary.not_checked, 8);
}

// ---- adoption -----------------------------------------------------------------------------------

#[test]
fn adoption_carries_over_failures_only() {
    let available = assess(&file(&fixture("llmstxt_org.txt")), false);
    let failing = assess(&served(503, "text/html", ""), false);
    let gone = assess(&served(404, "text/html", ""), false);
    let soft = assess(&served(200, "text/html", "<html></html>"), false);

    assert!(carry_adoption(None, &available));
    assert!(
        !carry_adoption(None, &failing),
        "no history: a failure is not adoption"
    );
    assert!(carry_adoption(Some(&available), &failing));
    assert!(!carry_adoption(Some(&failing), &failing));
    assert!(!carry_adoption(Some(&available), &gone));
    assert!(!carry_adoption(Some(&available), &soft));

    // A failure after a failure keeps adoption as long as it was carried.
    let mut still = failing.clone();
    still.adopted = carry_adoption(Some(&available), &failing);
    assert!(carry_adoption(Some(&still), &failing));
    // assess takes the carried value directly.
    assert!(assess(&served(500, "", ""), true).adopted);
    assert!(!assess(&served(404, "", ""), true).adopted);
}

// ---- the access report --------------------------------------------------------------------------

#[test]
fn the_access_report_carries_llms_txt_without_history() {
    let mut out = site(3, |p| p);
    assert_eq!(report_for(&out).llms_txt, None);
    let v = serde_json::to_value(report_for(&out)).unwrap();
    assert!(v.get("llms_txt").is_none());

    out.signals.llms_txt = Some(file(&fixture("llmstxt_org.txt")));
    let r = report_for(&out).llms_txt.unwrap();
    assert_eq!(r.state, LlmsState::Available);

    out.signals.llms_txt = Some(served(502, "text/html", ""));
    let r = report_for(&out).llms_txt.unwrap();
    assert_eq!(r.state, LlmsState::Unavailable);
    assert!(!r.adopted, "build_report knows no history");
}

#[test]
fn an_older_report_without_llms_txt_still_reads_and_the_section_round_trips() {
    let mut out = site(2, |p| p);
    out.signals.llms_txt = Some(file(&fixture("docs_site.txt")));
    let report = report_for(&out);
    let mut v = serde_json::to_value(&report).unwrap();
    assert_eq!(v["llms_txt"]["state"], "available");
    assert_eq!(v["llms_txt"]["checks"][0]["id"], "h1");
    let back: AccessReport = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(back, report);
    v.as_object_mut().unwrap().remove("llms_txt");
    let old: AccessReport = serde_json::from_value(v).unwrap();
    assert_eq!(old.llms_txt, None);
}

#[test]
fn hostile_bodies_parse_in_linear_time() {
    let size = 512 * 1024;
    let mut nested_links = "[a](".repeat(size / 4);
    nested_links.push(')');
    let mut many_links = "# T\n\n## Docs\n\n".to_owned();
    while many_links.len() < size {
        many_links.push_str(&format!("[x](/p{}) ", many_links.len()));
    }
    let bodies = [
        "[".repeat(size),
        format!("{}]", "[".repeat(size)),
        "[a](".repeat(size / 4),
        nested_links,
        format!("[a]({}", "(".repeat(size)),
        "[a](/x)".repeat(size / 7),
        many_links,
    ];
    for body in &bodies {
        let start = std::time::Instant::now();
        let doc = parse(body);
        let urls = links(&doc);
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "{} bytes starting {:?} took {elapsed:?}",
            body.len(),
            &body[..8]
        );
        assert!(doc.urls.len() <= codoseo_geo::llms::MAX_LINKS);
        assert!(urls.len() <= codoseo_geo::llms::MAX_LINKS);
    }
    // The cap holds the first links in file order.
    let doc = parse(&bodies[5]);
    assert_eq!(doc.urls.len(), codoseo_geo::llms::MAX_LINKS);
    assert_eq!(links(&doc), vec!["/x".to_owned()]);
}

#[test]
fn not_checked_counts_each_link_once_however_it_is_written() {
    // "/a", "a" and the absolute URL are the same page once resolved against the file's URL;
    // "/b#x" is "/b".
    let mut f = file(
        "# T\n\n## Docs\n\n- [A](/a)\n- [A again](https://example.com/a)\n- [B](/b)\n- [B part](/b#x)\n- [a](a)\n",
    );
    f.final_url = Some("https://example.com/llms.txt".to_owned());
    f.links = vec![LinkStatus {
        url: "https://example.com/a".to_owned(),
        status: Some(200),
        from_crawl: true,
    }];
    let s = assess(&f, false).link_summary;
    assert_eq!((s.checked, s.not_checked), (1, 1), "{s:?}");
}

#[test]
fn a_huge_h1_and_section_names_are_capped() {
    let h1 = "Ü".repeat(100 * 1024);
    let bare: String = (0..2000)
        .map(|i| format!("## Section number {i}\n\nprose\n\n"))
        .collect();
    let body = format!("# {h1}\n\n> Summary.\n\n## Docs\n\n- [A](/a)\n\n{bare}");
    let r = assess(&file(&body), false);
    let title = r.title.as_deref().unwrap();
    assert!(
        title.chars().count() <= codoseo_geo::llms::MAX_TITLE,
        "{}",
        title.len()
    );
    assert!(title.ends_with('…'));
    for c in &r.checks {
        assert!(
            c.detail.chars().count() <= codoseo_geo::llms::MAX_DETAIL,
            "{:?}: {} chars",
            c.id,
            c.detail.chars().count()
        );
    }
    let mut f = file(&body);
    f.links = vec![LinkStatus {
        url: format!("https://example.com/{}", "x".repeat(10_000)),
        status: Some(404),
        from_crawl: false,
    }];
    let r = assess(&f, false);
    assert!(r.link_summary.failing_urls[0].chars().count() <= 2048);
}
