//! Check IDs, severity and the per-page issue bitmask. The check metadata (title, severity,
//! category) lives in the checks crate.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    Warning,
    Notice,
}

/// One bit per check, stored as `pages.issues`. Bit numbers are never reused.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IssueBits(pub u64);

macro_rules! check_ids {
    ($($variant:ident = $slug:literal,)+) => {
        /// Every check, with its stable ID. The value is the bit in `IssueBits` and is never
        /// reused; new checks are appended. The slug is the serde name.
        #[repr(u8)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum CheckId {
            $(
                #[serde(rename = $slug)]
                $variant,
            )+
        }

        impl CheckId {
            /// All checks in ID order.
            pub const ALL: [CheckId; { [$($slug),+].len() }] = [$(CheckId::$variant),+];

            /// Stable name, the same as the serde name.
            pub fn slug(self) -> &'static str {
                match self {
                    $(CheckId::$variant => $slug,)+
                }
            }
        }
    };
}

check_ids! {
    Http4xx = "http_4xx",
    Http5xx = "http_5xx",
    FetchFailed = "fetch_failed",
    RedirectLoop = "redirect_loop",
    Redirected = "redirected",
    RedirectChain = "redirect_chain",
    Noindex = "noindex",
    Canonicalised = "canonicalised",
    CanonicalMissing = "canonical_missing",
    BlockedByRobots = "blocked_by_robots",
    CanonicalToNon200 = "canonical_to_non_200",
    RobotsBlocksSite = "robots_blocks_site",
    TitleMissing = "title_missing",
    TitleTooLong = "title_too_long",
    TitleTooShort = "title_too_short",
    TitleMultiple = "title_multiple",
    TitleDuplicate = "title_duplicate",
    DescriptionMissing = "description_missing",
    DescriptionTooLong = "description_too_long",
    DescriptionTooShort = "description_too_short",
    DescriptionDuplicate = "description_duplicate",
    H1Missing = "h1_missing",
    H1Multiple = "h1_multiple",
    H1Duplicate = "h1_duplicate",
    ThinContent = "thin_content",
    ContentDuplicate = "content_duplicate",
    ImagesMissingAlt = "images_missing_alt",
    LinksToBroken = "links_to_broken",
    LinksToRedirect = "links_to_redirect",
    Orphan = "orphan",
    NoInternalOutlinks = "no_internal_outlinks",
    NofollowInternalLinks = "nofollow_internal_links",
    DeepPage = "deep_page",
    SitemapNon200 = "sitemap_non_200",
    SitemapNoindex = "sitemap_noindex",
    SitemapCanonicalised = "sitemap_canonicalised",
    NotInSitemap = "not_in_sitemap",
    SitemapMissing = "sitemap_missing",
    MixedContent = "mixed_content",
    NotHttps = "not_https",
    SlowResponse = "slow_response",
    OgMissing = "og_missing",
    JsonldInvalid = "jsonld_invalid",
    HreflangMissingSelf = "hreflang_missing_self",
}

// IDs are dense from 0, unique and fit in the 64-bit mask; a bad edit fails the build.
const _: () = {
    let mut i = 0;
    while i < CheckId::ALL.len() {
        let value = CheckId::ALL[i] as u8;
        assert!(value < 64, "check ID is 64 or more");
        assert!(
            value as usize == i,
            "check IDs must be dense, unique and in order"
        );
        i += 1;
    }
};

impl CheckId {
    /// The bit number in `IssueBits`.
    pub const fn bit(self) -> u8 {
        self as u8
    }

    pub fn from_bit(bit: u8) -> Option<CheckId> {
        CheckId::ALL.get(usize::from(bit)).copied()
    }
}

impl IssueBits {
    pub fn set_check(&mut self, id: CheckId) {
        self.set(id.bit());
    }

    pub fn has_check(&self, id: CheckId) -> bool {
        self.has(id.bit())
    }

    pub fn set(&mut self, bit: u8) {
        assert!(bit < 64, "check bit {bit} out of range");
        self.0 |= 1 << bit;
    }

    pub fn has(&self, bit: u8) -> bool {
        bit < 64 && self.0 & (1 << bit) != 0
    }

    pub fn iter(&self) -> impl Iterator<Item = u8> + '_ {
        (0..64u8).filter(|b| self.has(*b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_bits_set_and_test() {
        let mut b = IssueBits::default();
        b.set(0);
        b.set(63);
        assert!(b.has(0));
        assert!(b.has(63));
        assert!(!b.has(1));
        assert_eq!(b.iter().collect::<Vec<_>>(), vec![0, 63]);
    }

    #[test]
    #[should_panic(expected = "check bit 64 out of range")]
    fn issue_bit_64_is_rejected() {
        IssueBits::default().set(64);
    }
}
