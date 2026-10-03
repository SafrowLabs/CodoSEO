use codoseo_core::Url;
use codoseo_core::url::normalize;
use proptest::prelude::*;

proptest! {
    #[test]
    fn normalising_twice_changes_nothing(href in "[a-zA-Z0-9/%._~?=&#:-]{0,40}") {
        let base = Url::parse("https://example.com/dir/page").unwrap();
        if let Some(once) = normalize(&base, &href) {
            let twice = normalize(&base, once.as_str());
            prop_assert_eq!(Some(once), twice);
        }
    }
}
