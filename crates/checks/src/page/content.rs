//! Content checks.

use codoseo_core::page::PageRecord;

use super::is_indexable;

pub fn thin_content(p: &PageRecord) -> bool {
    p.is_html_ok() && is_indexable(p) && p.fields.word_count < 200
}

pub fn images_missing_alt(p: &PageRecord) -> bool {
    p.fields.images_missing_alt >= 1
}
