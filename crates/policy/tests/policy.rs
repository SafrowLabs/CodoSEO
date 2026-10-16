//! The policy format (TOML and JSON, both ways), its validation and the AI rules' evaluation.

use std::collections::HashSet;

use codoseo_core::check::{CheckId, IssueBits};
use codoseo_core::crawl::{RobotsFile, SitemapSummary};
use codoseo_core::output::{CrawlOutput, LinkGraph, SiteSignals, StopReason};
use codoseo_core::page::{Indexability, PageFields, PageRecord};
use codoseo_core::url::url_hash;
use codoseo_geo::eligibility::DirectiveSlug;
use codoseo_geo::report::{AccessReport, build_report, important_urls};
use codoseo_geo::{Purpose, Stance};
use codoseo_policy::{
    Address, Exception, HostPref, IndexRule, IndexStance, PathPattern, PolicyInput, RuleId,
    SitePolicy, Slash, Status, Thresholds, evaluate, parse_file,
};
use proptest::prelude::*;
use url::Url;

/// The example from the design: every key set.
const FULL: &str = r#"
[policy]
version = 1
[policy.seo.address]
host = "apex"
trailing_slash = "without"
[[policy.seo.index]]
pattern = "/checkout/*"
stance = "keep_out"
[policy.seo]
index_default = "index"
ignore_params = ["utm_*", "gclid", "fbclid", "sort", "ref"]
pagination_param = "page"
must_work = ["/", "/pricing", "/checkout/cart"]
[policy.seo.thresholds]
title_max = 65
thin_words = 120
[[policy.seo.exceptions]]
check = "thin_content"
scope = "/stores/*"
note = "Store pages are short by design"
[policy.ai]
purposes = { search = "allow", user_fetch = "allow", training = "block" }
bots = { "Perplexity-User" = "block" }
accepted_directives = ["max_snippet", "data_nosnippet"]
probes = true
"#;

#[test]
fn the_design_example_parses() {
    let parsed = parse_file(FULL).unwrap();
    assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    let p = parsed.policy;
    assert_eq!(
        p.seo.address,
        Some(Address {
            host: HostPref::Apex,
            trailing_slash: Slash::Without
        })
    );
    assert_eq!(p.seo.index[0].stance, IndexStance::KeepOut);
    assert_eq!(p.seo.index_default, IndexStance::Index);
    assert_eq!(p.seo.thresholds.effective().title_max, 65);
    assert_eq!(p.seo.thresholds.effective().title_min, 30);
    assert_eq!(p.seo.exceptions[0].check, CheckId::ThinContent);
    assert_eq!(p.ai.intent.purpose_stance(Purpose::Training), Stance::Block);
    assert_eq!(
        p.ai.intent.bot_override("perplexity-user"),
        Some(Stance::Block)
    );
    assert!(
        p.ai.intent
            .accepted_directives
            .contains(&DirectiveSlug::MaxSnippet)
    );
    assert!(p.ai.probes);
}

#[test]
fn toml_and_json_round_trip_the_full_example() {
    let p = parse_file(FULL).unwrap().policy;
    let toml = p.to_toml();
    let back = parse_file(&toml).unwrap();
    assert_eq!(back.policy, p, "{toml}");
    let json = p.to_json();
    assert_eq!(
        serde_json::from_value::<SitePolicy>(json.clone()).unwrap(),
        p
    );
    assert_eq!(SitePolicy::from_json_lenient(json), p);
}

#[test]
fn the_ai_keys_keep_the_intent_shape() {
    let p = parse_file(FULL).unwrap().policy;
    let ai = p.to_json()["ai"].clone();
    // What was in `sites.ai_intent`, plus `probes`.
    let intent: codoseo_geo::Intent = serde_json::from_value(ai.clone()).unwrap();
    assert_eq!(intent, p.ai.intent);
    assert_eq!(ai["probes"], true);
    assert_eq!(ai["purposes"]["training"], "block");
}

#[test]
fn unknown_keys_are_warnings_not_errors() {
    let file = "[policy]\nversion = 1\nfuture = 3\n[policy.seo.tresholds]\ntitle_max = 70\n[policy.ai]\nprobes = false\nbotz = {}\n[other]\nx = 1\n";
    let parsed = parse_file(file).unwrap();
    assert!(!parsed.policy.ai.probes);
    let mut w = parsed.warnings;
    w.sort();
    assert_eq!(
        w,
        ["policy.ai.botz", "policy.future", "policy.seo.tresholds"]
    );
}

#[test]
fn the_database_reads_leniently() {
    // A section a newer release wrote that this one can't read falls back to its defaults; the
    // rest survives.
    let v = serde_json::json!({
        "version": 1,
        "seo": { "index": [{ "pattern": "/x/*", "stance": "someday" }] },
        "ai": { "purposes": { "training": "block" }, "probes": false, "new_key": 1 }
    });
    let p = SitePolicy::from_json_lenient(v);
    assert!(p.seo.index.is_empty());
    assert_eq!(p.ai.intent.purpose_stance(Purpose::Training), Stance::Block);
    assert!(!p.ai.probes);
    // `{}` and an empty `ai` are the defaults.
    assert_eq!(
        SitePolicy::from_json_lenient(serde_json::json!({})),
        SitePolicy::default()
    );
}

