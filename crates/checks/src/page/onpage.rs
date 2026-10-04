//! Title, description and H1 checks. All need a readable HTML response, so a 404 page is
//! reported once (as `http_4xx`) and not again as having no title.

use codoseo_core::page::PageRecord;

use super::{char_len, is_blank};

pub fn title_missing(p: &PageRecord) -> bool {
    p.is_html_ok() && is_blank(&p.fields.title)
}

pub fn title_too_long(p: &PageRecord) -> bool {
    p.is_html_ok() && char_len(&p.fields.title) > 60
}

pub fn title_too_short(p: &PageRecord) -> bool {
    p.is_html_ok() && (1..30).contains(&char_len(&p.fields.title))
}

pub fn title_multiple(p: &PageRecord) -> bool {
    p.is_html_ok() && p.fields.title_count > 1
}

pub fn description_missing(p: &PageRecord) -> bool {
    p.is_html_ok() && is_blank(&p.fields.meta_description)
}

pub fn description_too_long(p: &PageRecord) -> bool {
    p.is_html_ok() && char_len(&p.fields.meta_description) > 160
}

pub fn description_too_short(p: &PageRecord) -> bool {
    p.is_html_ok() && (1..70).contains(&char_len(&p.fields.meta_description))
}

pub fn h1_missing(p: &PageRecord) -> bool {
    p.is_html_ok() && p.fields.h1.iter().all(|h| h.trim().is_empty())
}

pub fn h1_multiple(p: &PageRecord) -> bool {
    p.is_html_ok() && p.fields.h1.len() > 1
}
