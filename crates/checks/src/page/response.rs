//! Response and Indexability checks.

use codoseo_core::page::{FetchFailure, Indexability, PageRecord};

use super::is_indexable;

pub fn http_4xx(p: &PageRecord) -> bool {
    (400..500).contains(&p.status)
}

pub fn http_5xx(p: &PageRecord) -> bool {
    (500..600).contains(&p.status)
}

fn is_loop(error: FetchFailure) -> bool {
    matches!(
        error,
        FetchFailure::RedirectLoop | FetchFailure::TooManyRedirects
    )
}

pub fn fetch_failed(p: &PageRecord) -> bool {
    p.error.is_some_and(|e| !is_loop(e))
}

pub fn redirect_loop(p: &PageRecord) -> bool {
    p.error.is_some_and(is_loop)
}

pub fn redirected(p: &PageRecord) -> bool {
    (300..400).contains(&p.status) && p.error.is_none()
}

pub fn redirect_chain(p: &PageRecord) -> bool {
    p.redirect_chain.len() >= 2
}

pub fn noindex(p: &PageRecord) -> bool {
    p.indexability == Indexability::Noindex
}

pub fn canonicalised(p: &PageRecord) -> bool {
    p.indexability == Indexability::Canonicalised
}

pub fn blocked_by_robots(p: &PageRecord) -> bool {
    p.indexability == Indexability::BlockedByRobots
}

pub fn canonical_missing(p: &PageRecord) -> bool {
    p.is_html_ok() && is_indexable(p) && p.fields.canonical.is_none()
}
