//! robots.txt comparison: rules matter, comments and `Sitemap:` lines don't.

use codoseo_core::change::{Change, ChangeKind};
use codoseo_core::check::Severity;
use codoseo_core::crawl::RobotsFile;
use xxhash_rust::xxh3::xxh3_64;

/// Hash of the rules in a robots.txt body. Comments, blank lines and `Sitemap:` lines are
/// dropped and field names are lower-cased, so edits that don't change what bots may do
/// don't change the fingerprint. Values keep their case, since paths are case-sensitive.
pub fn robots_fingerprint(body: &str) -> u64 {
    let mut rules = String::new();
    for line in body.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field.trim().to_ascii_lowercase(), value.trim()),
            None => (line.to_ascii_lowercase(), ""),
        };
        if field == "sitemap" {
            continue;
        }
        if !rules.is_empty() {
            rules.push('\n');
        }
        rules.push_str(&field);
        rules.push(':');
        rules.push_str(value);
    }
    xxh3_64(rules.as_bytes())
}

/// A change when the robots.txt status or its rules differ. A missing file on both sides is
/// no change; missing on one side shows as `none`.
pub(crate) fn robots_change(
    prev: Option<&RobotsFile>,
    curr: Option<&RobotsFile>,
) -> Option<Change> {
    let (before, after) = match (prev, curr) {
        (None, None) => return None,
        (Some(p), Some(c)) => {
            let rules_differ = robots_fingerprint(&p.body) != robots_fingerprint(&c.body);
            if p.status == c.status && !rules_differ {
                return None;
            }
            if p.status == c.status {
                (
                    p.status.to_string(),
                    format!("{} (rules changed)", c.status),
                )
            } else {
                (p.status.to_string(), c.status.to_string())
            }
        }
        (Some(p), None) => (p.status.to_string(), "none".to_owned()),
        (None, Some(c)) => ("none".to_owned(), c.status.to_string()),
    };
    Some(Change {
        kind: ChangeKind::RobotsTxtChanged,
        severity: Severity::Warning,
        url: None,
        before,
        after,
    })
}
