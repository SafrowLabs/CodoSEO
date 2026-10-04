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
///
/// `newly_blocked` (this crawl stopped as `RobotsBlocked` and the previous one didn't)
/// always gives one critical change, even when the file reads the same.
pub(crate) fn robots_change(
    prev: Option<&RobotsFile>,
    curr: Option<&RobotsFile>,
    newly_blocked: bool,
) -> Option<Change> {
    let status =
        |f: Option<&RobotsFile>| f.map_or_else(|| "none".to_owned(), |f| f.status.to_string());
    let (before, after, rules_changed) = match (prev, curr) {
        (None, None) => (status(prev), status(curr), false),
        (Some(p), Some(c)) => {
            let rules_differ = robots_fingerprint(&p.body) != robots_fingerprint(&c.body);
            (
                status(prev),
                status(curr),
                p.status == c.status && rules_differ,
            )
        }
        _ => (status(prev), status(curr), false),
    };
    let differs = before != after || rules_changed;
    if !differs && !newly_blocked {
        return None;
    }
    let notes: Vec<&str> = [
        rules_changed.then_some("rules changed"),
        newly_blocked.then_some("blocks the crawl"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let after = if notes.is_empty() {
        after
    } else {
        format!("{after} ({})", notes.join(", "))
    };
    Some(Change {
        kind: ChangeKind::RobotsTxtChanged,
        severity: if newly_blocked {
            Severity::Critical
        } else {
            Severity::Warning
        },
        url: None,
        before,
        after,
    })
}
