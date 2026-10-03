#![allow(dead_code)]

use codoseo_core::Url;
use codoseo_core::check::IssueBits;
use codoseo_core::page::{Indexability, PageFields, PageRecord};
use codoseo_core::url::url_hash;

pub fn sample_record() -> PageRecord {
    let url = Url::parse("https://e.com/a").unwrap();
    PageRecord {
        url_hash: url_hash(&url),
        url,
        status: 200,
        redirect_chain: Vec::new(),
        response_ms: 120,
        size_bytes: 2048,
        content_type: Some("text/html; charset=utf-8".into()),
        depth: Some(1),
        in_sitemap: false,
        indexability: Indexability::Indexable,
        fields: PageFields {
            title: Some("Title".into()),
            h1: vec!["Heading".into()],
            ..Default::default()
        },
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
