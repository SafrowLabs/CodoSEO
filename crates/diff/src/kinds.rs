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
        ChangeKind::AiBotBlocked => 12,
        ChangeKind::AiAnswersRestricted => 13,
        ChangeKind::AiBlockNotApplied => 14,
        ChangeKind::AiIssueResolved => 15,
        ChangeKind::AiPreferencesChanged => 16,
        ChangeKind::AiFetchBlocked => 17,
        ChangeKind::AiContentMissing => 18,
        ChangeKind::AiLlmsTxtUnavailable => 19,
        ChangeKind::MustWorkFailed => 20,
        ChangeKind::BrokenLink => 21,
        ChangeKind::DescriptionChanged => 22,
        ChangeKind::H1Changed => 23,
        ChangeKind::AddedToSitemap => 24,
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

/// Trimmed text, or `None` when it is missing or blank.
fn text(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|t| !t.is_empty())
}

/// A field that was added, removed or reworded, as one change of `kind`.
fn text_changed(kind: ChangeKind, old: Option<&str>, new: Option<&str>) -> Option<PageChange> {
    let (old, new) = (text(old), text(new));
    (old != new).then(|| {
        (
            kind,
            Severity::Notice,
            old.unwrap_or_default().to_owned(),
            new.unwrap_or_default().to_owned(),
        )
    })
}

/// A page other pages link to that does not answer now and did before (or is new, when `old`
/// is `None`). A page nothing links to is left to `status_changed`: no visitor reaches it by
/// a link.
pub(crate) fn broken_link(old: Option<&PageRecord>, curr: &PageRecord) -> Option<PageChange> {
    if !is_error(curr) || curr.inlinks == 0 || old.is_some_and(is_error) {
        return None;
    }
    Some((
        ChangeKind::BrokenLink,
        Severity::Warning,
        old.map(|p| p.status.to_string()).unwrap_or_default(),
        curr.status.to_string(),
    ))
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
    // Wording only matters on a page that answers: an error page has no description or H1 of
    // the site's choosing, and its status change already says what happened.
    if !is_error(old) && !is_error(curr) {
        out.extend(text_changed(
            ChangeKind::DescriptionChanged,
            old.fields.meta_description.as_deref(),
            curr.fields.meta_description.as_deref(),
        ));
        out.extend(text_changed(
            ChangeKind::H1Changed,
            old.fields.h1.first().map(String::as_str),
            curr.fields.h1.first().map(String::as_str),
        ));
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

/// Whether both sitemap readings are whole, so a page's `in_sitemap` can be compared.
pub(crate) fn sitemaps_comparable(prev: &SitemapSummary, curr: &SitemapSummary) -> bool {
    let whole = |s: &SitemapSummary| s.failed_files == 0 && s.complete && !s.truncated;
    whole(prev) && whole(curr)
}

/// The sitemap lost 10% or more of its URLs. Skipped when either summary may be partial.
pub(crate) fn sitemap_shrank(prev: &SitemapSummary, curr: &SitemapSummary) -> bool {
    if prev.failed_files > 0 || curr.failed_files > 0 || !prev.complete || !curr.complete {
        return false;
    }
    prev.url_count > 0 && u64::from(curr.url_count) * 10 <= u64::from(prev.url_count) * 9
}
