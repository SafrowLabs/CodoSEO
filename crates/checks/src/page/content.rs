//! Content checks.

use codoseo_core::page::PageRecord;

use super::is_indexable;
use crate::Thresholds;

pub fn thin_content(p: &PageRecord, t: &Thresholds) -> bool {
    p.is_html_ok() && is_indexable(p) && p.fields.word_count < t.thin_words
}

/// Off when the thresholds switch the check off.
pub fn images_missing_alt(p: &PageRecord, t: &Thresholds) -> bool {
    t.image_alt && p.fields.images_missing_alt >= 1
}
