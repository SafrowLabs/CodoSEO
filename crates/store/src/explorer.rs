//! URL explorer queries over a crawl's `pages`.
//!
//! [`PageFilter`] is the filter vocabulary shared by the explorer, the CSV export, the sidebar
//! report links and the audit screen's issue rows. Its string form (`s4`, `nx`,
//! `check:title_missing`) is what goes in the URL.

use codoseo_core::check::CheckId;

/// One explorer filter. Filters map to a SQL predicate over `pages` (aliased `p`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageFilter {
    All,
    Status2xx,
    Status3xx,
    Status4xx,
    Status5xx,
    /// No HTTP response at all: timeouts, connection errors, redirect loops.
    NoResponse,
    Indexable,
    NonIndexable,
    Html,
    Image,
    Other,
    /// Pages with this check's issue bit set.
    Check(CheckId),
}

impl PageFilter {
    /// Parses the URL form; anything unknown falls back to `All`.
    pub fn parse(s: &str) -> PageFilter {
        match s {
            "s2" => PageFilter::Status2xx,
            "s3" => PageFilter::Status3xx,
            "s4" => PageFilter::Status4xx,
            "s5" => PageFilter::Status5xx,
            "s0" => PageFilter::NoResponse,
            "ix" => PageFilter::Indexable,
            "nx" => PageFilter::NonIndexable,
            "html" => PageFilter::Html,
            "img" => PageFilter::Image,
            "oth" => PageFilter::Other,
            _ => s
                .strip_prefix("check:")
                .and_then(CheckId::from_slug)
                .map_or(PageFilter::All, PageFilter::Check),
        }
    }

    /// The URL form; `parse(f.key()) == f`.
    pub fn key(&self) -> String {
        match self {
            PageFilter::All => "all".to_owned(),
            PageFilter::Status2xx => "s2".to_owned(),
            PageFilter::Status3xx => "s3".to_owned(),
            PageFilter::Status4xx => "s4".to_owned(),
            PageFilter::Status5xx => "s5".to_owned(),
            PageFilter::NoResponse => "s0".to_owned(),
            PageFilter::Indexable => "ix".to_owned(),
            PageFilter::NonIndexable => "nx".to_owned(),
            PageFilter::Html => "html".to_owned(),
            PageFilter::Image => "img".to_owned(),
            PageFilter::Other => "oth".to_owned(),
            PageFilter::Check(id) => format!("check:{}", id.slug()),
        }
    }

    /// A SQL predicate over `pages p`. Only constants are interpolated (status ranges, enum
    /// labels and a check's bit number), never user input.
    pub fn predicate(&self) -> String {
        match self {
            PageFilter::All => "TRUE".to_owned(),
            PageFilter::Status2xx => "p.status BETWEEN 200 AND 299".to_owned(),
            PageFilter::Status3xx => "p.status BETWEEN 300 AND 399".to_owned(),
            PageFilter::Status4xx => "p.status BETWEEN 400 AND 499".to_owned(),
            PageFilter::Status5xx => "p.status BETWEEN 500 AND 599".to_owned(),
            PageFilter::NoResponse => "p.status = 0".to_owned(),
            PageFilter::Indexable => "p.indexability = 'indexable'".to_owned(),
            PageFilter::NonIndexable => "p.indexability <> 'indexable'".to_owned(),
            PageFilter::Html => "p.content_type ILIKE 'text/html%'".to_owned(),
            PageFilter::Image => "p.content_type ILIKE 'image/%'".to_owned(),
            PageFilter::Other => "NOT (coalesce(p.content_type, '') ILIKE 'text/html%' \
                                  OR coalesce(p.content_type, '') ILIKE 'image/%')"
                .to_owned(),
            PageFilter::Check(id) => format!("(p.issues & (1::bigint << {})) <> 0", *id as u8),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        let mut all = vec![
            PageFilter::All,
            PageFilter::Status2xx,
            PageFilter::Status3xx,
            PageFilter::Status4xx,
            PageFilter::Status5xx,
            PageFilter::NoResponse,
            PageFilter::Indexable,
            PageFilter::NonIndexable,
            PageFilter::Html,
            PageFilter::Image,
            PageFilter::Other,
        ];
        all.extend(CheckId::ALL.iter().map(|&c| PageFilter::Check(c)));
        for f in all {
            assert_eq!(PageFilter::parse(&f.key()), f, "{}", f.key());
        }
    }

    #[test]
    fn unknown_filters_fall_back_to_all() {
        assert_eq!(PageFilter::parse("check:nope"), PageFilter::All);
        assert_eq!(PageFilter::parse("'; DROP TABLE pages; --"), PageFilter::All);
    }
}
