//! How wide a title and description render in Google's results, in pixels, so the explorer
//! can show where a snippet gets cut. Google sets titles in Arial 20 px and descriptions in
//! Arial 14 px; the widths come from Arial's advance widths (2048 units per em), grouped by
//! width, with a sensible default for characters outside the table.

/// Where Google cuts a title on desktop.
pub const TITLE_LIMIT_PX: u32 = 580;
/// Where Google cuts a description on desktop (about two lines).
pub const DESCRIPTION_LIMIT_PX: u32 = 990;

const UNITS_PER_EM: u64 = 2048;

/// Width of `text` set as a result title (Arial 20 px).
pub fn title_px(text: &str) -> u32 {
    px(text, 20)
}

/// Width of `text` set as a result description (Arial 14 px).
pub fn description_px(text: &str) -> u32 {
    px(text, 14)
}

fn px(text: &str, size: u64) -> u32 {
    let units: u64 = text.chars().map(|c| u64::from(advance(c))).sum();
    // Rounded to the nearest pixel.
    ((units * size + UNITS_PER_EM / 2) / UNITS_PER_EM).min(u64::from(u32::MAX)) as u32
}

/// Arial's advance width for `c`, in units of 1/2048 em.
fn advance(c: char) -> u16 {
    match c {
        'i' | 'j' | 'l' => 455,
        ' ' | '\u{a0}' | '!' | ',' | '.' | '/' | ':' | ';' | '[' | '\\' | ']' | 'f' | 't' | 'I' => {
            569
        }
        '(' | ')' | '-' | '`' | 'r' => 682,
        'c' | 'k' | 's' | 'v' | 'x' | 'y' | 'z' | 'J' => 1024,
        '0'..='9'
        | 'a'
        | 'b'
        | 'd'
        | 'e'
        | 'g'
        | 'h'
        | 'n'
        | 'o'
        | 'p'
        | 'q'
        | 'u'
        | '#'
        | '$'
        | '?'
        | '_'
        | 'L' => 1139,
        '+' | '<' | '=' | '>' | '~' | '×' => 1196,
        'F' | 'T' | 'Z' => 1251,
        'A' | 'B' | 'E' | 'K' | 'P' | 'S' | 'V' | 'X' | 'Y' | '&' => 1366,
        'C' | 'D' | 'H' | 'N' | 'R' | 'U' | 'w' => 1479,
        'G' | 'O' | 'Q' => 1593,
        'M' | 'm' => 1706,
        '%' => 1821,
        'W' => 1933,
        '@' => 2079,
        '\'' => 391,
        '|' => 532,
        '{' | '}' => 684,
        '"' => 727,
        '*' => 797,
        '^' => 961,
        // Typographic punctuation and symbols common in titles.
        '‘' | '’' | '‚' => 455,
        '“' | '”' | '„' | '·' => 683,
        '•' => 717,
        '–' | '€' | '£' | '¥' => 1139,
        '©' | '®' => 1509,
        '—' | '…' | '™' => 2048,
        // Accented Latin letters are as wide as their base letter; the narrow i forms aside,
        // the case averages are close enough.
        'ì' | 'í' | 'î' | 'ï' | 'ı' | 'Ì' | 'Í' | 'Î' | 'Ï' => 569,
        c if is_wide(c) => 2048,
        c if c.is_uppercase() => 1430,
        _ => 1139,
    }
}

/// CJK, Hangul, full-width forms and emoji: one em each.
fn is_wide(c: char) -> bool {
    matches!(
        c as u32,
        0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1FAFF
            | 0x20000..=0x3FFFD
    )
}

