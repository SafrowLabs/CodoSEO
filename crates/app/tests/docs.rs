//! The docs' image links point at files that exist, and the AI access guide shows the synthetic
//! fetch check where it describes it.

use std::path::{Path, PathBuf};

fn docs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs")
}

/// Every `images/...` path a document under `docs/` links to.
fn image_links(text: &str) -> Vec<String> {
    text.match_indices("images/")
        .filter(|(at, _)| {
            // A link target starts the path: `(images/`, `"images/`.
            at.checked_sub(1)
                .and_then(|i| text.as_bytes().get(i))
                .is_some_and(|b| matches!(b, b'(' | b'"'))
        })
        .map(|(at, _)| {
            text[at..]
                .chars()
                .take_while(|c| !matches!(c, ')' | '"' | ' ' | '\n'))
                .collect()
        })
        .collect()
}

#[test]
fn every_image_the_docs_link_to_exists() {
    let mut checked = 0;
    for entry in std::fs::read_dir(docs()).expect("docs/") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a doc");
        for link in image_links(&text) {
            assert!(
                docs().join(&link).is_file(),
                "{} links to {link}, which does not exist",
                path.display()
            );
            checked += 1;
        }
    }
    assert!(checked > 0);
}

#[test]
fn the_fetch_check_section_shows_its_screenshots() {
    let text = std::fs::read_to_string(docs().join("ai-access.md")).expect("ai-access.md");
    let start = text
        .find("## Synthetic fetch checks")
        .expect("the fetch check section");
    let end = text[start + 3..]
        .find("\n## ")
        .map_or(text.len(), |i| start + 3 + i);
    let links = image_links(&text[start..end]);
    assert!(
        links
            .iter()
            .any(|l| l.starts_with("images/ai-access/") && l.contains("probes")),
        "{links:?}"
    );
}
