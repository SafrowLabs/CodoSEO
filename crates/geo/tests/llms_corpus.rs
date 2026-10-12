//! Real-shaped llms.txt files (tests/fixtures/llms) and the checks each one fails.
//!
//! The shapes come from files sites serve: llmstxt.org's own, a docs site with an `## Optional`
//! section, an API docs index with H3 subsections and `- [name](url) - notes` items
//! (Anthropic-style), a platform index with plain-list "how to use" sections (Vercel-style), a
//! framework's index of per-package files (Svelte-style), a docs index with prose and no summary
//! (Stripe-style, served as text/markdown), and the broken ones: no H1, a late H1, plain URLs
//! instead of Markdown links, prose-only sections, an HTML 404 page served with 200, an empty
//! file, an H1 alone, a BOM, CRLF line ends, relative links and a code fence holding an example.

use std::collections::BTreeSet;

use codoseo_core::output::LlmsTxtFetch;
use codoseo_geo::llms::{LlmsCheckId, LlmsState, MAX_BYTES, NOTICE_BYTES, assess, links, parse};

use LlmsCheckId::*;

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/llms/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn fetch(content_type: &str, body: String) -> LlmsTxtFetch {
    LlmsTxtFetch {
        status: 200,
        content_type: Some(content_type.to_owned()),
        bytes: body.len() as u64,
        body,
        ..LlmsTxtFetch::default()
    }
}

const PLAIN: &str = "text/plain; charset=utf-8";

#[test]
fn every_fixture_gets_the_expected_state_and_failing_checks() {
    let table: &[(&str, &str, LlmsState, &[LlmsCheckId])] = &[
        ("llmstxt_org.txt", PLAIN, LlmsState::Available, &[]),
        ("docs_site.txt", PLAIN, LlmsState::Available, &[]),
        (
            "anthropic_style.txt",
            PLAIN,
            LlmsState::Available,
            &[Summary],
        ),
        ("vercel_style.txt", PLAIN, LlmsState::Available, &[]),
        ("svelte_style.txt", PLAIN, LlmsState::Available, &[]),
        (
            "stripe_style.txt",
            "text/markdown; charset=utf-8",
            LlmsState::Available,
            &[Summary],
        ),
        ("no_h1.txt", PLAIN, LlmsState::Available, &[H1, Summary]),
        ("late_h1.txt", PLAIN, LlmsState::Available, &[H1]),
        (
            "plain_urls.txt",
            PLAIN,
            LlmsState::Available,
            &[Sections, Links],
        ),
        (
            "non_list_sections.txt",
            PLAIN,
            LlmsState::Available,
            &[Sections],
        ),
        ("relative_links.txt", PLAIN, LlmsState::Available, &[]),
        ("bom.txt", PLAIN, LlmsState::Available, &[]),
        ("crlf.txt", PLAIN, LlmsState::Available, &[]),
        ("code_fence.txt", PLAIN, LlmsState::Available, &[]),
        (
            "h1_only.txt",
            PLAIN,
            LlmsState::Available,
            &[Summary, Sections, Links, Length],
        ),
        (
            "empty.txt",
            PLAIN,
            LlmsState::Available,
            &[H1, Summary, Sections, Links, Length],
        ),
        // A real file mis-served as HTML stays a file.
        (
            "llmstxt_org.txt",
            "text/html; charset=utf-8",
            LlmsState::Available,
            &[Mime],
        ),
        // The site's catch-all HTML page, whatever it claims to be.
        (
            "html_soft404.html",
            "text/html; charset=utf-8",
            LlmsState::SoftNotFound,
            &[],
        ),
        ("html_soft404.html", PLAIN, LlmsState::SoftNotFound, &[]),
    ];
    let mut failures = Vec::new();
    for &(name, ct, state, failing) in table {
        let r = assess(&fetch(ct, fixture(name)), false);
        let got: BTreeSet<LlmsCheckId> = r.checks.iter().filter(|c| !c.ok).map(|c| c.id).collect();
        let want: BTreeSet<LlmsCheckId> = failing.iter().copied().collect();
        if r.state != state || got != want {
            failures.push(format!(
                "{name} ({ct}): state {:?}, failing {got:?}; want {state:?}, {want:?}\n  {:#?}",
                r.state, r.checks
            ));
        }
        if state == LlmsState::Available {
            assert_eq!(r.checks.len(), 8, "{name}: every check runs on a file");
            assert!(r.adopted, "{name}");
        } else {
            assert!(r.checks.is_empty() && !r.adopted, "{name}");
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn oversized_and_truncated_files_fail_only_the_size_check() {
    let mut body = fixture("docs_site.txt");
    while (body.len() as u64) <= NOTICE_BYTES {
        body.push_str("- [More](https://docs.acme.test/more.md): Another page of the docs\n");
    }
    let r = assess(&fetch(PLAIN, body.clone()), false);
    let failing: Vec<LlmsCheckId> = r.checks.iter().filter(|c| !c.ok).map(|c| c.id).collect();
    assert_eq!(failing, [Size]);

    let mut f = fetch(PLAIN, body);
    f.bytes = 2 * MAX_BYTES as u64;
    f.truncated = true;
    let r = assess(&f, false);
    let size = r.check(Size).unwrap();
    assert!(
        !size.ok && size.detail.contains("only the first 512 KiB"),
        "{size:?}"
    );
}

#[test]
fn the_corpus_links_come_out_as_written() {
    assert_eq!(
        links(&parse(&fixture("relative_links.txt"))),
        ["/docs/install.md", "docs/configure.md", "./docs/deploy.md"]
    );
    // The H3 subsections of an Anthropic-style index stay in their H2 section.
    let doc = parse(&fixture("anthropic_style.txt"));
    let english = doc.sections.iter().find(|s| s.title == "English").unwrap();
    assert_eq!(english.links.len(), 6);
    assert_eq!(links(&doc).len(), 6, "a bare URL in prose is not a link");
    // Nothing in the code fence counts.
    let doc = parse(&fixture("code_fence.txt"));
    assert_eq!(doc.title.as_deref(), Some("Fenced Example"));
    assert_eq!(links(&doc), ["https://fence.test/notes.md"]);
    assert_eq!(doc.sections.len(), 2);
    // A BOM and CRLF line ends change nothing.
    let bom = parse(&fixture("bom.txt"));
    assert_eq!(bom.title.as_deref(), Some("BOM Site"));
    assert!(bom.h1_first);
    let crlf = parse(&fixture("crlf.txt"));
    assert_eq!(
        crlf.summary.as_deref(),
        Some("A file saved with Windows line endings.")
    );
    assert_eq!(
        links(&crlf),
        ["https://crlf.test/start.md", "https://crlf.test/api.md"]
    );
}
