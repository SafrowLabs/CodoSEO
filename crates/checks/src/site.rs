//! Checks that need the whole crawl: duplicates, links to bad pages, orphans, canonical and
//! sitemap problems, and the two site-wide facts.

use std::collections::HashMap;
use std::hash::Hash;

use codoseo_core::check::{CheckId, IssueBits};
use codoseo_core::output::{CrawlOutput, StopReason};
use codoseo_core::page::{Indexability, PageRecord};
use url::Url;

use crate::{is_broken, is_redirect};

/// The Site-scope bits for every page, in page order. Needs `inlinks` to be set.
pub(crate) fn site_issues(out: &CrawlOutput) -> Vec<IssueBits> {
    let mut bits = vec![IssueBits::default(); out.pages.len()];
    let pages = &out.pages;

    duplicates(pages, &mut bits, CheckId::TitleDuplicate, |p| {
        normalised(p.fields.title.as_deref())
    });
    duplicates(pages, &mut bits, CheckId::DescriptionDuplicate, |p| {
        normalised(p.fields.meta_description.as_deref())
    });
    duplicates(pages, &mut bits, CheckId::H1Duplicate, |p| {
        normalised(p.fields.h1.first().map(String::as_str))
    });
    duplicates(pages, &mut bits, CheckId::ContentDuplicate, |p| {
        Some(p.fields.content_hash)
    });

    for e in &out.links.edges {
        if e.from == e.to {
            continue;
        }
        let target = &pages[e.to as usize];
        if is_broken(target) {
            bits[e.from as usize].set_check(CheckId::LinksToBroken);
        } else if is_redirect(target) {
            bits[e.from as usize].set_check(CheckId::LinksToRedirect);
        }
    }

    let by_url: HashMap<&Url, &PageRecord> = pages.iter().map(|p| (&p.url, p)).collect();
    let sitemap_has_urls = out.sitemap.url_count > 0;

    for (i, p) in pages.iter().enumerate() {
        let b = &mut bits[i];
        // Listed in the sitemap but never reached by links: a page with a depth was found
        // by following links (the origin, or a redirect target), so it isn't an orphan.
        if p.in_sitemap && p.depth.is_none() && p.inlinks == 0 {
            b.set_check(CheckId::Orphan);
        }
        if let Some(target) = p.fields.canonical.as_ref().filter(|c| **c != p.url)
            && by_url
                .get(target)
                .is_some_and(|t| !(200..300).contains(&t.status))
        {
            b.set_check(CheckId::CanonicalToNon200);
        }
        if p.in_sitemap {
            if !(200..300).contains(&p.status) {
                b.set_check(CheckId::SitemapNon200);
            }
            match p.indexability {
                Indexability::Noindex => b.set_check(CheckId::SitemapNoindex),
                Indexability::Canonicalised => b.set_check(CheckId::SitemapCanonicalised),
                _ => {}
            }
        } else if sitemap_has_urls && p.indexability == Indexability::Indexable {
            b.set_check(CheckId::NotInSitemap);
        }
    }
    bits
}

/// The site-wide checks that fire; each counts as 1 and sets no page bit.
pub(crate) fn site_wide_issues(out: &CrawlOutput) -> Vec<CheckId> {
    let mut fired = Vec::new();
    if out.stop == StopReason::RobotsBlocked {
        fired.push(CheckId::RobotsBlocksSite);
    }
    if out.sitemap.url_count == 0 {
        fired.push(CheckId::SitemapMissing);
    }
    fired
}

/// Trimmed and lower-cased; `None` when nothing is left.
fn normalised(text: Option<&str>) -> Option<String> {
    let t = text?.trim();
    (!t.is_empty()).then(|| t.to_lowercase())
}

/// Sets `id` on every page whose key is shared with another page. Only indexable HTML
/// pages take part; pages with no key (`None`) are left out.
fn duplicates<K: Hash + Eq>(
    pages: &[PageRecord],
    bits: &mut [IssueBits],
    id: CheckId,
    key: impl Fn(&PageRecord) -> Option<K>,
) {
    let eligible = |p: &PageRecord| p.is_html_ok() && p.indexability == Indexability::Indexable;
    let mut counts: HashMap<K, u32> = HashMap::new();
    for p in pages.iter().filter(|p| eligible(p)) {
        if let Some(k) = key(p) {
            *counts.entry(k).or_default() += 1;
        }
    }
    for (i, p) in pages.iter().enumerate() {
        if eligible(p) && key(p).is_some_and(|k| counts[&k] > 1) {
            bits[i].set_check(id);
        }
    }
}
