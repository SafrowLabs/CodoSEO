//! The other addresses of the home page: `http` and `https`, with and without `www.`.
//!
//! People type and link all four. Each one that answers should end, after its redirects, on the
//! site's settled origin. One that answers with an error, or serves a page of its own without
//! redirecting, is reported. One that does not answer at all (no DNS record for `www.`, port 80
//! closed) is not: nothing is served there, so there is nothing to mislead a visitor or a
//! search engine.

use std::time::Duration;

use codoseo_core::output::HomeVariant;
use tokio::time::{Instant, timeout_at};
use url::{Host, Url};

use crate::fetch::Fetcher;

/// The longest one variant may take.
const VARIANT_TIMEOUT: Duration = Duration::from_secs(10);

/// The three other addresses of `origin`'s home page; none for an IP address, a single-label
/// host (`localhost`) or a non-default port, which have no such family.
pub(crate) fn variants_of(origin: &Url) -> Vec<Url> {
    let Some(Host::Domain(host)) = origin.host() else {
        return Vec::new();
    };
    if origin.port().is_some() || !host.contains('.') {
        return Vec::new();
    }
    let apex = host.strip_prefix("www.").unwrap_or(host);
    let mut out = Vec::new();
    for scheme in ["https", "http"] {
        for h in [apex.to_owned(), format!("www.{apex}")] {
            if let Ok(u) = Url::parse(&format!("{scheme}://{h}/"))
                && u.origin() != origin.origin()
            {
                out.push(u);
            }
        }
    }
    out
}

/// Whether a variant's final answer is the site: it ended on `origin` without an error.
fn leads_home(origin: &Url, status: u16, final_url: &Url) -> bool {
    status < 400 && final_url.origin() == origin.origin()
}

/// Requests each variant once, following redirects. Variants that do not answer are left out.
pub(crate) async fn probe(fetcher: &Fetcher, origin: &Url, deadline: Instant) -> Vec<HomeVariant> {
    let mut out = Vec::new();
    for url in variants_of(origin) {
        let limit = deadline.min(Instant::now() + VARIANT_TIMEOUT);
        let Ok(Ok(res)) = timeout_at(limit, fetcher.fetch_raw(&url, 0)).await else {
            continue;
        };
        out.push(HomeVariant {
            ok: leads_home(origin, res.status, &res.final_url),
            url,
            status: res.status,
            final_url: res.final_url,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn list(origin: &str) -> Vec<String> {
        variants_of(&u(origin))
            .iter()
            .map(|v| v.as_str().to_owned())
            .collect()
    }

    #[test]
    fn the_three_other_addresses_of_the_home_page() {
        assert_eq!(
            list("https://e.com/"),
            ["https://www.e.com/", "http://e.com/", "http://www.e.com/"]
        );
        assert_eq!(
            list("https://www.e.com/"),
            ["https://e.com/", "http://e.com/", "http://www.e.com/"]
        );
        assert_eq!(
            list("http://e.com/"),
            ["https://e.com/", "https://www.e.com/", "http://www.e.com/"]
        );
        // A subdomain's own family: itself and its www.
        assert_eq!(list("https://blog.e.com/").len(), 3);
    }

    #[test]
    fn addresses_without_such_a_family_have_none() {
        for origin in [
            "http://127.0.0.1/",
            "http://localhost/",
            "https://e.com:8443/",
            "http://[::1]/",
        ] {
            assert!(list(origin).is_empty(), "{origin}");
        }
    }

    #[test]
    fn a_variant_is_fine_only_when_it_ends_on_the_site() {
        let origin = u("https://e.com/");
        assert!(leads_home(&origin, 200, &u("https://e.com/")));
        assert!(leads_home(&origin, 200, &u("https://e.com/en/")));
        // Its own 404, or its own copy of the site.
        assert!(!leads_home(&origin, 404, &u("http://www.e.com/")));
        assert!(!leads_home(&origin, 200, &u("http://www.e.com/")));
        // Redirected home, but home is failing.
        assert!(!leads_home(&origin, 503, &u("https://e.com/")));
    }
}
