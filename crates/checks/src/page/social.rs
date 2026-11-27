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

/// Whether `code` is a hreflang value search engines read: `x-default`, or a language
/// (two or three letters), optionally a script (four letters), optionally a region (two letters).
/// Hyphens only, any letter case.
///
/// Beyond the shape, the regions people most often get wrong are refused: `UK` (the code is
/// `GB`), `EU` and `LATAM`-style names, and numeric regions such as `419`, which are valid
/// language tags but not values search engines use for hreflang.
pub fn valid_hreflang_code(code: &str) -> bool {
    let code = code.trim();
    if code.eq_ignore_ascii_case("x-default") {
        return true;
    }
    let alpha = |s: &str, lens: &[usize]| {
        lens.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphabetic())
    };
    let mut parts = code.split('-');
    let Some(lang) = parts.next() else {
        return false;
    };
    if !alpha(lang, &[2, 3]) {
        return false;
    }
    let rest: Vec<&str> = parts.collect();
    let region_ok =
        |r: &str| alpha(r, &[2]) && !r.eq_ignore_ascii_case("uk") && !r.eq_ignore_ascii_case("eu");
    match rest.as_slice() {
        [] => true,
        [one] => alpha(one, &[4]) || region_ok(one),
        [script, region] => alpha(script, &[4]) && region_ok(region),
        _ => false,
    }
}

pub fn hreflang_invalid_code(p: &PageRecord) -> bool {
    p.is_html_ok()
        && p.fields
            .hreflang
            .iter()
            .any(|(code, _)| !valid_hreflang_code(code))
}

pub fn hreflang_missing_self(p: &PageRecord) -> bool {
    p.is_html_ok()
        && !p.fields.hreflang.is_empty()
        && p.fields.hreflang.iter().all(|(_, url)| *url != p.url)
}

#[cfg(test)]
mod tests {
    use super::valid_hreflang_code;

    #[test]
    fn hreflang_codes_search_engines_read() {
        for ok in [
            "en",
            "EN",
            "en-GB",
            "en-gb",
            "pt-BR",
            "zh-Hans",
            "zh-Hant-TW",
            "fil",
            "x-default",
            "X-Default",
            " de ",
        ] {
            assert!(valid_hreflang_code(ok), "{ok}");
        }
    }

    #[test]
    fn hreflang_codes_they_ignore() {
        for bad in [
            "",
            "en_US",
            "en-UK",
            "en-EU",
            "english",
            "e",
            "en-",
            "-GB",
            "en-usa",
            "es-419",
            "en-GB-x",
            "en GB",
            "en-Latn-GB-oed",
        ] {
            assert!(!valid_hreflang_code(bad), "{bad:?}");
        }
    }
}
