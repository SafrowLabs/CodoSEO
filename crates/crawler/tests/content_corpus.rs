//! The raw-content corpus, B2's exit criterion: pages shaped like the HTML real frameworks and
//! site builders serve, run through the extractor and `codoseo_geo::content::classify`, each
//! scored as a true or false positive or negative. A positive is a page classified `Shell` or
//! `LikelyShell`; `Thin` and `Present` are negatives. The test prints a table and the totals and
//! fails on any false positive or false negative.
//!
//! Run it with `cargo test -p codoseo-crawler --test content_corpus -- --nocapture` to see the
//! table.
//!
//! Flutter web is the one known false negative: its default page is a single
//! `flutter_bootstrap.js` script and nothing else, the same raw signals as an image-only landing
//! page with one analytics tag, so flagging it would flag those too.
//!
//! The fixtures copy the markup each tool emits by default (mount elements, noscript notices,
//! script tags, data blobs), so a fixture passes because the classifier reads real output, not
//! because the fixture repeats the classifier's thresholds.

use std::collections::HashSet;

use codoseo_core::check::IssueBits;
use codoseo_core::page::{Indexability, PageRecord};
use codoseo_core::url::url_hash;
use codoseo_crawler::extract::extract;
use codoseo_geo::content::{classify, script_share, shell_signals};
use url::Url;

/// What a fixture is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// A client-side app shell: its content only appears once JavaScript runs.
    Shell,
    /// Its content is in the HTML as served (or it really is that short).
    Normal,
    /// A shell the raw-HTML signals can't tell from a normal page without false positives.
    /// Listed, not scored.
    Undetectable(&'static str),
}

const FIXTURES: &[(&str, &str, Expect)] = &[
    // Shells.
    ("shell_cra.html", "Create React App", Expect::Shell),
    ("shell_vite_react.html", "Vite + React", Expect::Shell),
    ("shell_vue_cli.html", "Vue CLI", Expect::Shell),
    ("shell_angular.html", "Angular CLI", Expect::Shell),
    (
        "shell_next_client.html",
        "Next.js client-only page",
        Expect::Shell,
    ),
    (
        "shell_next_app_bailout.html",
        "Next.js app router, client bailout",
        Expect::Shell,
    ),
    (
        "shell_nuxt_spa.html",
        "Nuxt SPA (ssr: false)",
        Expect::Shell,
    ),
    (
        "shell_gatsby.html",
        "Gatsby client-only route",
        Expect::Shell,
    ),
    ("shell_ember.html", "Ember", Expect::Shell),
    (
        "shell_flutter.html",
        "Flutter web",
        Expect::Undetectable("one bootstrap script, like an image page with one tag"),
    ),
    ("shell_sveltekit_csr.html", "SvelteKit CSR", Expect::Shell),
    // Normal pages.
    ("normal_wordpress.html", "WordPress post", Expect::Normal),
    ("normal_next_ssr.html", "Next.js SSR", Expect::Normal),
    ("normal_nuxt_ssr.html", "Nuxt SSR", Expect::Normal),
    ("normal_astro.html", "Astro", Expect::Normal),
    ("normal_hugo.html", "Hugo", Expect::Normal),
    ("normal_docusaurus.html", "Docusaurus", Expect::Normal),
    ("normal_shopify.html", "Shopify product", Expect::Normal),
    ("normal_amp.html", "AMP article", Expect::Normal),
    (
        "normal_short_contact.html",
        "Short contact page",
        Expect::Normal,
    ),
    (
        "normal_coming_soon.html",
        "Coming-soon page",
        Expect::Normal,
    ),
    (
        "normal_gtm_noscript.html",
        "GTM noscript page",
        Expect::Normal,
    ),
    (
        "normal_image_landing.html",
        "Image-only landing",
        Expect::Normal,
    ),
    (
        "normal_short_post_disqus.html",
        "Short post with Disqus",
        Expect::Normal,
    ),
    (
        "normal_short_h2_disqus.html",
        "Short h2 note with Disqus",
        Expect::Normal,
    ),
    (
        "normal_void_mount_ids.html",
        "Mount ids on a link and an img",
        Expect::Normal,
    ),
    (
        "normal_svg_sprite.html",
        "Mount ids on SVG symbols",
        Expect::Normal,
    ),
];

