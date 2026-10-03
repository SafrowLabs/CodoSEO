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
