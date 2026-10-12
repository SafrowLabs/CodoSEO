use codoseo_core::Url;
use codoseo_core::page::{JsonLdStatus, MountPoint, RawSignals};
use codoseo_crawler::extract::{Extracted, Extractor, extract};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn page(url: &str) -> Url {
    Url::parse(url).unwrap()
}

fn extract_at(name: &str, url: &str, content_type: Option<&str>) -> Extracted {
    extract(&page(url), content_type, &fixture(name))
}

fn extract_file(name: &str) -> Extracted {
    extract_at(name, "https://northwind.test/tents", Some("text/html"))
}

fn extract_chunked(name: &str, size: usize) -> Extracted {
    let mut e = Extractor::new(&page("https://northwind.test/tents"), Some("text/html"));
    for chunk in fixture(name).chunks(size) {
        e.write(chunk);
    }
    e.finish()
}

#[test]
fn basic_page_snapshot() {
    insta::assert_yaml_snapshot!(extract_file("basic.html"));
}

#[test]
fn basic_page_fields() {
    let e = extract_file("basic.html");
    let f = &e.fields;
    assert_eq!(f.title.as_deref(), Some("Tents & Shelters | Northwind"));
    assert_eq!(f.title_count, 1);
    assert_eq!(
        f.meta_description.as_deref(),
        Some("Backpacking tents & tarps.")
    );
    assert_eq!(
        f.canonical.as_ref().map(Url::as_str),
        Some("https://northwind.test/tents")
    );
    assert_eq!(
        f.hreflang,
        vec![("de".to_owned(), page("https://northwind.test/de/zelte"))]
    );
    assert_eq!(f.h1, vec!["Tents & Shelters"]);
    assert_eq!(f.h2, vec!["Two person", "Family"]);
    assert_eq!(
        f.word_count, 16,
        "script, style and title text are not counted"
    );
    assert_ne!(f.content_hash, 0);
    let links: Vec<(&str, &str)> = e
        .links
        .iter()
        .map(|l| (l.url.as_str(), l.anchor.as_str()))
        .collect();
    assert_eq!(
        links,
        vec![
            ("https://northwind.test/tents/ridgeline-2p", "Ridgeline 2P"),
            ("https://other.test/x", "Partner"),
        ]
    );
}

#[test]
fn collects_robots_meta_case_insensitively() {
    let f = extract_file("noindex.html").fields;
    assert_eq!(f.meta_robots.as_deref(), Some("noindex, follow, nosnippet"));
    assert_eq!(f.x_robots_tag, None, "headers are the crawler's job");
}

#[test]
fn resolves_a_relative_canonical() {
    let f = extract_at("canonical_relative.html", "https://ex.test/a/b/page", None).fields;
    assert_eq!(
        f.canonical.unwrap().as_str(),
        "https://ex.test/a/shop/item?id=1"
    );
}

#[test]
fn keeps_every_h1_and_ignores_svg_titles() {
    let f = extract_file("multi_h1.html").fields;
    assert_eq!(f.h1, vec!["One", "Two"]);
    assert_eq!(f.title.as_deref(), Some("Real"));
    assert_eq!(f.title_count, 1);
}

#[test]
fn checks_json_ld_blocks() {
    assert_eq!(
        extract_file("jsonld_valid.html").fields.jsonld,
        JsonLdStatus::Valid(2)
    );
    assert_eq!(
        extract_file("jsonld_invalid.html").fields.jsonld,
        JsonLdStatus::Invalid
    );
    assert_eq!(
        extract_file("basic.html").fields.jsonld,
        JsonLdStatus::Absent
    );
}

#[test]
fn reads_open_graph_tags() {
    let og = extract_file("og.html").fields.og;
    assert_eq!(og.title.as_deref(), Some("OG Title"));
    assert_eq!(og.description.as_deref(), Some("OG desc"));
    assert_eq!(og.image.as_deref(), Some("https://img.test/a.jpg"));
}

#[test]
fn counts_images_without_alt_but_not_empty_alt() {
    assert_eq!(extract_file("img_alt.html").fields.images_missing_alt, 2);
}

