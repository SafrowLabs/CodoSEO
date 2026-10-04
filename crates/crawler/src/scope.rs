//! The internal/external rule: same host and port as the start address.

use url::Url;

/// Decides which URLs belong to the site being crawled.
///
/// A URL is internal when its host (lower-cased, one leading `www.` removed) and its port
/// match the start address. The scheme is ignored, so `http://` and `https://` versions of
/// a page are the same site. `Url::port` is `None` for a scheme's default port, so `:80` on
/// `http` and `:443` on `https` both read as "no explicit port"; any other port is part of
/// the site's identity.
/// Subdomains other than `www.` are external.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteScope {
    host: String,
    port: Option<u16>,
}

impl SiteScope {
    pub fn new(origin: &Url) -> SiteScope {
        SiteScope {
            host: host_key(origin),
            port: origin.port(),
        }
    }

    pub fn is_internal(&self, url: &Url) -> bool {
        host_key(url) == self.host && url.port() == self.port
    }
}

fn host_key(url: &Url) -> String {
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    match host.strip_prefix("www.") {
        Some(rest) => rest.to_owned(),
        None => host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn ignores_scheme_and_leading_www() {
        let s = SiteScope::new(&u("https://www.e.com/"));
        assert!(s.is_internal(&u("https://e.com/a")));
        assert!(s.is_internal(&u("http://www.e.com/b")));
        assert!(s.is_internal(&u("HTTPS://WWW.E.COM/c")));
    }

    #[test]
    fn subdomains_and_other_ports_are_external() {
        let s = SiteScope::new(&u("https://www.e.com/"));
        assert!(!s.is_internal(&u("https://blog.e.com/")));
        assert!(!s.is_internal(&u("https://e.com:8443/")));
        assert!(!s.is_internal(&u("https://other.org/")));
        assert!(!s.is_internal(&u("https://www.www.e.com/")));
    }

    #[test]
    fn explicit_ports_must_match_and_default_ports_are_equal() {
        let s = SiteScope::new(&u("http://e.com/"));
        assert!(s.is_internal(&u("https://e.com/")));
        assert!(s.is_internal(&u("http://e.com:80/")));
        assert!(s.is_internal(&u("https://e.com:443/")));
        let s = SiteScope::new(&u("http://127.0.0.1:8080/"));
        assert!(s.is_internal(&u("https://127.0.0.1:8080/x")));
        assert!(!s.is_internal(&u("http://127.0.0.1:9090/x")));
        assert!(!s.is_internal(&u("http://127.0.0.1/x")));
    }
}
