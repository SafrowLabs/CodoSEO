use codoseo_core::page::{AiMeta, PageFields, PageRecord};
use codoseo_geo::eligibility::{
    Cause, CauseSource, DirectiveSlug, Effect, Engine, EngineId, engines, page_effect,
    record_effect,
};
use codoseo_geo::registry::registry;

fn engine(id: EngineId) -> &'static Engine {
    engines().iter().find(|e| e.id == id).expect("engine")
}

fn fields(meta: Option<&str>, header: Option<&str>, bot_meta: &[(&str, &str)]) -> PageFields {
    PageFields {
        meta_robots: meta.map(str::to_owned),
        x_robots_tag: header.map(str::to_owned),
        word_count: 100,
        ai: AiMeta {
            bot_meta: bot_meta
                .iter()
                .map(|(n, c)| ((*n).to_owned(), (*c).to_owned()))
                .collect(),
            ..AiMeta::default()
        },
        ..PageFields::default()
    }
}

fn effect(id: EngineId, f: &PageFields) -> Effect {
    page_effect(engine(id), f).0
}

#[test]
fn every_engine_crawler_is_in_the_registry_and_documented() {
    assert_eq!(engines().len(), 10);
    for e in engines() {
        let bot = registry()
            .bot(e.crawler)
            .unwrap_or_else(|| panic!("{}", e.crawler));
        assert!(e.source_url.starts_with("https://"), "{}", e.name);
        assert!(e.meta_scopes.contains(&"robots"));
        assert!(
            e.meta_scopes.iter().all(|s| *s == s.to_ascii_lowercase()),
            "{}",
            e.name
        );
        assert!(
            e.meta_scopes
                .contains(&bot.token.to_ascii_lowercase().as_str())
                || e.page_controls
        );
        assert_eq!(e.note().is_some(), !e.page_controls);
    }
}

#[test]
fn google_directives() {
    let g = EngineId::Google;
    assert_eq!(effect(g, &fields(None, None, &[])), Effect::Eligible);
    for d in [
        "noindex",
        "none",
        "nosnippet",
        "max-snippet:0",
        "NoIndex, follow",
    ] {
        assert_eq!(
            effect(g, &fields(Some(d), None, &[])),
            Effect::Excluded,
            "{d}"
        );
    }
    let (e, causes) = page_effect(engine(g), &fields(Some("max-snippet: 50"), None, &[]));
    assert_eq!(e, Effect::Limited);
    assert_eq!(
        causes,
        [Cause {
            directive: DirectiveSlug::MaxSnippet,
            detail: "max-snippet:50".into(),
            source: CauseSource::Meta {
                name: "robots".into()
            },
            effect: Effect::Limited,
        }]
    );
    // Not Google's controls.
    for d in ["noarchive", "nocache", "nofollow", "max-image-preview:none"] {
        assert_eq!(
            effect(g, &fields(Some(d), None, &[])),
            Effect::Eligible,
            "{d}"
        );
    }
}

#[test]
fn max_snippet_minus_one_is_no_limit_and_junk_is_ignored() {
    let g = EngineId::Google;
    for d in [
        "max-snippet:-1",
        "max-snippet:abc",
        "max-snippet:",
        "max-snippet:-5",
    ] {
        assert_eq!(
            effect(g, &fields(Some(d), None, &[])),
            Effect::Eligible,
            "{d}"
        );
    }
}

#[test]
fn excluded_beats_limited_and_all_causes_are_collected() {
    let f = fields(Some("max-snippet:30, noindex"), Some("nosnippet"), &[]);
    let (e, causes) = page_effect(engine(EngineId::Google), &f);
    assert_eq!(e, Effect::Excluded);
    let kinds: Vec<_> = causes.iter().map(|c| c.directive).collect();
    assert_eq!(
        kinds,
        [
            DirectiveSlug::MaxSnippet,
            DirectiveSlug::Noindex,
            DirectiveSlug::Nosnippet
        ]
    );
    assert_eq!(
        causes[2].source,
        CauseSource::Header {
            scope: "all".into()
        }
    );
}

#[test]
fn bing_directives() {
    let b = EngineId::Bing;
    for d in ["noindex", "none", "noarchive"] {
        assert_eq!(
            effect(b, &fields(Some(d), None, &[])),
            Effect::Excluded,
            "{d}"
        );
    }
    for d in ["nocache", "nosnippet"] {
        assert_eq!(
            effect(b, &fields(Some(d), None, &[])),
            Effect::Limited,
            "{d}"
        );
    }
    assert_eq!(
        effect(b, &fields(Some("max-snippet:0"), None, &[])),
        Effect::Eligible
    );
}

#[test]
fn apple_and_amazon_directives() {
    let a = EngineId::Apple;
    assert_eq!(
        effect(a, &fields(Some("noindex"), None, &[])),
        Effect::Excluded
    );
    assert_eq!(
        effect(a, &fields(Some("none"), None, &[])),
        Effect::Excluded
    );
    assert_eq!(
        effect(a, &fields(Some("nosnippet"), None, &[])),
        Effect::Limited
    );
    assert_eq!(
        effect(a, &fields(Some("noarchive"), None, &[])),
        Effect::Eligible
    );
    let z = EngineId::Amazon;
    for d in ["noindex", "none", "noarchive"] {
        assert_eq!(
            effect(z, &fields(Some(d), None, &[])),
            Effect::Excluded,
            "{d}"
        );
    }
    assert_eq!(
        effect(z, &fields(Some("nosnippet"), None, &[])),
        Effect::Eligible
    );
}