#[test]
fn counts_http_resources_on_https_pages() {
    assert_eq!(extract_file("mixed_content.html").fields.mixed_content, 3);
    let on_http = extract_at("mixed_content.html", "http://northwind.test/", None);
    assert_eq!(on_http.fields.mixed_content, 0);
}

#[test]
fn records_nofollow() {
    let flags: Vec<bool> = extract_file("nofollow.html")
        .links
        .iter()
        .map(|l| l.nofollow)
        .collect();
    assert_eq!(flags, vec![true, true, false]);
}

#[test]
fn resolves_everything_against_the_first_base_href() {
    let e = extract_file("base_href.html");
    assert_eq!(
        e.fields.canonical.unwrap().as_str(),
        "https://cdn.test/docs/page"
    );
    assert_eq!(e.links[0].url.as_str(), "https://cdn.test/docs/intro");
}

// Review focus 1: legacy encodings and broken markup.

#[test]
fn decodes_windows_1252_from_the_meta_tag() {
    let f = extract_file("win1252.html").fields;
    assert_eq!(f.title.as_deref(), Some("Café café"));
}

#[test]
fn header_charset_wins() {
    let f = extract_at(
        "win1252_no_meta.html",
        "https://e.test/",
        Some("text/html; charset=windows-1252"),
    )
    .fields;
    assert_eq!(f.title.as_deref(), Some("Café"));
}

#[test]
fn decodes_shift_jis() {
    assert_eq!(
        extract_file("shift_jis.html").fields.title.as_deref(),
        Some("日本語のページ")
    );
}

#[test]
fn strips_a_utf8_bom() {
    assert_eq!(
        extract_file("bom_utf8.html").fields.title.as_deref(),
        Some("BOM page")
    );
}

#[test]
fn transcodes_utf16() {
    let e = extract_file("utf16le.html");
    assert_eq!(e.fields.title.as_deref(), Some("Wide page"));
    assert_eq!(e.links[0].url.as_str(), "https://northwind.test/w");
}

#[test]
fn keeps_the_first_of_two_titles() {
    let f = extract_file("two_titles.html").fields;
    assert_eq!(f.title.as_deref(), Some("First"));
    assert_eq!(f.title_count, 2);
}

#[test]
fn copes_with_missing_closing_tags() {
    let f = extract_file("no_head_close.html").fields;
    assert_eq!(f.title.as_deref(), Some("Open"));
    assert_eq!(f.meta_description.as_deref(), Some("Still found"));
}

#[test]
fn chunk_boundaries_do_not_change_the_result() {
    let whole = extract_file("basic.html");
    assert_eq!(extract_chunked("basic.html", 1), whole);
    assert_eq!(extract_chunked("basic.html", 7), whole);
}

#[test]
fn never_panics_on_binary_garbage() {
    let garbage: Vec<u8> = (0..20_000u32).map(|i| (i * 31 % 256) as u8).collect();
    let e = extract(&page("https://e.test/"), Some("text/html"), &garbage);
    assert!(e.fields.title.is_none());
}

// Fixes from the M1 review.

fn extract_html(html: &str) -> Extracted {
    extract(
        &page("https://northwind.test/"),
        Some("text/html"),
        html.as_bytes(),
    )
}

#[test]
fn attribute_urls_keep_legacy_entity_lookalikes() {
    let e = extract_html(
        r#"<link rel="canonical" href="/s?q=tent&region=us&section=2"><a href="/p?a=1&copy=2&amp;b=3&not=4">x</a>"#,
    );
    assert_eq!(
        e.fields.canonical.unwrap().as_str(),
        "https://northwind.test/s?q=tent&region=us&section=2"
    );
    assert_eq!(
        e.links[0].url.as_str(),
        "https://northwind.test/p?a=1&copy=2&b=3&not=4"
    );
}

#[test]
fn oversized_json_ld_is_reported_as_too_large() {
    let html = format!(
        r#"<script type="application/ld+json">[{}[]]</script>"#,
        "[],".repeat(400_000)
    );
    assert_eq!(extract_html(&html).fields.jsonld, JsonLdStatus::TooLarge);
}

