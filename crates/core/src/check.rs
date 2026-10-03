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
    ($($variant:ident = $id:literal => $slug:literal,)+) => {
        /// Every check, with its stable ID. The value is the bit in `IssueBits` and is never
        /// reused; new checks are appended with the next number. The slug is the serde name.
        #[repr(u8)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum CheckId {
            $(
                #[serde(rename = $slug)]
                $variant = $id,
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
    Http4xx = 0 => "http_4xx",
    Http5xx = 1 => "http_5xx",
    FetchFailed = 2 => "fetch_failed",
    RedirectLoop = 3 => "redirect_loop",
    Redirected = 4 => "redirected",
    RedirectChain = 5 => "redirect_chain",
    Noindex = 6 => "noindex",
    Canonicalised = 7 => "canonicalised",
    CanonicalMissing = 8 => "canonical_missing",
    BlockedByRobots = 9 => "blocked_by_robots",
    CanonicalToNon200 = 10 => "canonical_to_non_200",
    RobotsBlocksSite = 11 => "robots_blocks_site",
    TitleMissing = 12 => "title_missing",
    TitleTooLong = 13 => "title_too_long",
    TitleTooShort = 14 => "title_too_short",
    TitleMultiple = 15 => "title_multiple",
    TitleDuplicate = 16 => "title_duplicate",
    DescriptionMissing = 17 => "description_missing",
    DescriptionTooLong = 18 => "description_too_long",
    DescriptionTooShort = 19 => "description_too_short",
    DescriptionDuplicate = 20 => "description_duplicate",
    H1Missing = 21 => "h1_missing",
    H1Multiple = 22 => "h1_multiple",
    H1Duplicate = 23 => "h1_duplicate",
    ThinContent = 24 => "thin_content",
    ContentDuplicate = 25 => "content_duplicate",
    ImagesMissingAlt = 26 => "images_missing_alt",
    LinksToBroken = 27 => "links_to_broken",
    LinksToRedirect = 28 => "links_to_redirect",
    Orphan = 29 => "orphan",
    NoInternalOutlinks = 30 => "no_internal_outlinks",
    NofollowInternalLinks = 31 => "nofollow_internal_links",
    DeepPage = 32 => "deep_page",
    SitemapNon200 = 33 => "sitemap_non_200",
    SitemapNoindex = 34 => "sitemap_noindex",
    SitemapCanonicalised = 35 => "sitemap_canonicalised",
    NotInSitemap = 36 => "not_in_sitemap",
    SitemapMissing = 37 => "sitemap_missing",
    MixedContent = 38 => "mixed_content",
    NotHttps = 39 => "not_https",
    SlowResponse = 40 => "slow_response",
    OgMissing = 41 => "og_missing",
    JsonldInvalid = 42 => "jsonld_invalid",
    HreflangMissingSelf = 43 => "hreflang_missing_self",
}

// IDs are unique, below 64, and `ALL` lists them in ascending order. Each ID is written
// out in the table above, so moving or renumbering a variant is a visible edit that the
// pinned table in tests/check_ids.rs catches.
const _: () = {
    let mut i = 0;
    while i < CheckId::ALL.len() {
        assert!((CheckId::ALL[i] as u8) < 64, "check ID is 64 or more");
        if i > 0 {
            assert!(
                (CheckId::ALL[i - 1] as u8) < (CheckId::ALL[i] as u8),
                "check IDs must be unique and in ascending order"
            );
        }
        i += 1;
    }
};

impl CheckId {
    /// The bit number in `IssueBits`.
    pub const fn bit(self) -> u8 {
        self as u8
    }

    pub fn from_bit(bit: u8) -> Option<CheckId> {
        CheckId::ALL.into_iter().find(|c| c.bit() == bit)
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
