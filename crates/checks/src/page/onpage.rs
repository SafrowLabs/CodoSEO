//! Title, description and H1 checks. All need a readable HTML response, so a 404 page is
//! reported once (as `http_4xx`) and not again as having no title.

use codoseo_core::page::PageRecord;

use super::{char_len, is_blank};
use crate::Thresholds;

pub fn title_missing(p: &PageRecord) -> bool {
    p.is_html_ok() && is_blank(&p.fields.title)
}

pub fn title_too_long(p: &PageRecord, t: &Thresholds) -> bool {
    p.is_html_ok() && char_len(&p.fields.title) > usize::from(t.title_max)
}

pub fn title_too_short(p: &PageRecord, t: &Thresholds) -> bool {
    p.is_html_ok() && (1..usize::from(t.title_min)).contains(&char_len(&p.fields.title))
}

pub fn title_multiple(p: &PageRecord) -> bool {
    p.is_html_ok() && p.fields.title_count > 1
}

pub fn description_missing(p: &PageRecord) -> bool {
    p.is_html_ok() && is_blank(&p.fields.meta_description)
}

pub fn description_too_long(p: &PageRecord, t: &Thresholds) -> bool {
    p.is_html_ok() && char_len(&p.fields.meta_description) > usize::from(t.description_max)
}

pub fn description_too_short(p: &PageRecord, t: &Thresholds) -> bool {
    p.is_html_ok()
        && (1..usize::from(t.description_min)).contains(&char_len(&p.fields.meta_description))
}

pub fn h1_missing(p: &PageRecord) -> bool {
    p.is_html_ok() && p.fields.h1.iter().all(|h| h.trim().is_empty())
}

pub fn h1_multiple(p: &PageRecord) -> bool {
    p.is_html_ok() && p.fields.h1.len() > 1
}
