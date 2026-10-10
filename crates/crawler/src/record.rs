//! Turns fetch results into page records, without any network access.

use std::collections::HashSet;

use codoseo_core::check::IssueBits;
use codoseo_core::page::{FetchFailure, Indexability, PageFields, PageRecord, indexability};
use codoseo_core::url::{normalize, url_hash};
use url::Url;

use crate::extract::{Link, extract};
use crate::fetch::{FetchError, FetchResult};
use crate::scope::SiteScope;

/// A record plus what the orchestrator needs to continue the crawl from it.
pub(crate) struct Built {
    pub record: PageRecord,
    pub links: Vec<Link>,
    /// False when the page's links are recorded but must not be followed.
    pub follow_links: bool,
}

/// Builds the records for one completed fetch: one, or two when the URL redirected to an
/// internal URL that `admit_target` accepts (the final page's record comes from the same
/// response, so no second fetch is needed).
pub(crate) fn from_fetch(
    url: &Url,
    depth: Option<u16>,
    in_sitemap: impl Fn(&Url) -> bool,
    res: &FetchResult,
    scope: &SiteScope,
    admit_target: impl FnOnce(&Url) -> bool,
) -> Vec<Built> {
    let Some(first_hop) = res.chain.first() else {
        return vec![page(url, depth, in_sitemap(url), res, scope)];
    };

    let target = normalize(&res.final_url, res.final_url.as_str());
    let shown_target = target.clone().unwrap_or_else(|| res.final_url.clone());
    let mut record = base_record(url, depth, in_sitemap(url));
    record.status = first_hop.status;
    record.redirect_chain = res
        .chain
        .iter()
        .map(|h| (h.status, h.url.clone()))
        .collect();
    record.redirect_target = Some(shown_target.clone());
    record.response_ms = res.response_ms;
    record.indexability = indexability(url, record.status, &record.fields, false);
    record.key_hash = record.compute_key_hash();
    let mut built = vec![Built {
        record,
        links: Vec::new(),
        follow_links: false,
    }];

    if shown_target != *url
        && target.is_some()
        && scope.is_internal(&shown_target)
        && admit_target(&shown_target)
    {
        built.push(page(
            &shown_target,
            depth,
            in_sitemap(&shown_target),
            res,
            scope,
        ));
    }
    built
}

/// The record for a final response (no redirect hops of its own).
fn page(
    url: &Url,
    depth: Option<u16>,
    in_sitemap: bool,
    res: &FetchResult,
    scope: &SiteScope,
) -> Built {
    let mut record = base_record(url, depth, in_sitemap);
    record.status = res.status;
    record.response_ms = res.response_ms;
    record.size_bytes = res.size_bytes;
    record.content_type = res.content_type.clone();

    let links = match &res.body {
        Some(body) => {
            let extracted = extract(url, res.content_type.as_deref(), body);
            record.fields = extracted.fields;
            extracted.links
        }
        None => Vec::new(),
    };
    record.fields.x_robots_tag = res.x_robots_tag.clone();
    record.indexability = indexability(url, record.status, &record.fields, false);

    let (mut internal, mut external, mut nofollow) =
        (HashSet::new(), HashSet::new(), HashSet::new());
    for link in &links {
        let hash = url_hash(&link.url);
        if scope.is_internal(&link.url) {
            internal.insert(hash);
            if link.nofollow {
                nofollow.insert(hash);
            }
        } else {
            external.insert(hash);
        }
    }
    record.outlinks_internal = count(&internal);
    record.outlinks_external = count(&external);
    record.outlinks_nofollow = count(&nofollow);

    let follow_links = record.is_html_ok() && !record.fields.is_nofollow();
    record.key_hash = record.compute_key_hash();
    Built {
        record,
        links,
        follow_links,
    }
}

pub(crate) fn from_error(
    url: &Url,
    depth: Option<u16>,
    in_sitemap: bool,
    err: &FetchError,
) -> PageRecord {
    let mut record = base_record(url, depth, in_sitemap);
    let (failure, chain) = match err {
        FetchError::Blocked(_) => (FetchFailure::Blocked, None),
        FetchError::Timeout => (FetchFailure::Timeout, None),
        FetchError::Connect(_) => (FetchFailure::Connect, None),
        FetchError::TooManyRedirects { chain } => (FetchFailure::TooManyRedirects, Some(chain)),
        FetchError::RedirectLoop { chain } => (FetchFailure::RedirectLoop, Some(chain)),
        FetchError::InvalidRedirect { .. } => (FetchFailure::InvalidRedirect, None),
        FetchError::Http(_) | FetchError::Client(_) => (FetchFailure::Other, None),
    };
    if let Some(chain) = chain {
        record.status = chain.first().map_or(0, |h| h.status);
        record.redirect_chain = chain.iter().map(|h| (h.status, h.url.clone())).collect();
    }
    record.error = Some(failure);
    record.indexability = indexability(url, record.status, &record.fields, false);
    record.key_hash = record.compute_key_hash();
    record
}

