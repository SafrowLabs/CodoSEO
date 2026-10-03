use codoseo_core::Url;
use codoseo_core::url::{normalize, url_hash};

fn base() -> Url {
    Url::parse("https://Example.com:443/a/b?x=1").unwrap()
}

fn norm(href: &str) -> Option<String> {
    normalize(&base(), href).map(|u| u.to_string())
}

#[test]
fn resolves_relative_links_and_drops_fragments() {
    assert_eq!(norm("c#frag").as_deref(), Some("https://example.com/a/c"));
}

#[test]
fn lowercases_scheme_and_host_and_drops_default_ports() {
    assert_eq!(
        norm("HTTP://EXAMPLE.com:80/Path").as_deref(),
        Some("http://example.com/Path")
    );
}

#[test]
fn keeps_query_strings_as_written() {
    assert_eq!(
        norm("/p?b=2&a=1").as_deref(),
        Some("https://example.com/p?b=2&a=1")
    );
}

#[test]
fn converts_international_hosts_to_punycode() {
    assert_eq!(
        norm("https://bücher.de/").as_deref(),
        Some("https://xn--bcher-kva.de/")
    );
}

#[test]
fn uppercases_escapes_and_decodes_unreserved_characters() {
    assert_eq!(
        norm("/a%2fb%7e?q=%7e%2f").as_deref(),
        Some("https://example.com/a%2Fb~?q=~%2F")
    );
}

#[test]
fn drops_a_trailing_dot_from_the_host() {
    assert_eq!(
        norm("https://example.com./x").as_deref(),
        Some("https://example.com/x")
    );
}

#[test]
fn rejects_non_http_links_fragments_and_blanks() {
    for bad in [
        "mailto:a@b.c",
        "tel:123",
        "javascript:void(0)",
        "data:text/html,x",
        "ftp://x/",
        "#top",
        "  ",
        "",
    ] {
        assert_eq!(norm(bad), None, "{bad:?} should be rejected");
    }
}

#[test]
fn resolves_against_a_base_href() {
    let base_href = Url::parse("https://cdn.example.com/docs/").unwrap();
    assert_eq!(
        normalize(&base_href, "page")
            .map(|u| u.to_string())
            .as_deref(),
        Some("https://cdn.example.com/docs/page")
    );
}

#[test]
fn hash_is_stable_across_equivalent_spellings() {
    let a = Url::parse("https://example.com/a").unwrap();
    let b = normalize(&base(), "/a#x").unwrap();
    assert_eq!(url_hash(&a), url_hash(&b));
    assert_ne!(
        url_hash(&a),
        url_hash(&Url::parse("https://example.com/b").unwrap())
    );
}
