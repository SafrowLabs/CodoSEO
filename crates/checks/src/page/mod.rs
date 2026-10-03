//! Checks that look at one page on its own.
//!
//! Each check is a `fn(&PageRecord) -> bool`, kept in the file for its group:
//!
//! - `response.rs`: Response and Indexability checks
//! - `onpage.rs`: title, description and H1 checks
//! - `content.rs`: thin content and image alt text
//! - `technical.rs`: Technical checks, plus the Links page checks (`no_internal_outlinks`,
//!   `nofollow_internal_links`, `deep_page`)
//! - `social.rs`: Open Graph, plus the Schema checks (`jsonld_invalid`, `hreflang_missing_self`)

mod content;
mod onpage;
mod response;
mod social;
mod technical;

use codoseo_core::check::{CheckId, IssueBits};
use codoseo_core::page::PageRecord;

type PageCheck = fn(&PageRecord) -> bool;

/// Every Page-scope check, in `CheckId` order.
const PAGE_CHECKS: [(CheckId, PageCheck); 30] = [
    (CheckId::Http4xx, response::http_4xx),
    (CheckId::Http5xx, response::http_5xx),
    (CheckId::FetchFailed, response::fetch_failed),
    (CheckId::RedirectLoop, response::redirect_loop),
    (CheckId::Redirected, response::redirected),
    (CheckId::RedirectChain, response::redirect_chain),
    (CheckId::Noindex, response::noindex),
    (CheckId::Canonicalised, response::canonicalised),
    (CheckId::CanonicalMissing, response::canonical_missing),
    (CheckId::BlockedByRobots, response::blocked_by_robots),
    (CheckId::TitleMissing, onpage::title_missing),
    (CheckId::TitleTooLong, onpage::title_too_long),
    (CheckId::TitleTooShort, onpage::title_too_short),
    (CheckId::TitleMultiple, onpage::title_multiple),
    (CheckId::DescriptionMissing, onpage::description_missing),
    (CheckId::DescriptionTooLong, onpage::description_too_long),
    (CheckId::DescriptionTooShort, onpage::description_too_short),
    (CheckId::H1Missing, onpage::h1_missing),
    (CheckId::H1Multiple, onpage::h1_multiple),
    (CheckId::ThinContent, content::thin_content),
    (CheckId::ImagesMissingAlt, content::images_missing_alt),
    (CheckId::NoInternalOutlinks, technical::no_internal_outlinks),
    (
        CheckId::NofollowInternalLinks,
        technical::nofollow_internal_links,
    ),
    (CheckId::DeepPage, technical::deep_page),
    (CheckId::MixedContent, technical::mixed_content),
    (CheckId::NotHttps, technical::not_https),
    (CheckId::SlowResponse, technical::slow_response),
    (CheckId::OgMissing, social::og_missing),
    (CheckId::JsonldInvalid, social::jsonld_invalid),
    (CheckId::HreflangMissingSelf, social::hreflang_missing_self),
];

/// The Page-scope checks that fail on this page.
pub fn page_issues(page: &PageRecord) -> IssueBits {
    let mut bits = IssueBits::default();
    for (id, check) in PAGE_CHECKS {
        if check(page) {
            bits.set_check(id);
        }
    }
    bits
}

/// An indexable page: the checks about search appearance only apply to these.
fn is_indexable(page: &PageRecord) -> bool {
    page.indexability == codoseo_core::page::Indexability::Indexable
}

fn is_blank(text: &Option<String>) -> bool {
    text.as_deref().is_none_or(|t| t.trim().is_empty())
}

/// Length in characters after trimming; 0 for missing or blank text.
fn char_len(text: &Option<String>) -> usize {
    text.as_deref().map_or(0, |t| t.trim().chars().count())
}