pub(crate) fn robots_blocked(url: &Url, depth: Option<u16>, in_sitemap: bool) -> PageRecord {
    let mut record = base_record(url, depth, in_sitemap);
    record.indexability = indexability(url, 0, &record.fields, true);
    record.key_hash = record.compute_key_hash();
    record
}

fn base_record(url: &Url, depth: Option<u16>, in_sitemap: bool) -> PageRecord {
    PageRecord {
        url: url.clone(),
        url_hash: url_hash(url),
        status: 0,
        redirect_chain: Vec::new(),
        response_ms: 0,
        size_bytes: 0,
        content_type: None,
        depth,
        in_sitemap,
        indexability: Indexability::ServerError,
        fields: PageFields::default(),
        inlinks: 0,
        outlinks_internal: 0,
        outlinks_external: 0,
        issues: IssueBits::default(),
        key_hash: 0,
        redirect_target: None,
        outlinks_nofollow: 0,
        error: None,
    }
}

fn count(set: &HashSet<u64>) -> u32 {
    u32::try_from(set.len()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use reqwest::header::HeaderMap;

    use super::*;
    use crate::fetch::Hop;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn scope() -> SiteScope {
        SiteScope::new(&u("https://e.com/"))
    }

    fn hop(status: u16, url: &str) -> Hop {
        Hop {
            status,
            url: u(url),
        }
    }

    fn fetch_result(status: u16, content_type: &str, body: &[u8]) -> FetchResult {
        FetchResult {
            final_url: u("https://e.com/"),
            status,
            chain: Vec::new(),
            headers: HeaderMap::new(),
            content_type: Some(content_type.to_owned()),
            x_robots_tag: None,
            response_ms: 42,
            size_bytes: body.len() as u64,
            body: Some(Bytes::copy_from_slice(body)),
            truncated: false,
        }
    }

    fn redirected(hops: Vec<(u16, &str)>, final_url: &str, status: u16) -> FetchResult {
        let mut r = fetch_result(
            status,
            "text/html",
            b"<title>New</title><a href=\"/z\">z</a>",
        );
        r.final_url = u(final_url);
        r.chain = hops.into_iter().map(|(s, url)| hop(s, url)).collect();
        r
    }

    #[test]
    fn counts_unique_outlinks_and_marks_the_page_indexable() {
        let res = fetch_result(
            200,
            "text/html",
            br#"<title>T</title><a href="/x">x</a><a href="/x">again</a><a rel="nofollow" href="/n">n</a><a href="https://other.org/">o</a>"#,
        );
        let b = &from_fetch(
            &u("https://e.com/"),
            Some(0),
            |_| false,
            &res,
            &scope(),
            |_| true,
        )[0];
        assert_eq!(
            (
                b.record.outlinks_internal,
                b.record.outlinks_external,
                b.record.outlinks_nofollow
            ),
            (2, 1, 1)
        );
        assert_eq!(b.record.indexability, Indexability::Indexable);
        assert!(b.follow_links);
        assert_eq!(b.links.len(), 4);
        assert_eq!(b.record.fields.title.as_deref(), Some("T"));
        assert_eq!(b.record.key_hash, b.record.compute_key_hash());
        assert_eq!((b.record.response_ms, b.record.status), (42, 200));
        assert_eq!(
            b.record.url_hash,
            codoseo_core::url::url_hash(&b.record.url)
        );
    }

    #[test]
    fn self_links_count_as_internal() {
        let res = fetch_result(
            200,
            "text/html",
            br#"<a href="/">home</a><a href="/a">a</a>"#,
        );
        let b = &from_fetch(
            &u("https://e.com/"),
            Some(0),
            |_| false,
            &res,
            &scope(),
            |_| true,
        )[0];
        assert_eq!(b.record.outlinks_internal, 2);
    }

    #[test]
    fn nofollow_is_counted_only_for_internal_links() {
        let res = fetch_result(
            200,
            "text/html",
            br#"<a rel="nofollow" href="https://other.org/">o</a><a rel="nofollow" href="/n">n</a>"#,
        );
        let b = &from_fetch(
            &u("https://e.com/"),
            Some(0),
            |_| false,
            &res,
            &scope(),
            |_| true,
        )[0];
        assert_eq!(
            (
                b.record.outlinks_internal,
                b.record.outlinks_external,
                b.record.outlinks_nofollow
            ),
            (1, 1, 1)
        );
    }

    #[test]
    fn meta_nofollow_keeps_links_but_does_not_follow() {
        let nf = fetch_result(
            200,
            "text/html",
            br#"<meta name="robots" content="nofollow"><a href="/x">x</a>"#,
        );
        let b = &from_fetch(
            &u("https://e.com/"),
            Some(0),
            |_| false,
            &nf,
            &scope(),
            |_| true,
        )[0];
        assert!(!b.follow_links);
        assert_eq!(b.links.len(), 1);
        assert_eq!(b.record.outlinks_internal, 1);
    }

    #[test]
    fn x_robots_tag_header_is_copied_and_applies() {
        let mut res = fetch_result(200, "text/html", br#"<a href="/x">x</a>"#);
        res.x_robots_tag = Some("noindex, nofollow".to_owned());
        let b = &from_fetch(
            &u("https://e.com/"),
            Some(0),
            |_| false,
            &res,
            &scope(),
            |_| true,
        )[0];
        assert_eq!(
            b.record.fields.x_robots_tag.as_deref(),
            Some("noindex, nofollow")
        );
        assert_eq!(b.record.indexability, Indexability::Noindex);
        assert!(!b.follow_links);
    }

    #[test]
    fn error_pages_and_non_html_are_not_followed() {
        let not_found = fetch_result(404, "text/html", br#"<a href="/x">x</a>"#);
        let b = &from_fetch(
            &u("https://e.com/m"),
            Some(1),
            |_| false,
            &not_found,
            &scope(),
            |_| true,
        )[0];
        assert!(!b.follow_links);
        assert_eq!(b.record.indexability, Indexability::ClientError);

        let mut pdf = fetch_result(200, "application/pdf", b"");
        pdf.body = None;
        pdf.size_bytes = 1234;
        let b = &from_fetch(
            &u("https://e.com/a.pdf"),
            Some(1),
            |_| false,
            &pdf,
            &scope(),
            |_| true,
        )[0];
        assert!(!b.follow_links);
        assert!(b.links.is_empty());
        assert_eq!(b.record.size_bytes, 1234);
        assert_eq!(b.record.content_type.as_deref(), Some("application/pdf"));
    }

    #[test]
    fn sitemap_membership_is_looked_up_per_url() {
        let res = fetch_result(200, "text/html", b"");
        let b = &from_fetch(
            &u("https://e.com/"),
            None,
            |x| x.path() == "/",
            &res,
            &scope(),
            |_| true,
        )[0];
        assert!(b.record.in_sitemap);
        assert_eq!(b.record.depth, None);
    }

    #[test]
    fn redirect_makes_two_records_at_the_same_depth() {
        let r = redirected(vec![(301, "https://e.com/old")], "https://e.com/new", 200);
        let v = from_fetch(
            &u("https://e.com/old"),
            Some(2),
            |_| false,
            &r,
            &scope(),
            |_| true,
        );
        assert_eq!(v.len(), 2);
        let first = &v[0];
        assert_eq!(
            (first.record.status, first.record.indexability),
            (301, Indexability::Redirected)
        );
        assert_eq!(first.record.redirect_target, Some(u("https://e.com/new")));
        assert_eq!(
            first.record.redirect_chain,
            vec![(301, u("https://e.com/old"))]
        );
        assert_eq!((first.record.size_bytes, first.record.response_ms), (0, 42));
        assert!(first.links.is_empty());
        assert!(!first.follow_links);
        assert_eq!(first.record.fields, PageFields::default());
        assert_eq!(first.record.key_hash, first.record.compute_key_hash());

        let second = &v[1];
        assert_eq!(
            (second.record.url.clone(), second.record.depth),
            (u("https://e.com/new"), Some(2))
        );
        assert_eq!(second.record.status, 200);
        assert!(second.record.redirect_chain.is_empty());
        assert_eq!(second.record.redirect_target, None);
        assert_eq!(second.record.fields.title.as_deref(), Some("New"));
        assert_eq!(second.links.len(), 1);
        assert!(second.follow_links);
        assert_eq!(second.record.key_hash, second.record.compute_key_hash());
    }

    #[test]
    fn multi_hop_chain_keeps_every_hop_and_first_status() {
        let r = redirected(
            vec![(302, "https://e.com/a"), (301, "https://e.com/b")],
            "https://e.com/c",
            200,
        );
        let v = from_fetch(
            &u("https://e.com/a"),
            Some(1),
            |_| false,
            &r,
            &scope(),
            |_| true,
        );
        assert_eq!(v[0].record.status, 302);
        assert_eq!(
            v[0].record.redirect_chain,
            vec![(302, u("https://e.com/a")), (301, u("https://e.com/b"))]
        );
        assert_eq!(v[0].record.redirect_target, Some(u("https://e.com/c")));
    }

    #[test]
    fn redirect_target_not_admitted_gives_one_record() {
        let r = redirected(vec![(301, "https://e.com/old")], "https://e.com/new", 200);
        let v = from_fetch(
            &u("https://e.com/old"),
            Some(2),
            |_| false,
            &r,
            &scope(),
            |_| false,
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].record.redirect_target, Some(u("https://e.com/new")));
    }

    #[test]
    fn external_redirect_target_is_never_offered_to_admit() {
        let r = redirected(vec![(301, "https://e.com/out")], "https://other.org/x", 200);
        let v = from_fetch(
            &u("https://e.com/out"),
            Some(1),
            |_| false,
            &r,
            &scope(),
            |_| panic!("external targets must not be admitted"),
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].record.redirect_target, Some(u("https://other.org/x")));
    }

    #[test]
    fn no_redirect_never_calls_admit() {
        let res = fetch_result(200, "text/html", b"");
        let v = from_fetch(
            &u("https://e.com/"),
            Some(0),
            |_| false,
            &res,
            &scope(),
            |_| panic!("no redirect, nothing to admit"),
        );
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn errors_map_one_to_one() {
        let url = u("https://e.com/t");
        let cases: Vec<(FetchError, FetchFailure)> = vec![
            (FetchError::Timeout, FetchFailure::Timeout),
            (FetchError::Connect("x".into()), FetchFailure::Connect),
            (FetchError::Blocked("x".into()), FetchFailure::Blocked),
            (
                FetchError::InvalidRedirect {
                    location: "x".into(),
                },
                FetchFailure::InvalidRedirect,
            ),
            (FetchError::Http("x".into()), FetchFailure::Other),
            (FetchError::Client("x".into()), FetchFailure::Other),
        ];
        for (err, want) in cases {
            let r = from_error(&url, Some(1), true, &err);
            assert_eq!((r.error, r.status), (Some(want), 0), "{err}");
            assert!(r.in_sitemap);
            assert_eq!(r.indexability, Indexability::ServerError);
            assert!(r.redirect_chain.is_empty());
            assert_eq!(r.key_hash, r.compute_key_hash());
        }
    }

    #[test]
    fn redirect_errors_keep_the_chain_and_first_status() {
        let looped = from_error(
            &u("https://e.com/l"),
            Some(1),
            false,
            &FetchError::RedirectLoop {
                chain: vec![hop(301, "https://e.com/l")],
            },
        );
        assert_eq!(
            (looped.status, looped.error),
            (301, Some(FetchFailure::RedirectLoop))
        );
        assert_eq!(looped.redirect_chain, vec![(301, u("https://e.com/l"))]);

        let many = from_error(
            &u("https://e.com/1"),
            Some(1),
            false,
            &FetchError::TooManyRedirects {
                chain: vec![hop(302, "https://e.com/1"), hop(301, "https://e.com/2")],
            },
        );
        assert_eq!(
            (many.status, many.error),
            (302, Some(FetchFailure::TooManyRedirects))
        );
        assert_eq!(many.redirect_chain.len(), 2);
    }

    #[test]
    fn robots_blocked_record() {
        let r = robots_blocked(&u("https://e.com/p"), Some(1), true);
        assert_eq!(r.indexability, Indexability::BlockedByRobots);
        assert_eq!(
            (r.status, r.error, r.depth, r.in_sitemap),
            (0, None, Some(1), true)
        );
        assert_eq!(r.key_hash, r.compute_key_hash());
    }
}
