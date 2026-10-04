use std::collections::HashSet;

use codoseo_core::check::{CheckId, IssueBits};

#[test]
fn check_ids_are_dense_unique_and_below_64() {
    let bits: Vec<u8> = CheckId::ALL.iter().map(|c| c.bit()).collect();
    assert_eq!(bits, (0..44).collect::<Vec<u8>>());
    for c in CheckId::ALL {
        assert_eq!(CheckId::from_bit(c.bit()), Some(c));
    }
    assert_eq!(CheckId::from_bit(44), None);
    let slugs: HashSet<_> = CheckId::ALL.iter().map(|c| c.slug()).collect();
    assert_eq!(slugs.len(), 44);
    assert_eq!(
        serde_json::to_string(&CheckId::TitleMissing).unwrap(),
        "\"title_missing\""
    );
    assert_eq!(CheckId::TitleMissing.slug(), "title_missing");
}

#[test]
fn slug_matches_serde_name_for_every_check() {
    for c in CheckId::ALL {
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, format!("\"{}\"", c.slug()));
        assert_eq!(serde_json::from_str::<CheckId>(&json).unwrap(), c);
    }
}

/// The persisted bit of every check. Never edit an existing row; new checks are appended.
const PINNED: [(&str, u8); 44] = [
    ("http_4xx", 0),
    ("http_5xx", 1),
    ("fetch_failed", 2),
    ("redirect_loop", 3),
    ("redirected", 4),
    ("redirect_chain", 5),
    ("noindex", 6),
    ("canonicalised", 7),
    ("canonical_missing", 8),
    ("blocked_by_robots", 9),
    ("canonical_to_non_200", 10),
    ("robots_blocks_site", 11),
    ("title_missing", 12),
    ("title_too_long", 13),
    ("title_too_short", 14),
    ("title_multiple", 15),
    ("title_duplicate", 16),
    ("description_missing", 17),
    ("description_too_long", 18),
    ("description_too_short", 19),
    ("description_duplicate", 20),
    ("h1_missing", 21),
    ("h1_multiple", 22),
    ("h1_duplicate", 23),
    ("thin_content", 24),
    ("content_duplicate", 25),
    ("images_missing_alt", 26),
    ("links_to_broken", 27),
    ("links_to_redirect", 28),
    ("orphan", 29),
    ("no_internal_outlinks", 30),
    ("nofollow_internal_links", 31),
    ("deep_page", 32),
    ("sitemap_non_200", 33),
    ("sitemap_noindex", 34),
    ("sitemap_canonicalised", 35),
    ("not_in_sitemap", 36),
    ("sitemap_missing", 37),
    ("mixed_content", 38),
    ("not_https", 39),
    ("slow_response", 40),
    ("og_missing", 41),
    ("jsonld_invalid", 42),
    ("hreflang_missing_self", 43),
];

#[test]
fn every_slug_keeps_its_persisted_bit() {
    let actual: Vec<(&str, u8)> = CheckId::ALL.iter().map(|c| (c.slug(), c.bit())).collect();
    assert_eq!(actual, PINNED);
}

#[test]
fn stable_values_never_move() {
    assert_eq!(CheckId::Http4xx as u8, 0);
    assert_eq!(CheckId::RobotsBlocksSite as u8, 11);
    assert_eq!(CheckId::LinksToBroken as u8, 27);
    assert_eq!(CheckId::HreflangMissingSelf as u8, 43);
}

#[test]
fn issue_bits_by_check() {
    let mut b = IssueBits::default();
    b.set_check(CheckId::Orphan);
    assert!(b.has_check(CheckId::Orphan) && !b.has_check(CheckId::Noindex));
}