/// The page record the crawler would build for `body` served at `url` (see `record.rs`).
fn record(url: &Url, body: &[u8]) -> PageRecord {
    let extracted = extract(url, Some("text/html; charset=utf-8"), body);
    let internal: HashSet<u64> = extracted
        .links
        .iter()
        .filter(|l| l.url.host_str() == url.host_str())
        .map(|l| url_hash(&l.url))
        .collect();
    let external: HashSet<u64> = extracted
        .links
        .iter()
        .filter(|l| l.url.host_str() != url.host_str())
        .map(|l| url_hash(&l.url))
        .collect();
    PageRecord {
        url_hash: url_hash(url),
        url: url.clone(),
        status: 200,
        redirect_chain: Vec::new(),
        response_ms: 50,
        size_bytes: body.len() as u64,
        content_type: Some("text/html; charset=utf-8".to_owned()),
        depth: Some(0),
        in_sitemap: true,
        indexability: Indexability::Indexable,
        fields: extracted.fields,
        inlinks: 1,
        outlinks_internal: internal.len() as u32,
        outlinks_external: external.len() as u32,
        issues: IssueBits::default(),
        key_hash: 0,
        redirect_target: None,
        outlinks_nofollow: 0,
        error: None,
    }
}

#[test]
fn the_content_corpus_has_no_false_positives_or_negatives() {
    let dir = format!("{}/tests/fixtures/content", env!("CARGO_MANIFEST_DIR"));
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .expect("fixtures")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    let mut listed: Vec<String> = FIXTURES.iter().map(|(f, ..)| (*f).to_owned()).collect();
    listed.sort();
    assert_eq!(on_disk, listed, "every fixture on disk is scored");

    let (mut tp, mut tn, mut fp, mut fn_) = (0, 0, 0, 0);
    let mut known = 0;
    let mut wrong = Vec::new();
    println!(
        "{:36} {:12} {:>5} {:>4} {:>6} {:>4} {:3} {:28} result",
        "fixture", "verdict", "words", "h1", "links", "js%", "ext", "signals"
    );
    for (file, what, expect) in FIXTURES {
        let body = std::fs::read(format!("{dir}/{file}")).expect("fixture");
        // Served at its canonical URL, so its absolute links to its own host are internal.
        let fallback = Url::parse(&format!("https://corpus.test/{file}")).expect("url");
        let url = extract(&fallback, Some("text/html"), &body)
            .fields
            .canonical
            .unwrap_or(fallback);
        let page = record(&url, &body);
        let verdict = classify(&page);
        let positive = verdict.is_shell();
        let result = match (expect, positive) {
            (Expect::Shell, true) => {
                tp += 1;
                "TP".to_owned()
            }
            (Expect::Normal, false) => {
                tn += 1;
                "TN".to_owned()
            }
            (Expect::Normal, true) => {
                fp += 1;
                wrong.push(format!("{file} ({what}): false positive, {verdict:?}"));
                "FP".to_owned()
            }
            (Expect::Shell, false) => {
                fn_ += 1;
                wrong.push(format!("{file} ({what}): false negative, {verdict:?}"));
                "FN".to_owned()
            }
            (Expect::Undetectable(why), detected) => {
                known += 1;
                if detected {
                    wrong.push(format!(
                        "{file} ({what}) is detected now: score it as a shell"
                    ));
                }
                format!("known FN: {why}")
            }
        };
        let signals: Vec<String> = shell_signals(&page)
            .iter()
            .map(|s| format!("{s:?}"))
            .collect();
        let share = script_share(&page);
        println!(
            "{:36} {:12} {:>5} {:>4} {:>6} {:>4} {:3} {:28} {result}",
            what,
            format!("{verdict:?}"),
            page.fields.word_count,
            page.fields.h1.len(),
            page.outlinks_internal,
            share,
            page.fields.raw.scripts_external,
            signals.join(","),
        );
    }
    println!("\nTP {tp}  TN {tn}  FP {fp}  FN {fn_}  known FN {known}");
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert_eq!((fp, fn_), (0, 0));
}
