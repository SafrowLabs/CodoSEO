mod common;

use codoseo_core::Url;
use codoseo_core::page::{Indexability, PageFields, indexability, is_nofollow, is_noindex};
use common::sample_record;

#[test]
fn robots_directives() {
    assert!(is_noindex(Some("NoIndex, follow"), None));
    assert!(is_noindex(None, Some("none")));
    assert!(is_noindex(None, Some("googlebot: noindex")));
    assert!(!is_noindex(None, Some("bingbot: noindex"))); // scoped to another bot
    assert!(is_noindex(None, Some("codoseobot: noindex")));
    assert!(!is_noindex(Some("index, follow"), None));
    assert!(is_nofollow(Some("noindex,nofollow"), None));
    assert!(is_nofollow(None, Some("none")));
    assert!(!is_nofollow(Some("index"), None));
}

#[test]
fn indexability_rules() {
    let u = Url::parse("https://e.com/a").unwrap();
    let f = PageFields::default();
    assert_eq!(indexability(&u, 200, &f, false), Indexability::Indexable);
    assert_eq!(
        indexability(&u, 200, &f, true),
        Indexability::BlockedByRobots
    );
    assert_eq!(indexability(&u, 301, &f, false), Indexability::Redirected);
    assert_eq!(indexability(&u, 404, &f, false), Indexability::ClientError);
    assert_eq!(indexability(&u, 0, &f, false), Indexability::ServerError);
    assert_eq!(indexability(&u, 503, &f, false), Indexability::ServerError);
    assert_eq!(indexability(&u, 100, &f, false), Indexability::ServerError);
    let ni = PageFields {
        meta_robots: Some("noindex".into()),
        ..Default::default()
    };
    assert_eq!(indexability(&u, 200, &ni, false), Indexability::Noindex);
    let header_ni = PageFields {
        x_robots_tag: Some("noindex".into()),
        ..Default::default()
    };
    assert_eq!(
        indexability(&u, 200, &header_ni, false),
        Indexability::Noindex
    );
    let canon = PageFields {
        canonical: Some(Url::parse("https://e.com/b").unwrap()),
        ..Default::default()
    };
    assert_eq!(
        indexability(&u, 200, &canon, false),
        Indexability::Canonicalised
    );
    let self_canon = PageFields {
        canonical: Some(u.clone()),
        ..Default::default()
    };
    assert_eq!(
        indexability(&u, 200, &self_canon, false),
        Indexability::Indexable
    );
}

#[test]
fn key_hash_tracks_content_not_timing() {
    let mut p = sample_record();
    let k = p.compute_key_hash();
    p.response_ms += 500;
    assert_eq!(p.compute_key_hash(), k);
    p.fields.title = Some("Other".into());
    assert_ne!(p.compute_key_hash(), k);

    let mut q = sample_record();
    q.in_sitemap = true;
    assert_ne!(q.compute_key_hash(), k);
    let mut r = sample_record();
    r.redirect_target = Some(Url::parse("https://e.com/z").unwrap());
    assert_ne!(r.compute_key_hash(), k);
}

#[test]
fn key_hash_separates_adjacent_fields() {
    // "ab" + "" must not hash like "a" + "b".
    let mut a = sample_record();
    a.fields.title = Some("ab".into());
    a.fields.meta_description = None;
    let mut b = sample_record();
    b.fields.title = Some("a".into());
    b.fields.meta_description = Some("b".into());
    assert_ne!(a.compute_key_hash(), b.compute_key_hash());
}

#[test]
fn html_ok_rules() {
    assert!(sample_record().is_html_ok());
    let mut p = sample_record();
    p.content_type = None;
    assert!(p.is_html_ok());
    p.content_type = Some("application/pdf".into());
    assert!(!p.is_html_ok());
    let mut p = sample_record();
    p.status = 404;
    assert!(!p.is_html_ok());
    let mut p = sample_record();
    p.error = Some(codoseo_core::page::FetchFailure::Timeout);
    assert!(!p.is_html_ok());
}
