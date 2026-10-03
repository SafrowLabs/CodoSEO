//! Open Graph, and the Schema checks (JSON-LD and hreflang).

use codoseo_core::page::{JsonLdStatus, PageRecord};

use super::{is_blank, is_indexable};

pub fn og_missing(p: &PageRecord) -> bool {
    p.is_html_ok()
        && is_indexable(p)
        && (is_blank(&p.fields.og.title) || is_blank(&p.fields.og.image))
}

pub fn jsonld_invalid(p: &PageRecord) -> bool {
    p.fields.jsonld == JsonLdStatus::Invalid
}

pub fn hreflang_missing_self(p: &PageRecord) -> bool {
    p.is_html_ok()
        && !p.fields.hreflang.is_empty()
        && p.fields.hreflang.iter().all(|(_, url)| *url != p.url)
}
