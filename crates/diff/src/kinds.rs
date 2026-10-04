//! The individual comparisons: one page against its earlier self, sitemaps, severities.

use codoseo_core::change::ChangeKind;
use codoseo_core::check::Severity;
use codoseo_core::crawl::SitemapSummary;
use codoseo_core::page::{Indexability, PageRecord};
use url::Url;

/// Kind, base severity, before and after.
pub(crate) type PageChange = (ChangeKind, Severity, String, String);

/// A page from the earlier crawl, with its URLs moved onto the current origin if the site moved.
pub(crate) struct PrevPage<'a> {
    pub rec: &'a PageRecord,
    pub url: Url,
    pub canonical: Option<Url>,
}

/// No response (with a recorded failure), 4xx or 5xx. Status 0 without a failure is a
/// robots-blocked record, which is not an error.
pub(crate) fn is_error(p: &PageRecord) -> bool {
    match p.status {
        0 => p.error.is_some(),
        400.. => true,
        _ => false,
    }
}

/// Key pages raise severity one step: Notice to Warning, Warning to Critical.
pub(crate) fn raise(s: Severity) -> Severity {
    match s {
        Severity::Notice => Severity::Warning,
        Severity::Warning | Severity::Critical => Severity::Critical,
    }
}

/// Position of a kind in declaration order, for sorting.
pub(crate) fn ordinal(kind: ChangeKind) -> u8 {
    match kind {
        ChangeKind::NewUrl => 0,
        ChangeKind::RemovedUrl => 1,
        ChangeKind::StatusChanged => 2,
        ChangeKind::BecameNoindex => 3,
        ChangeKind::TitleChanged => 4,
        ChangeKind::TitleRemoved => 5,
        ChangeKind::CanonicalChanged => 6,
        ChangeKind::RedirectChainGrew => 7,
        ChangeKind::RobotsTxtChanged => 8,
        ChangeKind::SitemapShrank => 9,
        ChangeKind::ErrorSpike => 10,
        ChangeKind::SiteMoved => 11,
    }
}

/// A trimmed title, or `None` when it is missing or blank.
fn title(p: &PageRecord) -> Option<&str> {
    p.fields
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

fn url_or_none(u: Option<&Url>) -> String {
    u.map_or_else(|| "none".to_owned(), |u| u.as_str().to_owned())
}

/// The redirect chain got longer. Checked on every matched pair, because the key hash
/// doesn't include the chain length.
pub(crate) fn chain_grew(prev: &PageRecord, curr: &PageRecord) -> Option<PageChange> {
    (curr.redirect_chain.len() > prev.redirect_chain.len()).then(|| {
        (
            ChangeKind::RedirectChainGrew,
            Severity::Notice,
            prev.redirect_chain.len().to_string(),
            curr.redirect_chain.len().to_string(),
        )
    })
}

/// What changed on one page: kind, base severity, before and after.
pub(crate) fn page_changes(prev: &PrevPage<'_>, curr: &PageRecord) -> Vec<PageChange> {
    let mut out = Vec::new();
    let old = prev.rec;

    if old.status != curr.status {
        let severity = if is_error(curr) && !is_error(old) {
            Severity::Warning
        } else {
            Severity::Notice
        };
        out.push((
            ChangeKind::StatusChanged,
            severity,
            old.status.to_string(),
            curr.status.to_string(),
        ));
    }
    if curr.indexability == Indexability::Noindex && old.indexability != Indexability::Noindex {
        out.push((
            ChangeKind::BecameNoindex,
            Severity::Warning,
            format!("{:?}", old.indexability),
            format!("{:?}", curr.indexability),
        ));
    }
    match (title(old), title(curr)) {
        (Some(a), None) => out.push((
            ChangeKind::TitleRemoved,
            Severity::Warning,
            a.to_owned(),
            String::new(),
        )),
        (Some(a), Some(b)) if a != b => out.push((
            ChangeKind::TitleChanged,
            Severity::Notice,
            a.to_owned(),
            b.to_owned(),
        )),
        _ => {}
    }
    if prev.canonical != curr.fields.canonical {
        out.push((
            ChangeKind::CanonicalChanged,
            Severity::Notice,
            url_or_none(prev.canonical.as_ref()),
            url_or_none(curr.fields.canonical.as_ref()),
        ));
    }
    out
}

/// The sitemap lost 10% or more of its URLs. Skipped when either summary may be partial.
pub(crate) fn sitemap_shrank(prev: &SitemapSummary, curr: &SitemapSummary) -> bool {
    if prev.failed_files > 0 || curr.failed_files > 0 || !prev.complete || !curr.complete {
        return false;
    }
    prev.url_count > 0 && u64::from(curr.url_count) * 10 <= u64::from(prev.url_count) * 9
}