fn errors(file: &str) -> Vec<String> {
    parse_file(file)
        .unwrap_err()
        .into_iter()
        .map(|e| e.to_string())
        .collect()
}

#[test]
fn validation_names_each_problem() {
    let e = errors(
        "[policy]\nversion = 2\n[[policy.seo.index]]\npattern = \"checkout/*\"\nstance = \"index\"\n\
         [[policy.seo.index]]\npattern = \"/a\"\nstance = \"index\"\n[[policy.seo.index]]\npattern = \"/a\"\nstance = \"keep_out\"\n\
         [policy.seo]\nignore_params = [\"a=b\"]\nmust_work = [\"pricing\"]\n\
         [policy.seo.thresholds]\ntitle_min = 70\ntitle_max = 60\nslow_ms = 5\n\
         [policy.ai]\naccepted_directives = [\"noindex\"]\n",
    );
    let joined = e.join("\n");
    assert!(
        joined.contains("version: version 2 is not supported"),
        "{joined}"
    );
    assert!(
        joined.contains("seo.index[0].pattern: a pattern starts with /"),
        "{joined}"
    );
    assert!(
        joined.contains("seo.index[2].pattern: \"/a\" is listed twice"),
        "{joined}"
    );
    assert!(joined.contains("seo.ignore_params[0]"), "{joined}");
    assert!(joined.contains("seo.must_work[0]"), "{joined}");
    assert!(
        joined.contains("title_min (70) must be below title_max (60)"),
        "{joined}"
    );
    assert!(
        joined.contains("seo.thresholds.slow_ms: 5 is out of range"),
        "{joined}"
    );
    assert!(
        joined.contains("ai.accepted_directives: noindex can't be accepted"),
        "{joined}"
    );
}

#[test]
fn syntax_and_type_errors_carry_the_parsers_position() {
    let e = errors("[policy\nversion = 1\n");
    assert!(e[0].contains("line 1"), "{e:?}");
    let e = errors("[policy]\n[policy.ai]\npurposes = { search = \"maybe\" }\n");
    assert!(e[0].contains("maybe"), "{e:?}");
    let e = errors("[other]\nx = 1\n");
    assert!(e[0].contains("no [policy] table"), "{e:?}");
}

// ---- round trip, generated -------------------------------------------------------------------

fn stance() -> impl Strategy<Value = Stance> {
    prop_oneof![Just(Stance::Allow), Just(Stance::Block), Just(Stance::Any)]
}

fn purpose() -> impl Strategy<Value = Purpose> {
    prop_oneof![
        Just(Purpose::Search),
        Just(Purpose::UserFetch),
        Just(Purpose::Agent),
        Just(Purpose::Training),
        Just(Purpose::Ads),
    ]
}

fn directive() -> impl Strategy<Value = DirectiveSlug> {
    prop_oneof![
        Just(DirectiveSlug::Nosnippet),
        Just(DirectiveSlug::MaxSnippet),
        Just(DirectiveSlug::Noarchive),
        Just(DirectiveSlug::Nocache),
        Just(DirectiveSlug::DataNosnippet),
    ]
}

fn index_stance() -> impl Strategy<Value = IndexStance> {
    prop_oneof![
        Just(IndexStance::Index),
        Just(IndexStance::KeepOut),
        Just(IndexStance::None)
    ]
}

prop_compose! {
    fn policy()(
        purposes in prop::collection::btree_map(purpose(), stance(), 0..5),
        bots in prop::collection::btree_map("[A-Za-z][A-Za-z-]{0,15}", stance(), 0..4),
        accepted in prop::collection::btree_set(directive(), 0..5),
        probes in any::<bool>(),
        address in prop::option::of((prop_oneof![Just(HostPref::Apex), Just(HostPref::Www)],
            prop_oneof![Just(Slash::Without), Just(Slash::With), Just(Slash::Either)])),
        index in prop::collection::vec(("/[a-z*]{0,8}", index_stance()), 0..4),
        index_default in index_stance(),
        params in prop::collection::vec("[a-z_*]{1,8}", 0..4),
        pagination in prop::option::of("[a-z]{1,6}"),
        must_work in prop::collection::vec("/[a-z]{0,8}", 0..4),
        title_max in prop::option::of(61u16..200),
        thin in prop::option::of(1u32..5000),
        alt in prop::option::of(any::<bool>()),
        note in "[ a-zA-Z]{0,20}",
    ) -> SitePolicy {
        let mut p = SitePolicy::default();
        p.ai.intent.purposes = purposes;
        p.ai.intent.bots = bots;
        p.ai.intent.accepted_directives = accepted;
        p.ai.probes = probes;
        p.seo.address = address.map(|(host, trailing_slash)| Address { host, trailing_slash });
        p.seo.index = index.into_iter().map(|(pattern, stance)| IndexRule { pattern: PathPattern(pattern), stance }).collect();
        p.seo.index_default = index_default;
        p.seo.ignore_params = params;
        p.seo.pagination_param = pagination;
        p.seo.must_work = must_work;
        p.seo.thresholds = Thresholds { title_max, thin_words: thin, image_alt: alt, ..Thresholds::default() };
        p.seo.exceptions = vec![Exception { check: CheckId::TitleTooLong, scope: PathPattern("/blog/*".into()), note }];
        p
    }
}

