mod common;

use codoseo_core::Url;
use codoseo_core::page::{
    AiMeta, Indexability, PageFields, directives_for, indexability, is_nofollow, is_noindex,
};
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
fn agent_prefix_scopes_the_directives_after_it() {
    // A prefix for another bot scopes every directive after it.
    let other = Some("otherbot: noindex, nofollow");
    assert!(!is_noindex(other, None) && !is_nofollow(other, None));
    assert!(!is_noindex(Some("bingbot: nofollow, noindex"), None));
    assert!(!is_noindex(None, Some("bingbot: nofollow, noindex")));
    // A later prefix replaces the scope.
    let mixed = Some("googlebot: noindex, bingbot: nofollow");
    assert!(is_noindex(mixed, None) && !is_nofollow(mixed, None));
    assert!(is_noindex(None, mixed) && !is_nofollow(None, mixed));
    // Directives before any prefix apply to everyone.
    let lead = Some("noindex, otherbot: nofollow");
    assert!(is_noindex(lead, None) && !is_nofollow(lead, None));
    // Our own scopes apply to everything that follows.
    assert!(is_nofollow(Some("codoseobot: index, nofollow"), None));
    assert!(is_noindex(Some("googlebot: index, noindex"), None));
}

#[test]
fn value_directives_are_not_agent_prefixes() {
    assert!(is_noindex(
        Some("unavailable_after: 25 Jun 2010 15:00:00 PST, noindex"),
        None
    ));
    assert!(is_nofollow(Some("max-snippet: 20, nofollow"), None));
    assert!(is_nofollow(
        None,
        Some("Max-Image-Preview: large, nofollow")
    ));
    assert!(is_noindex(Some("max-video-preview: 5, noindex"), None));
    // Still scoped when they follow another bot's prefix.
    assert!(!is_noindex(
        Some("otherbot: max-snippet: 20, noindex"),
        None
    ));
}

#[test]
fn indexability_status_boundaries() {
    let u = Url::parse("https://e.com/a").unwrap();
    let f = PageFields::default();
    let at = |status| indexability(&u, status, &f, false);
    assert_eq!(at(199), Indexability::ServerError);
    assert_eq!(at(200), Indexability::Indexable);
    assert_eq!(at(299), Indexability::Indexable);
    assert_eq!(at(300), Indexability::Redirected);
    assert_eq!(at(399), Indexability::Redirected);
    assert_eq!(at(400), Indexability::ClientError);
    assert_eq!(at(499), Indexability::ClientError);
    assert_eq!(at(500), Indexability::ServerError);
    assert_eq!(at(599), Indexability::ServerError);
    assert_eq!(at(600), Indexability::ServerError);
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

#[test]
fn key_hash_is_stable_when_ai_meta_is_empty() {
    // Values computed with the hasher before `AiMeta` existed: a deploy must not turn every
    // stored page into a change.
    assert_eq!(sample_record().compute_key_hash(), 8758331034592253484);
    let mut p = sample_record();
    p.fields.meta_robots = Some("noindex, nofollow".into());
    p.fields.x_robots_tag = Some("googlebot: nosnippet".into());
    p.in_sitemap = true;
    assert_eq!(p.compute_key_hash(), 1952386060705738542);
    // Only the bot-named metas feed the hash; the other AI fields do not.
    p.fields.ai.nosnippet_words = 40;
    p.fields.ai.tdm_reservation = Some("1".into());
    assert_eq!(p.compute_key_hash(), 1952386060705738542);
    p.fields.ai.bot_meta = vec![("bingbot".into(), "noarchive".into())];
    let with_meta = p.compute_key_hash();
    assert_ne!(with_meta, 1952386060705738542);
    p.fields.ai.bot_meta = vec![("bingbot".into(), "nosnippet".into())];
    assert_ne!(p.compute_key_hash(), with_meta);
}

#[test]
fn bot_named_metas_count_for_noindex_and_nofollow() {
    let meta = |name: &str, content: &str| PageFields {
        ai: AiMeta {
            bot_meta: vec![(name.into(), content.into())],
            ..AiMeta::default()
        },
        ..PageFields::default()
    };
    assert!(meta("googlebot", "noindex").is_noindex());
    assert!(meta("codoseobot", "none").is_noindex());
    assert!(meta("googlebot", "nofollow").is_nofollow());
    assert!(!meta("bingbot", "noindex").is_noindex());
    assert!(!meta("googlebot", "nosnippet").is_noindex());
    // Combined with the plain robots meta.
    let mut f = meta("googlebot", "index");
    f.meta_robots = Some("noindex".into());
    assert!(f.is_noindex());
    let u = Url::parse("https://e.com/a").unwrap();
    assert_eq!(
        indexability(&u, 200, &meta("googlebot", "noindex"), false),
        Indexability::Noindex
    );
    assert_eq!(
        indexability(&u, 200, &meta("bingbot", "noindex"), false),
        Indexability::Indexable
    );
}

#[test]
fn directives_for_scopes_and_keeps_value_directives() {
    let d = |v: &str, scopes: &[&str]| directives_for(Some(v), scopes);
    assert_eq!(
        d(
            "noarchive, bingbot: nosnippet, max-snippet: 50",
            &["robots", "bingbot"]
        ),
        ["noarchive", "nosnippet", "max-snippet:50"]
    );
    // An unprefixed directive applies to everyone; a prefix scopes what follows it.
    assert_eq!(
        d(
            "NoIndex, googlebot: nosnippet, bingbot: noarchive",
            &["googlebot"]
        ),
        ["noindex", "nosnippet"]
    );
    assert_eq!(d("googlebot: noindex", &["bingbot"]), Vec::<String>::new());
    assert!(directives_for(None, &["googlebot"]).is_empty());
    assert!(d(" , ,", &["googlebot"]).is_empty());
}

#[test]
fn a_date_after_unavailable_after_is_not_an_agent_prefix() {
    let d = directives_for(
        Some("unavailable_after: Friday, 25-Aug-2010 15:00:00 PST, noindex"),
        &["robots"],
    );
    assert!(d.contains(&"noindex".to_owned()), "{d:?}");
}
