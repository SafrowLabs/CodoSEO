//! The RankOrg link (spec sections 8 and 14): RankOrg is the sibling product CodoSEO feeds, and
//! v0.1 connects them with links only. A link carries the domain, the site's top pages and UTM
//! tags, so RankOrg can start from what the audit already found.

use url::Url;

/// Shown to RankOrg as `utm_campaign`.
pub const CAMPAIGN: &str = "codoseo_audit";

/// RankOrg's address with the domain, one `page` parameter per top page, and the UTM tags.
/// `medium` says where the click came from: `audit_preview` or `explorer`.
pub fn link(base: &Url, domain: &str, pages: &[String], medium: &str) -> Url {
    let mut url = base.clone();
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("domain", domain);
        for page in pages {
            q.append_pair("page", page);
        }
        q.append_pair("utm_source", "codoseo");
        q.append_pair("utm_medium", medium);
        q.append_pair("utm_campaign", CAMPAIGN);
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carries_domain_pages_and_utm_tags_and_keeps_the_base_query() {
        let base = Url::parse("https://rankorg.example/start?ref=abc").unwrap();
        let pages = vec![
            "https://example.com/a?x=1&y=2".to_owned(),
            "https://example.com/b c".to_owned(),
        ];
        let url = link(&base, "example.com", &pages, "explorer");
        let q: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(q[0], ("ref".into(), "abc".into()));
        assert!(q.contains(&("domain".into(), "example.com".into())));
        let got: Vec<&str> = q
            .iter()
            .filter(|(k, _)| k == "page")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(
            got,
            ["https://example.com/a?x=1&y=2", "https://example.com/b c"]
        );
        assert!(q.contains(&("utm_source".into(), "codoseo".into())));
        assert!(q.contains(&("utm_medium".into(), "explorer".into())));
        assert!(q.contains(&("utm_campaign".into(), "codoseo_audit".into())));
        // Special characters in a page URL stay inside their value.
        assert!(url.as_str().contains("x%3D1%26y%3D2"), "{url}");
    }
}