#[test]
fn a_bot_meta_applies_to_its_own_engine_only() {
    let f = fields(None, None, &[("bingbot", "noarchive")]);
    assert_eq!(effect(EngineId::Bing, &f), Effect::Excluded);
    assert_eq!(effect(EngineId::Google, &f), Effect::Eligible);
    assert_eq!(effect(EngineId::Apple, &f), Effect::Eligible);
    let (_, causes) = page_effect(engine(EngineId::Bing), &f);
    assert_eq!(
        causes[0].source,
        CauseSource::Meta {
            name: "bingbot".into()
        }
    );
    // msnbot is Bing's other name.
    let f = fields(None, None, &[("msnbot", "nosnippet")]);
    assert_eq!(effect(EngineId::Bing, &f), Effect::Limited);
    // `<meta name="googlebot">` reaches Google, and not through Bing.
    let f = fields(None, None, &[("googlebot", "noindex")]);
    assert_eq!(effect(EngineId::Google, &f), Effect::Excluded);
    assert_eq!(effect(EngineId::Bing, &f), Effect::Eligible);
}

#[test]
fn scoped_headers_follow_their_agent() {
    let f = fields(None, Some("googlebot: noindex"), &[]);
    assert_eq!(effect(EngineId::Google, &f), Effect::Excluded);
    assert_eq!(effect(EngineId::Bing, &f), Effect::Eligible);
    let (_, causes) = page_effect(engine(EngineId::Google), &f);
    assert_eq!(
        causes[0].source,
        CauseSource::Header {
            scope: "googlebot".into()
        }
    );
    let f = fields(None, Some("bingbot: noarchive, nosnippet"), &[]);
    assert_eq!(effect(EngineId::Bing, &f), Effect::Excluded);
    assert_eq!(effect(EngineId::Google, &f), Effect::Eligible);
    // A directive before any prefix reaches everyone, one after a prefix only its agent.
    let f = fields(None, Some("noarchive, googlebot: nosnippet"), &[]);
    assert_eq!(effect(EngineId::Bing, &f), Effect::Excluded);
    assert_eq!(effect(EngineId::Amazon, &f), Effect::Excluded);
    assert_eq!(effect(EngineId::Apple, &f), Effect::Eligible);
    assert_eq!(effect(EngineId::Google, &f), Effect::Excluded);
}

#[test]
fn data_nosnippet_needs_a_quarter_of_the_words() {
    let with = |hidden: u32, total: u32| PageFields {
        word_count: total,
        ai: AiMeta {
            nosnippet_words: hidden,
            ..AiMeta::default()
        },
        ..PageFields::default()
    };
    for id in [EngineId::Google, EngineId::Bing] {
        assert_eq!(effect(id, &with(24, 100)), Effect::Eligible);
        assert_eq!(effect(id, &with(25, 100)), Effect::Limited);
        assert_eq!(effect(id, &with(0, 0)), Effect::Eligible);
    }
    let (e, causes) = page_effect(engine(EngineId::Google), &with(62, 100));
    assert_eq!(e, Effect::Limited);
    assert_eq!(causes[0].directive, DirectiveSlug::DataNosnippet);
    assert_eq!(causes[0].detail, "62% of words");
    assert_eq!(causes[0].source, CauseSource::Attribute);
    // More hidden words than counted words (odd markup) reads as everything, not above.
    assert_eq!(
        page_effect(engine(EngineId::Google), &with(30, 10)).1[0].detail,
        "100% of words"
    );
    // Apple and Amazon document no such control.
    assert_eq!(effect(EngineId::Apple, &with(90, 100)), Effect::Eligible);
    assert_eq!(effect(EngineId::Amazon, &with(90, 100)), Effect::Eligible);
}

#[test]
fn robots_only_engines_have_no_page_effect() {
    let f = fields(
        Some("noindex, nosnippet"),
        Some("none"),
        &[("claude-searchbot", "noindex")],
    );
    for id in [
        EngineId::ChatGpt,
        EngineId::Claude,
        EngineId::Perplexity,
        EngineId::MetaAi,
        EngineId::DuckDuckGo,
        EngineId::Mistral,
    ] {
        let e = engine(id);
        assert!(!e.page_controls);
        assert_eq!(page_effect(e, &f), (Effect::Eligible, Vec::new()));
        assert!(e.note().is_some());
    }
}

#[test]
fn only_successful_html_pages_have_an_effect() {
    let mut r = PageRecord {
        url: "https://e.com/a".parse().unwrap(),
        url_hash: 1,
        status: 200,
        redirect_chain: Vec::new(),
        response_ms: 1,
        size_bytes: 1,
        content_type: Some("text/html".into()),
        depth: Some(0),
        in_sitemap: false,
        indexability: codoseo_core::page::Indexability::Noindex,
        fields: fields(Some("noindex"), None, &[]),
        inlinks: 0,
        outlinks_internal: 0,
        outlinks_external: 0,
        issues: Default::default(),
        key_hash: 0,
        redirect_target: None,
        outlinks_nofollow: 0,
        error: None,
    };
    let g = engine(EngineId::Google);
    assert_eq!(record_effect(g, &r).map(|(e, _)| e), Some(Effect::Excluded));
    r.status = 404;
    assert_eq!(record_effect(g, &r), None);
    r.status = 200;
    r.content_type = Some("application/pdf".into());
    assert_eq!(record_effect(g, &r), None);
}

#[test]
fn old_audit_json_without_ai_still_loads() {
    let f: PageFields = serde_json::from_str(
        r#"{"title":null,"title_count":0,"meta_description":null,"meta_robots":null,"x_robots_tag":null,
        "canonical":null,"hreflang":[],"h1":[],"h2":[],"word_count":0,"content_hash":0,
        "images_missing_alt":0,"og":{"title":null,"description":null,"image":null},
        "jsonld":"absent","mixed_content":0}"#,
    )
    .expect("loads");
    assert!(f.ai.is_empty());
}
