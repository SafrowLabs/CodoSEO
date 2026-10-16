//! The numbers behind the length, thin, slow and redirect checks, which a site policy may change,
//! and the check titles that name them.

use std::borrow::Cow;

use codoseo_core::check::CheckId;

use crate::registry::{BASE_CHECKS, def};

/// The values the threshold checks compare with. [`Thresholds::DEFAULT`] is what every crawl
/// used before site policies, and what a crawl without one still uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Thresholds {
    /// A title longer than this many characters is too long.
    pub title_max: u16,
    /// A title shorter than this many characters is too short.
    pub title_min: u16,
    pub description_max: u16,
    pub description_min: u16,
    /// An indexable page with fewer words is thin.
    pub thin_words: u32,
    /// A response slower than this, in milliseconds to the headers (about the time to first
    /// byte), is slow.
    pub slow_ms: u32,
    /// More redirect hops than this is a chain.
    pub max_redirect_hops: u8,
    /// Whether images without alt text are flagged at all.
    pub image_alt: bool,
}

impl Thresholds {
    pub const DEFAULT: Thresholds = Thresholds {
        title_max: 60,
        title_min: 30,
        description_max: 160,
        description_min: 70,
        thin_words: 200,
        slow_ms: 1000,
        max_redirect_hops: 1,
        image_alt: true,
    };

    /// The checks a crawl runs under these thresholds: [`BASE_CHECKS`], less the image alt check
    /// when it is switched off.
    pub fn base_checks(&self) -> usize {
        BASE_CHECKS - usize::from(!self.image_alt)
    }
}

impl Default for Thresholds {
    fn default() -> Thresholds {
        Thresholds::DEFAULT
    }
}

/// `1 second`, `1.5 seconds`, `800 ms`.
pub fn duration_words(ms: u32) -> String {
    match ms {
        1000 => "1 second".to_owned(),
        ms if ms.is_multiple_of(1000) => format!("{} seconds", ms / 1000),
        ms if ms > 1000 && ms.is_multiple_of(100) => {
            format!("{}.{} seconds", ms / 1000, ms % 1000 / 100)
        }
        ms => format!("{ms} ms"),
    }
}

/// The check's title with the numbers of `t`: "Title is over 65 characters". A check no
/// threshold changes keeps its registry title, and the defaults give the registry titles exactly.
pub fn title_for(id: CheckId, t: &Thresholds) -> Cow<'static, str> {
    let title = def(id).title;
    let d = Thresholds::DEFAULT;
    let dynamic = match id {
        CheckId::TitleTooLong if t.title_max != d.title_max => {
            format!("Title is over {} characters", t.title_max)
        }
        CheckId::TitleTooShort if t.title_min != d.title_min => {
            format!("Title is under {} characters", t.title_min)
        }
        CheckId::DescriptionTooLong if t.description_max != d.description_max => {
            format!("Meta description is over {} characters", t.description_max)
        }
        CheckId::DescriptionTooShort if t.description_min != d.description_min => {
            format!("Meta description is under {} characters", t.description_min)
        }
        CheckId::ThinContent if t.thin_words != d.thin_words => {
            format!("Page has under {} words", t.thin_words)
        }
        CheckId::SlowResponse if t.slow_ms != d.slow_ms => {
            format!("Response took over {}", duration_words(t.slow_ms))
        }
        CheckId::RedirectChain if t.max_redirect_hops != d.max_redirect_hops => {
            format!(
                "Redirect chain has {} or more hops",
                u16::from(t.max_redirect_hops) + 1
            )
        }
        _ => return Cow::Borrowed(title),
    };
    Cow::Owned(dynamic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_titles_are_the_registry_titles() {
        for id in CheckId::ALL {
            assert_eq!(title_for(id, &Thresholds::DEFAULT), def(id).title, "{id:?}");
        }
    }

    #[test]
    fn titles_name_the_numbers() {
        let t = Thresholds {
            title_max: 65,
            thin_words: 120,
            slow_ms: 1500,
            max_redirect_hops: 2,
            ..Thresholds::DEFAULT
        };
        assert_eq!(
            title_for(CheckId::TitleTooLong, &t),
            "Title is over 65 characters"
        );
        assert_eq!(
            title_for(CheckId::ThinContent, &t),
            "Page has under 120 words"
        );
        assert_eq!(
            title_for(CheckId::SlowResponse, &t),
            "Response took over 1.5 seconds"
        );
        assert_eq!(
            title_for(CheckId::RedirectChain, &t),
            "Redirect chain has 3 or more hops"
        );
        let ms = Thresholds {
            slow_ms: 800,
            ..Thresholds::DEFAULT
        };
        assert_eq!(
            title_for(CheckId::SlowResponse, &ms),
            "Response took over 800 ms"
        );
    }
}