#[test]
fn finds_a_meta_charset_after_the_title() {
    let mut html = b"<html><head><title>Caf".to_vec();
    html.push(0xE9);
    html.extend_from_slice(b"</title><meta charset=\"windows-1252\"></head><body></body></html>");
    let whole = extract(&page("https://e.test/"), None, &html);
    assert_eq!(whole.fields.title.as_deref(), Some("Café"));
    let mut chunked = Extractor::new(&page("https://e.test/"), None);
    for b in &html {
        chunked.write(std::slice::from_ref(b));
    }
    assert_eq!(chunked.finish(), whole);
}

#[test]
fn heading_and_link_text_keep_word_breaks() {
    let e = extract_html(
        r#"<h1>Tents<br>Shelters</h1><a href="/r"><h3>Ridgeline 2P</h3><p>Light tent</p></a><h1>A<h2>B</h2>"#,
    );
    assert_eq!(e.fields.h1, vec!["Tents Shelters", "A"]);
    assert_eq!(e.fields.h2, vec!["B"]);
    assert_eq!(e.links[0].anchor, "Ridgeline 2P Light tent");
}

// AI answer-engine markup (GA.3).

#[test]
fn ai_meta_snapshot() {
    insta::assert_yaml_snapshot!(extract_file("ai_meta.html").fields.ai);
}

#[test]
fn collects_bot_named_metas_and_tdm() {
    let ai = extract_file("ai_meta.html").fields.ai;
    assert_eq!(
        ai.bot_meta,
        vec![
            (
                "googlebot".to_owned(),
                "noarchive, max-snippet: 50".to_owned()
            ),
            ("bingbot".to_owned(), "nosnippet".to_owned()),
            ("oai-searchbot".to_owned(), "noindex".to_owned()),
            ("claude-searchbot".to_owned(), "noindex".to_owned()),
        ],
        "plain robots and unrelated metas are not bot metas"
    );
    assert_eq!(ai.tdm_reservation.as_deref(), Some("1"));
    assert_eq!(
        ai.tdm_policy.as_deref(),
        Some("https://northwind.test/tdm-policy.json")
    );
}

#[test]
fn nested_data_nosnippet_counts_each_word_once() {
    let e = extract_file("ai_meta.html");
    // "Two nested words here" and "hidden"; the script text is not visible text.
    assert_eq!(e.fields.ai.nosnippet_words, 5);
    // 5 open words, the 4 in the first block, then "Open again hidden".
    assert_eq!(e.fields.word_count, 5 + 4 + 3);
}