/// Cuts `text` so it fits in `limit` pixels as measured by `f`, the way Google does: at a
/// word boundary when that keeps most of the text, followed by `" …"`. Returns the text and
/// whether it was cut. The result, ellipsis included, never measures more than `limit`.
pub fn truncate_to_px(text: &str, limit: u32, f: impl Fn(&str) -> u32) -> (String, bool) {
    const ELLIPSIS: &str = " …";
    let text = text.trim();
    if f(text) <= limit {
        return (text.to_owned(), false);
    }
    let bounds: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect();
    let cut_at = |k: usize| text[..bounds[k]].trim_end();
    let fits = |k: usize| f(&format!("{}{ELLIPSIS}", cut_at(k))) <= limit;
    if !fits(0) {
        return (String::new(), true);
    }
    // The longest prefix that still fits; widths only grow as the prefix does.
    let (mut lo, mut hi) = (0, bounds.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut cut = cut_at(lo);
    if let Some(space) = cut.rfind(char::is_whitespace)
        && space >= cut.len() * 2 / 3
    {
        cut = cut[..space].trim_end();
    }
    (format!("{cut}{ELLIPSIS}"), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typical_sixty_character_title_is_close_to_the_limit() {
        let title = "How to Choose Running Shoes for Flat Feet - A Complete Guide";
        assert_eq!(title.chars().count(), 60);
        let w = title_px(title);
        assert!((530..=630).contains(&w), "{w}");
    }

    #[test]
    fn wide_letters_are_wider_than_narrow_ones() {
        assert!(title_px("WWWW") > title_px("iiii"));
        assert!(description_px("WWWW") > description_px("iiii"));
        assert_eq!(title_px(""), 0);
        // "W" is 1933/2048 em: about 19 px at 20 px.
        assert_eq!(title_px("W"), 19);
    }

    #[test]
    fn descriptions_are_set_smaller_than_titles() {
        let s = "A meta description that is comfortably long enough to pass the check.";
        let (t, d) = (title_px(s), description_px(s));
        assert!(d < t);
        let expected = t * 14 / 20;
        assert!(d.abs_diff(expected) <= 2, "{d} vs {expected}");
    }

    #[test]
    fn unknown_characters_get_a_width() {
        assert!(title_px("東京") > title_px("ab"));
        assert!(title_px("Émile") > 0);
        assert_eq!(title_px("é"), title_px("e"));
    }

    #[test]
    fn short_text_is_left_alone() {
        let (out, cut) = truncate_to_px("  Short title ", TITLE_LIMIT_PX, title_px);
        assert_eq!(out, "Short title");
        assert!(!cut);
    }

    #[test]
    fn truncation_never_exceeds_the_limit() {
        let long = "The Complete Beginner's Guide to Growing Tomatoes Indoors All Year Round \
                    With Hydroponics, Grow Lights and Very Little Space";
        for limit in [0, 5, 20, 40, 100, 250, 400, 579, 580, 600, 990] {
            for f in [title_px as fn(&str) -> u32, description_px] {
                let (out, cut) = truncate_to_px(long, limit, f);
                assert!(f(&out) <= limit, "{limit}: {out:?} is {}", f(&out));
                if f(long) > limit {
                    assert!(cut);
                    assert!(out.is_empty() || out.ends_with(" …"), "{out:?}");
                }
            }
        }
        let (out, cut) = truncate_to_px(long, TITLE_LIMIT_PX, title_px);
        assert!(cut);
        assert!(out.starts_with("The Complete Beginner's Guide"));
        // Cut at a word boundary, not mid-word.
        let kept = out.trim_end_matches(" …");
        assert!(long.starts_with(kept));
        assert!(long[kept.len()..].starts_with(' '), "{out:?}");
    }

    #[test]
    fn one_long_word_is_cut_mid_word() {
        let word = "a".repeat(200);
        let (out, cut) = truncate_to_px(&word, 300, title_px);
        assert!(cut);
        // "a" is about 11 px and " …" about 26 px: some 24 letters fit in 300 px.
        assert!(out.starts_with(&"a".repeat(20)), "{out:?}");
        assert!(title_px(&out) <= 300);
    }
}
