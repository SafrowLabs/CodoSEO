//! Change detection between two crawls of the same site.
//!
//! Pages are matched by the hash of their URL, recomputed here rather than trusted from
//! the stored `url_hash`, and only pages on the snapshot's origin count. When the
//! origin itself changed, the earlier URLs are moved onto the new origin first, so a move
//! shows up as one `SiteMoved` and not as every page removed and added.

mod kinds;
mod robots;

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use codoseo_core::change::{Change, ChangeKind};
use codoseo_core::check::Severity;
use codoseo_core::output::StopReason;
use codoseo_core::page::PageRecord;
use codoseo_core::snapshot::Snapshot;
use codoseo_core::url::url_hash;
use url::Url;

use kinds::{PrevPage, chain_grew, is_error, ordinal, page_changes, raise, sitemap_shrank};
pub use robots::robots_fingerprint;

/// Pages from the top of the inlink ranking that count as key pages.
const TOP_PAGES: usize = 20;

/// Scheme, host and port of a URL: what makes two URLs the same site origin.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OriginKey {
    scheme: String,
    host: String,
    port: Option<u16>,
}

impl OriginKey {
    fn of(url: &Url) -> OriginKey {
        OriginKey {
            scheme: url.scheme().to_owned(),
            host: url.host_str().unwrap_or("").to_owned(),
            port: url.port_or_known_default(),
        }
    }

    fn contains(&self, url: &Url) -> bool {
        url.scheme() == self.scheme
            && url.host_str() == Some(self.host.as_str())
            && url.port_or_known_default() == self.port
    }
}

/// `url` with its path and query on `to`'s origin, if it is on `from`'s origin.
fn rebase(url: &Url, from: &OriginKey, to: &Url) -> Url {
    if !from.contains(url) {
        return url.clone();
    }
    let mut moved = to.clone();
    moved.set_path(url.path());
    moved.set_query(url.query());
    moved.set_fragment(None);
    moved
}

/// Compares two crawls and returns the changes, sorted by severity, then kind, then URL.
/// Changes to pages in `key_pages` (and to the origin page) are one severity step higher.
pub fn diff(prev: &Snapshot, curr: &Snapshot, key_pages: &HashSet<u64>) -> Vec<Change> {
    let prev_origin = OriginKey::of(&prev.origin);
    let curr_origin = OriginKey::of(&curr.origin);
    let moved = prev_origin != curr_origin;
    let origin_hash = url_hash(&curr.origin);
    let is_key = |hash: u64| hash == origin_hash || key_pages.contains(&hash);

    let mut changes = Vec::new();
    if moved {
        changes.push(Change {
            kind: ChangeKind::SiteMoved,
            severity: Severity::Critical,
            url: None,
            before: prev.origin.as_str().to_owned(),
            after: curr.origin.as_str().to_owned(),
        });
    }

    let prev_pages: HashMap<u64, PrevPage<'_>> = prev
        .pages
        .iter()
        .filter(|p| prev_origin.contains(&p.url))
        .map(|rec| {
            let (url, canonical) = if moved {
                (
                    rebase(&rec.url, &prev_origin, &curr.origin),
                    rec.fields
                        .canonical
                        .as_ref()
                        .map(|c| rebase(c, &prev_origin, &curr.origin)),
                )
            } else {
                (rec.url.clone(), rec.fields.canonical.clone())
            };
            let hash = url_hash(&url);
            (
                hash,
                PrevPage {
                    rec,
                    url,
                    canonical,
                },
            )
        })
        .collect();
    let curr_pages: HashMap<u64, &PageRecord> = curr
        .pages
        .iter()
        .filter(|p| curr_origin.contains(&p.url))
        .map(|p| (url_hash(&p.url), p))
        .collect();

    let mut page_change = |hash: u64, kind, severity, url: &Url, before, after| {
        changes.push(Change {
            kind,
            severity: if is_key(hash) {
                raise(severity)
            } else {
                severity
            },
            url: Some(url.clone()),
            before,
            after,
        });
    };

    let mut newly_failing = 0usize;
    for (hash, page) in &curr_pages {
        match prev_pages.get(hash) {
            Some(old) => {
                // The chain length is not in the key hash, so it is always checked.
                if let Some((kind, severity, before, after)) = chain_grew(old.rec, page) {
                    page_change(*hash, kind, severity, &page.url, before, after);
                }
                // Across a move the key hash can't be trusted to be comparable, so compare fields.
                if !moved && old.rec.key_hash == page.key_hash {
                    continue;
                }
                if is_error(page) && !is_error(old.rec) {
                    newly_failing += 1;
                }
                for (kind, severity, before, after) in page_changes(old, page) {
                    page_change(*hash, kind, severity, &page.url, before, after);
                }
            }
            None if prev.stop.is_complete() => {
                if is_error(page) {
                    newly_failing += 1;
                }
                page_change(
                    *hash,
                    ChangeKind::NewUrl,
                    Severity::Notice,
                    &page.url,
                    String::new(),
                    page.status.to_string(),
                );
            }
            None => {}
        }
    }
    if curr.stop.is_complete() {
        for (hash, old) in &prev_pages {
            if !curr_pages.contains_key(hash) {
                page_change(
                    *hash,
                    ChangeKind::RemovedUrl,
                    Severity::Notice,
                    &old.url,
                    old.rec.status.to_string(),
                    String::new(),
                );
            }
        }
    }

    if newly_failing >= error_threshold(curr_pages.len()) {
        changes.push(Change {
            kind: ChangeKind::ErrorSpike,
            severity: Severity::Critical,
            url: None,
            before: prev_pages
                .values()
                .filter(|p| is_error(p.rec))
                .count()
                .to_string(),
            after: curr_pages
                .values()
                .filter(|p| is_error(p))
                .count()
                .to_string(),
        });
    }

    let newly_blocked =
        curr.stop == StopReason::RobotsBlocked && prev.stop != StopReason::RobotsBlocked;
    changes.extend(robots::robots_change(
        prev.robots.as_ref(),
        curr.robots.as_ref(),
        newly_blocked,
    ));

    if sitemap_shrank(&prev.sitemap, &curr.sitemap) {
        changes.push(Change {
            kind: ChangeKind::SitemapShrank,
            severity: Severity::Warning,
            url: None,
            before: prev.sitemap.url_count.to_string(),
            after: curr.sitemap.url_count.to_string(),
        });
    }

    changes.sort_by_cached_key(|c| {
        (
            c.severity,
            ordinal(c.kind),
            c.url.as_ref().map(|u| u.as_str().to_owned()),
        )
    });
    changes
}

/// At least 3 pages, or 2% of the pages on the origin, whichever is more.
fn error_threshold(pages: usize) -> usize {
    3.max((pages * 2).div_ceil(100))
}

/// The pages whose changes matter more: the origin page, the 20 pages with the most
/// inlinks (ties broken by URL) and the starred ones.
pub fn key_pages(snap: &Snapshot, starred: &HashSet<u64>) -> HashSet<u64> {
    let origin = OriginKey::of(&snap.origin);
    let mut ranked: Vec<&PageRecord> = snap
        .pages
        .iter()
        .filter(|p| origin.contains(&p.url))
        .collect();
    ranked.sort_by(|a, b| {
        (Reverse(a.inlinks), a.url.as_str()).cmp(&(Reverse(b.inlinks), b.url.as_str()))
    });

    let mut keys = starred.clone();
    keys.insert(url_hash(&snap.origin));
    keys.extend(ranked.iter().take(TOP_PAGES).map(|p| url_hash(&p.url)));
    keys
}