proptest! {
    #[test]
    fn any_policy_round_trips_through_toml_and_json(p in policy()) {
        let toml = p.to_toml();
        let de = toml::Deserializer::parse(&toml).unwrap();
        #[derive(serde::Deserialize)]
        struct File { policy: SitePolicy }
        let back: File = serde::Deserialize::deserialize(de).unwrap();
        prop_assert_eq!(&back.policy, &p);
        let json: SitePolicy = serde_json::from_value(p.to_json()).unwrap();
        prop_assert_eq!(json, p);
    }
}

// ---- evaluation ------------------------------------------------------------------------------

fn page(path: &str) -> PageRecord {
    let url = Url::parse(&format!("https://example.com{path}")).unwrap();
    PageRecord {
        url_hash: url_hash(&url),
        url,
        status: 200,
        redirect_chain: Vec::new(),
        response_ms: 10,
        size_bytes: 1000,
        content_type: Some("text/html; charset=utf-8".to_owned()),
        depth: Some(1),
        in_sitemap: false,
        indexability: Indexability::Indexable,
        fields: PageFields {
            word_count: 100,
            ..PageFields::default()
        },
        inlinks: 1,
        outlinks_internal: 0,
        outlinks_external: 0,
        issues: IssueBits::default(),
        key_hash: 0,
        redirect_target: None,
        outlinks_nofollow: 0,
        error: None,
    }
}

fn report(robots: &str) -> AccessReport {
    let out = CrawlOutput {
        origin: Url::parse("https://example.com/").unwrap(),
        pages: ["/", "/a"].iter().map(|p| page(p)).collect(),
        links: LinkGraph::default(),
        robots: Some(RobotsFile {
            status: 200,
            body: robots.to_owned(),
            hash: 1,
        }),
        sitemap: SitemapSummary::default(),
        stop: StopReason::Completed,
        duration_ms: 1,
        signals: SiteSignals::default(),
    };
    let important = important_urls(&out.pages, &out.origin, &HashSet::new());
    build_report(&out, &important)
}

#[test]
fn ai_rules_read_the_findings_by_purpose() {
    let r = report("User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /\n");
    let mut p = SitePolicy::default();
    p.ai.intent
        .purposes
        .insert(Purpose::Training, Stance::Block);
    let rep = evaluate(&p, &PolicyInput::ai_only(Some(&r)));

    let search = rep.rule(RuleId::AiSearch).unwrap();
    assert_eq!(search.status, Status::Gaps);
    assert_eq!(search.gaps, 1);
    assert!(
        search
            .text
            .contains("OAI-SearchBot is blocked by robots.txt"),
        "{}",
        search.text
    );

    let training = rep.rule(RuleId::AiTraining).unwrap();
    assert_eq!(training.status, Status::Gaps, "{}", training.text);
    assert!(training.text.contains("gets in"), "{}", training.text);

    // No preference and no exception: not checked.
    assert_eq!(rep.rule(RuleId::AiAds).unwrap().status, Status::NotChecked);
    assert_eq!(rep.ai_gaps as usize, rep.gaps.len());
    assert_eq!(rep.seo_gaps, 0);
    assert!(
        rep.gaps
            .iter()
            .any(|g| g.subject.as_deref() == Some("OAI-SearchBot"))
    );
}

#[test]
fn a_met_rule_and_the_no_report_case() {
    let r = report("User-agent: *\nAllow: /\n");
    let rep = evaluate(&SitePolicy::default(), &PolicyInput::ai_only(Some(&r)));
    assert_eq!(rep.rule(RuleId::AiSearch).unwrap().status, Status::Met);
    assert_eq!(rep.ai_gaps, 0);
    let none = evaluate(&SitePolicy::default(), &PolicyInput::ai_only(None));
    assert!(none.rules.iter().all(|r| r.status == Status::NotChecked));
}

#[test]
fn blocking_bots_that_ignore_robots_has_limited_effect() {
    let r = report("User-agent: *\nDisallow: /\n");
    let mut p = SitePolicy::default();
    p.ai.intent
        .purposes
        .insert(Purpose::UserFetch, Stance::Block);
    let rep = evaluate(&p, &PolicyInput::ai_only(Some(&r)));
    let user = rep.rule(RuleId::AiUserFetch).unwrap();
    // Only when the registry lists a user fetcher that doesn't follow robots.txt.
    let ignores = codoseo_geo::registry()
        .bots
        .iter()
        .any(|b| b.purpose == Purpose::UserFetch && b.honours_robots == codoseo_geo::Honours::No);
    if ignores {
        assert_eq!(user.status, Status::LimitedEffect, "{}", user.text);
        assert!(user.text.contains("firewall"));
    } else {
        assert_eq!(user.status, Status::Met);
    }
}
