//! Technical checks, and the Links checks that only need this page.

use codoseo_core::page::PageRecord;

pub fn no_internal_outlinks(p: &PageRecord) -> bool {
    p.is_html_ok() && p.outlinks_internal == 0
}

pub fn nofollow_internal_links(p: &PageRecord) -> bool {
    p.outlinks_nofollow > 0
}

pub fn deep_page(p: &PageRecord) -> bool {
    (200..300).contains(&p.status) && p.depth.is_some_and(|d| d > 3)
}

pub fn mixed_content(p: &PageRecord) -> bool {
    p.url.scheme() == "https" && p.fields.mixed_content > 0
}

pub fn not_https(p: &PageRecord) -> bool {
    p.url.scheme() == "http" && (200..300).contains(&p.status)
}

pub fn slow_response(p: &PageRecord) -> bool {
    p.status != 0 && p.response_ms > 1_000
}