#[test]
fn bot_meta_is_capped_and_empty_without_markup() {
    let metas: String = (0..40)
        .map(|i| format!(r#"<meta name="googlebot" content="max-snippet:{i}">"#))
        .collect();
    let e = extract_html(&format!("<head>{metas}</head><body>x</body>"));
    assert_eq!(e.fields.ai.bot_meta.len(), 16);
    assert!(extract_html("<p>plain</p>").fields.ai.is_empty());
}

#[test]
fn data_nosnippet_edge_cases_never_stick() {
    let n = |html: &str| extract_html(html).fields.ai.nosnippet_words;
    // Void elements have no content.
    assert_eq!(n(r#"<img data-nosnippet src="a.png"> after one"#), 0);
    assert_eq!(n(r#"<br data-nosnippet>one two"#), 0);
    // Self-closing and empty attribute forms.
    assert_eq!(n(r#"<div data-nosnippet>a b</div> c d e"#), 2);
    assert_eq!(n(r#"<div data-nosnippet="">a b</div> c d e"#), 2);
    assert_eq!(n(r#"<DIV DATA-NOSNIPPET>a b</DIV> c"#), 2);
    // Words glued across the boundary stay separate counts.
    assert_eq!(n(r#"x<span data-nosnippet>y</span>z"#), 1);
    // Style and script inside are ignored.
    assert_eq!(n(r#"<div data-nosnippet>a<style>b c d</style> e</div>"#), 2);
    // Broken markup: missing end tags, stray end tags and unclosed elements must not panic.
    // An unclosed element runs to the end of the document, like the browser's tree would.
    assert_eq!(n(r#"<div data-nosnippet>a b"#), 2);
    assert_eq!(n(r#"</div></div><div data-nosnippet>a</div></div> b c"#), 1);
    // Only span, div and section count (Google's list): they need end tags, so an unclosed
    // `<p>` or `<li>` can't mark the rest of the page.
    assert_eq!(n("<ul><li data-nosnippet>a<li>b</ul> c d"), 0);
    assert_eq!(n("<p data-nosnippet>a<p>b c<div>d</div> e"), 0);
    assert_eq!(n(r#"<p>Open again <b data-nosnippet>hidden</b></p>"#), 0);
    // Never past the element's parent.
    assert_eq!(n("<div><div data-nosnippet>a</div></div> b c"), 1);
}

// Raw-HTML signals for SPA-shell detection (B2.2).

#[test]
fn a_create_react_app_shell_has_an_empty_mount_and_a_noscript_notice() {
    let f = extract_file("cra_shell.html").fields;
    assert_eq!(f.word_count, 0, "the noscript notice is not visible text");
    assert_eq!(
        f.raw,
        RawSignals {
            main_words: 0,
            script_bytes: 0,
            scripts_external: 1,
            mount: Some(MountPoint::Root),
            mount_empty: true,
            noscript_js: true,
        }
    );
}

#[test]
fn a_server_rendered_next_page_has_a_filled_mount_and_a_big_data_script() {
    let f = extract_file("next_ssr.html").fields;
    let raw = f.raw;
    assert_eq!(raw.mount, Some(MountPoint::Next));
    assert!(!raw.mount_empty);
    assert!(
        !raw.noscript_js,
        "an empty `<noscript data-n-css>` says nothing"
    );
    // polyfills, five chunks and the two manifests.
    assert_eq!(raw.scripts_external, 8);
    // Only `__NEXT_DATA__` is inline.
    assert_eq!(raw.script_bytes, 3984);
    // The nav and footer links are outside `<main>`.
    assert!(raw.main_words > 50, "{raw:?}");
    assert_eq!(f.word_count - raw.main_words, 4, "{raw:?}");
}

#[test]
fn a_tag_manager_noscript_iframe_is_not_a_javascript_notice() {
    let raw = extract_file("gtm_noscript.html").fields.raw;
    assert!(!raw.noscript_js);
    assert_eq!(raw.mount, None);
    assert!(!raw.mount_empty);
    assert_eq!(raw.scripts_external, 0);
    assert!(raw.script_bytes > 300, "{raw:?}");
    assert_eq!(raw.main_words, 9, "Spring sale, then seven words");
}

#[test]
fn raw_signals_do_not_depend_on_chunk_boundaries() {
    for name in ["cra_shell.html", "next_ssr.html", "gtm_noscript.html"] {
        for size in [1, 7, 64] {
            assert_eq!(
                extract_chunked(name, size).fields.raw,
                extract_file(name).fields.raw,
                "{name} in {size}-byte chunks"
            );
        }
    }
}

#[test]
fn noscript_notices_match_ignoring_case_and_markup() {
    let js = |html: &str| extract_html(html).fields.raw.noscript_js;
    // Vue CLI's template, with markup inside (lol_html reads noscript as raw text).
    assert!(js(
        "<noscript><strong>We're sorry but Shop doesn't work properly without \
         JavaScript enabled. Please enable it to continue.</strong></noscript>"
    ));
    // Angular's.
    assert!(js(
        "<noscript>Please enable JavaScript to continue using this application.</noscript>"
    ));
    assert!(js("<NOSCRIPT>This site REQUIRES\n  JavaScript.</NOSCRIPT>"));
    assert!(js(
        "<noscript>JavaScript is disabled in your browser.</noscript>"
    ));
    assert!(js("<noscript>Turn on JavaScript to see prices.</noscript>"));
    assert!(js(
        "<noscript><p>JavaScript must be enabled.</p></noscript>"
    ));
    // A tag's attributes are not text.
    assert!(!js(
        r#"<noscript><img alt="please enable javascript" src="/px.gif"></noscript>"#
    ));
    // AMP's boilerplate style, a tracking pixel, and no notice at all.
    assert!(!js(
        "<noscript><style amp-boilerplate>body{-webkit-animation:none;animation:none}</style></noscript>"
    ));
    assert!(!js(
        r#"<noscript><img height="1" width="1" src="https://www.facebook.com/tr?id=1&ev=PageView&noscript=1"/></noscript>"#
    ));
    assert!(
        !js("<p>Please enable JavaScript.</p>"),
        "only inside noscript"
    );
    // Only the first 256 bytes of a block are read.
    let long = format!(
        "<noscript>{} enable JavaScript</noscript>",
        "padding ".repeat(40)
    );
    assert!(!js(&long));
    // Each block is read on its own.
    assert!(js(&format!(
        "<noscript>{}</noscript><noscript>Enable JavaScript</noscript>",
        "padding ".repeat(40)
    )));
}

#[test]
fn mount_points_are_empty_only_without_children_or_text() {
    let raw = |html: &str| extract_html(html).fields.raw;
    let mount = |html: &str| {
        let r = raw(html);
        (r.mount, r.mount_empty)
    };
    assert_eq!(
        mount("<body><app-root></app-root></body>"),
        (Some(MountPoint::AppRoot), true)
    );
    assert_eq!(
        mount("<div id=\"app\">\n  \n</div><p>after</p>"),
        (Some(MountPoint::App), true)
    );
    assert_eq!(
        mount(r#"<div id="__nuxt"><!-- spa --></div>"#),
        (Some(MountPoint::Nuxt), true)
    );
    assert_eq!(
        mount(r#"<div id="___gatsby"><div id="gatsby-focus-wrapper"></div></div>"#),
        (Some(MountPoint::Gatsby), false),
        "a child element fills it"
    );
    assert_eq!(
        mount(r#"<div id="q-app">Loading</div>"#),
        (Some(MountPoint::QApp), false)
    );
    assert_eq!(
        mount(r#"<div id="svelte"><img src="/hero.jpg"></div>"#),
        (Some(MountPoint::Svelte), false)
    );
    // The first mount point is the one reported.
    assert_eq!(
        mount(r#"<div id="root"><div id="app"></div></div>"#),
        (Some(MountPoint::Root), false)
    );
    assert_eq!(mount(r#"<div id="main"></div>"#), (None, false));
}

#[test]
fn void_and_svg_elements_are_not_mount_points() {
    let mount = |html: &str| {
        let r = extract_html(html).fields.raw;
        (r.mount, r.mount_empty)
    };
    assert_eq!(
        mount(r#"<link id="app" rel="stylesheet" href="/a.css"><p>Hello</p>"#),
        (None, false)
    );
    assert_eq!(
        mount(r#"<img id="root" src="/logo.png"><p>Hello</p>"#),
        (None, false)
    );
    assert_eq!(
        mount(r#"<svg><symbol id="app" viewBox="0 0 1 1"/></svg><p>Hello</p>"#),
        (None, false)
    );
    assert_eq!(
        mount(r#"<svg><g id="root"></g></svg><p>Hello</p>"#),
        (None, false),
        "SVG content is not an app container"
    );
    // A skipped match does not hide the real mount point after it.
    assert_eq!(
        mount(r#"<link id="app" rel="stylesheet" href="/a.css"><div id="root"></div>"#),
        (Some(MountPoint::Root), true)
    );
    // Containers other than div that apps mount on still count.
    assert_eq!(
        mount(r#"<main id="app"></main>"#),
        (Some(MountPoint::App), true)
    );
}

#[test]
fn inline_script_bytes_leave_out_json_ld_and_external_scripts_count_module_preloads() {
    let raw = extract_html(
        r#"<link rel="modulepreload" href="/c.js"><link rel="preload" as="script" href="/d.js"><script>var a=1;</script><script src="/a.js"></script><script type="module" src="/b.js"></script><script type="application/ld+json">{"@type":"Thing"}</script><script type="application/json">{"x":1}</script>"#,
    )
    .fields
    .raw;
    assert_eq!(raw.script_bytes, "var a=1;".len() as u32 + 7);
    // Two `<script src>` and one modulepreload; a plain preload is only a hint.
    assert_eq!(raw.scripts_external, 3);
}

#[test]
fn main_words_count_main_article_and_role_main_once() {
    let words = |html: &str| extract_html(html).fields.raw.main_words;
    assert_eq!(
        words("<nav>a b</nav><main>c d <article>e f</article></main>"),
        4
    );
    assert_eq!(
        words(r#"<div role="main">one two three</div><p>four</p>"#),
        3
    );
    assert_eq!(words("<article>x<script>y z</script></article>"), 1);
    assert_eq!(words("<p>no landmark at all</p>"), 0);
}
