mod common;

use std::collections::HashSet;

use codoseo_core::output::CrawlOutput;
use codoseo_core::snapshot::Snapshot;
use codoseo_core::url::url_hash;
use codoseo_geo::eligibility::{DirectiveSlug, Effect, EngineId};
use codoseo_geo::registry::registry;
use codoseo_geo::report::{
    AccessReport, MAX_IMPORTANT, REPORT_VERSION, Reason, build_report, important_urls,
};
use codoseo_geo::robots::{GroupMatch, RobotsAvailability};
use common::*;

fn report_for(out: &CrawlOutput) -> AccessReport {
    let important = important_urls(&out.pages, &out.origin, &HashSet::new());
    build_report(out, &important)
}

#[test]
fn important_urls_agree_with_key_pages() {
    // 30 pages: ties on inlinks, the origin is not the most linked, and two starred pages, one of
    // which is also a top page.
    let mut pages = vec![page("/", 3)];
    for i in 0..29u32 {
        pages.push(page(&format!("/p{i:02}"), 30 - i / 3));
    }
    let origin = url("/");
    let starred: HashSet<u64> = [url("/p28"), url("/p01")].iter().map(url_hash).collect();
    let out = crawl(pages);
    let important = important_urls(&out.pages, &origin, &starred);

    let snap = Snapshot::from_output(&out);
    let key = codoseo_diff::key_pages(&snap, &starred);
    let ours: HashSet<u64> = important
        .iter()
        .map(|i| url_hash(&url::Url::parse(&i.url).expect("url")))
        .collect();
    assert_eq!(ours, key);
    assert_eq!(important[0].url, ORIGIN);
    assert_eq!(important[0].reason, Reason::Home);
    // Ties are broken by URL: p00, p01, p02 share the top rank.
    assert_eq!(important[1].url, "https://example.com/p00");
    assert!(important.iter().any(|i| i.reason == Reason::Starred));
    assert!(important.len() <= MAX_IMPORTANT);
}

#[test]
fn an_origin_missing_from_the_crawl_is_listed_without_a_status() {
    let out = crawl(vec![page("/a", 2), page("/b", 1)]);
    let important = important_urls(&out.pages, &out.origin, &HashSet::new());
    assert_eq!(important[0].url, ORIGIN);
    assert_eq!(important[0].status, None);
    assert!(!important[0].html_ok);
    let key = codoseo_diff::key_pages(&Snapshot::from_output(&out), &HashSet::new());
    let ours: HashSet<u64> = important
        .iter()
        .map(|i| url_hash(&url::Url::parse(&i.url).expect("url")))
        .collect();
    assert_eq!(ours, key);
}

#[test]
fn off_origin_pages_are_not_important() {
    let mut out = site(3, |p| p);
    let mut other = page("/x", 99);
    other.url = url::Url::parse("https://other.example/x").expect("url");
    out.pages.push(other);
    let important = important_urls(&out.pages, &out.origin, &HashSet::new());
    assert!(
        important
            .iter()
            .all(|i| i.url.starts_with("https://example.com/"))
    );
}

#[test]
fn verdicts_cover_every_registry_bot_including_control_tokens() {
    let out = with_robots(
        site(3, |p| p),
        200,
        "User-agent: Google-Extended\nDisallow: /\n\nUser-agent: *\nDisallow: /p1\n",
    );
    let r = report_for(&out);
    assert_eq!(r.version, REPORT_VERSION);
    assert_eq!(r.robots.availability, RobotsAvailability::Ok);
    assert_eq!(r.bots.len(), registry().bots.len());
    let ge = r.bot("Google-Extended").expect("control token");
    assert_eq!(ge.home_allowed, Some(false));
    assert_eq!(ge.group, GroupMatch::Named);
    assert_eq!(ge.blocked.len(), 3);
    let gpt = r.bot("GPTBot").expect("gptbot");
    assert_eq!(gpt.home_allowed, Some(true));
    assert_eq!(gpt.group, GroupMatch::Wildcard);
    assert_eq!(gpt.blocked.len(), 1);
    let p1 = r
        .important
        .iter()
        .position(|i| i.url.ends_with("/p1"))
        .expect("p1");
    assert_eq!(usize::from(gpt.blocked[0].url), p1);
    assert_eq!(r.rule_of(gpt, &gpt.blocked[0]).expect("rule").line, 5);
}

#[test]
fn a_missing_robots_file_allows_everything() {
    let out = with_robots(site(3, |p| p), 404, "");
    let r = report_for(&out);
    assert_eq!(r.robots.availability, RobotsAvailability::Missing);
    assert!(
        r.bots
            .iter()
            .all(|b| b.home_allowed == Some(true) && b.blocked.is_empty())
    );
    assert!(r.bots.iter().all(|b| b.group == GroupMatch::None));
}

#[test]
fn an_unavailable_or_unfetched_robots_file_has_no_verdicts() {
    let r = report_for(&with_robots(site(3, |p| p), 503, "oops"));
    assert_eq!(r.robots.availability, RobotsAvailability::Unavailable);
    assert!(r.bots.is_empty());
    let mut out = site(3, |p| p);
    out.robots = None;
    let r = report_for(&out);
    assert_eq!(r.robots.availability, RobotsAvailability::Unknown);
    assert!(r.bots.is_empty());
}

#[test]
fn engines_use_page_markup_and_the_crawlers_own_robots_verdict() {
    let out = with_robots(
        site(4, |p| p.robots_meta("nosnippet")),
        200,
        "User-agent: Bingbot\nDisallow: /p1\n",
    );
    let r = report_for(&out);
    let google = r.engine(EngineId::Google).expect("google");
    assert_eq!(google.html_pages, 4);
    assert_eq!(google.affected(), 4);
    assert_eq!(google.groups.len(), 1, "a site-wide directive is one group");
    assert!(
        google
            .groups
            .iter()
            .all(|g| g.effect == Effect::Excluded && !g.robots_blocked)
    );
    assert_eq!(google.site.pages, 4);
    assert_eq!(google.site.excluded, 4);
    assert_eq!(google.site.by_cause, vec![(DirectiveSlug::Nosnippet, 4)]);
    let bing = r.engine(EngineId::Bing).expect("bing");
    let blocked: Vec<_> = bing.groups.iter().filter(|g| g.robots_blocked).collect();
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0].urls.len(), 1);
    assert_eq!(bing.site.limited, 4);
    // Engines with no page controls list nothing, but still count the site's pages.
    let chatgpt = r.engine(EngineId::ChatGpt).expect("chatgpt");
    assert!(chatgpt.groups.is_empty());
    assert_eq!(chatgpt.site.pages, 4);
}

#[test]
fn non_html_and_failed_pages_are_not_eligibility_candidates() {
    let mut out = site(3, |p| p.robots_meta("nosnippet"));
    out.pages[1] = out.pages[1].clone().status(500);
    out.pages[2].content_type = Some("application/pdf".to_owned());
    let r = report_for(&out);
    let google = r.engine(EngineId::Google).expect("google");
    assert_eq!(google.html_pages, 1);
    assert_eq!(google.site.pages, 1);
}

#[test]
fn declared_preferences_come_from_robots_headers_meta_and_tdmrep() {
    let mut out = with_robots(
        site(2, |p| p),
        200,
        "User-agent: *\nContent-Signal: search=yes, ai-train=no\nContent-Usage: train-ai=n\n",
    );
    out.signals.home_headers = vec![
        ("content-signal".to_owned(), "ai-input=no".to_owned()),
        ("content-usage".to_owned(), "/blog train-ai=n".to_owned()),
        ("tdm-reservation".to_owned(), "1".to_owned()),
        (
            "tdm-policy".to_owned(),
            "https://example.com/tdm".to_owned(),
        ),
    ];
    out.pages[0].fields.ai.tdm_reservation = Some("1".to_owned());
    let out = with_tdmrep(
        out,
        200,
        r#"[{"location":"/","tdm-reservation":1,"tdm-policy":"https://example.com/p"}]"#,
    );
    let d = report_for(&out).declared;
    assert_eq!(d.content_signals.len(), 1);
    assert_eq!(d.content_usage.len(), 1);
    assert_eq!(d.headers.content_signal[0].key, "ai-input");
    assert!(d.headers.content_signal[0].known);
    assert_eq!(d.headers.content_usage[0].path.as_deref(), Some("/blog"));
    assert_eq!(d.headers.tdm_reservation.as_deref(), Some("1"));
    assert_eq!(d.tdm_meta.reservation.as_deref(), Some("1"));
    let tdm = d.tdmrep.expect("tdmrep");
    assert_eq!(tdm.entries.len(), 1);
    assert_eq!(tdm.error, None);

    let bad = report_for(&with_tdmrep(site(2, |p| p), 200, "{}"))
        .declared
        .tdmrep
        .expect("tdmrep");
    assert!(bad.entries.is_empty());
    assert!(bad.error.is_some());
    let gone = report_for(&with_tdmrep(site(2, |p| p), 404, ""))
        .declared
        .tdmrep
        .expect("tdmrep");
    assert_eq!((gone.status, gone.error), (404, None));
}

#[test]
fn robots_declarations_are_skipped_when_the_file_is_unavailable() {
    let out = with_robots(site(2, |p| p), 503, "Content-Signal: search=yes\n");
    assert!(report_for(&out).declared.content_signals.is_empty());
}

#[test]
fn json_round_trips_and_an_old_shape_loads() {
    let out = with_robots(
        site(5, |p| p.robots_meta("noarchive")),
        200,
        "User-agent: GPTBot\nDisallow: /\n",
    );
    let r = report_for(&out);
    let json = serde_json::to_string(&r).expect("json");
    let back: AccessReport = serde_json::from_str(&json).expect("back");
    assert_eq!(back, r);
    // Only the required fields: everything later-added defaults.
    let minimal = r#"{"registry_version":1,"robots":{"status":null,"availability":"unknown"},"important":[]}"#;
    let old: AccessReport = serde_json::from_str(minimal).expect("minimal");
    assert_eq!(old.version, 0);
    assert!(old.bots.is_empty() && old.engines.is_empty());
}

/// 60 important pages and every registry bot blocked everywhere by its own rule; `vary` is how
/// many distinct markup variants the pages cycle through.
fn heavy_report(vary: u32) -> (AccessReport, usize) {
    let mut robots = String::new();
    for bot in &registry().bots {
        robots.push_str(&format!("User-agent: {}\nDisallow: /\n\n", bot.token));
    }
    let out = with_robots(
        site(70, |p| {
            let v = 1 + p.inlinks % vary;
            p.robots_meta(&format!("nosnippet, noarchive, nocache, max-snippet:{v}"))
                .x_robots(&format!("googlebot: max-snippet:{v}, bingbot: noarchive"))
                .bot_meta("applebot", "nosnippet")
                .nosnippet_words(30 + v)
        }),
        200,
        &robots,
    );
    // The top 20 plus starred pages fill the cap.
    let starred: HashSet<u64> = out.pages.iter().skip(1).map(|p| url_hash(&p.url)).collect();
    let important = important_urls(&out.pages, &out.origin, &starred);
    let r = build_report(&out, &important);
    assert_eq!(r.important.len(), MAX_IMPORTANT);
    let size = serde_json::to_vec(&r).expect("json").len();
    println!(
        "report size ({vary} variants): {size} bytes (bots {}, engines {})",
        serde_json::to_vec(&r.bots).expect("json").len(),
        serde_json::to_vec(&r.engines).expect("json").len()
    );
    (r, size)
}

#[test]
fn a_heavy_report_stays_under_100_kb() {
    // Every control on every page, pages sharing a handful of markup variants.
    let (_, size) = heavy_report(4);
    assert!(size < 100_000, "{size} bytes");
    let (_, uniform) = heavy_report(1);
    assert!(uniform < 60_000, "{uniform} bytes");
}

#[test]
fn even_pages_that_all_differ_stay_bounded() {
    // The pathological case: no two pages share a group, five directives each.
    let (_, size) = heavy_report(60);
    assert!(size < 130_000, "{size} bytes");
}

/// 60 important URLs of a 70-page site, starred pages filling the cap.
fn sixty_urls(out: &CrawlOutput) -> AccessReport {
    let starred: HashSet<u64> = out.pages.iter().skip(1).map(|p| url_hash(&p.url)).collect();
    let important = important_urls(&out.pages, &out.origin, &starred);
    let r = build_report(out, &important);
    assert_eq!(r.important.len(), MAX_IMPORTANT);
    r
}

#[test]
fn a_hostile_robots_file_gives_a_bounded_report() {
    let mut body = String::new();
    // Thousands of agents in one group, with long declared lines that would copy them all.
    for i in 0..2_000 {
        body.push_str(&format!("User-agent: agent-{i}\n"));
    }
    let pairs: Vec<String> = (0..60)
        .map(|k| format!("key{k}={}", "v".repeat(40)))
        .collect();
    for _ in 0..40 {
        body.push_str(&format!("Content-Signal: {}\n", pairs.join(", ")));
        body.push_str(&format!("Content-Usage: /x {}\n", pairs.join(" ")));
    }
    // A long wildcard rule that blocks everything, and every bot blocked on every page by long
    // rules of its own.
    body.push_str(&format!(
        "User-agent: *\nDisallow: /{}\n",
        "*".repeat(1_000)
    ));
    for (b, bot) in registry().bots.iter().enumerate() {
        body.push_str(&format!("User-agent: {}\n", bot.token));
        for i in 0..70 {
            body.push_str(&format!("Disallow: /p{i}{}$\n", "*".repeat(150 + b)));
        }
    }
    assert!(body.len() >= 500 * 1024, "{} bytes", body.len());
    let out = with_robots(site(70, |p| p), 200, &body);
    let r = sixty_urls(&out);
    let size = serde_json::to_vec(&r).expect("json").len();
    println!("hostile report: {size} bytes");
    assert!(size < 150_000, "{size} bytes");
    assert!(r.rules.len() <= 128);
    assert!(
        r.rules
            .iter()
            .all(|rule| rule.pattern.chars().count() <= 201)
    );
    let signal = &r.declared.content_signals[0];
    assert!(signal.agents.len() <= 20 && signal.more_agents > 0);
    // Every block is still listed, with or without its rule.
    assert!(r.bots.iter().all(|b| b.blocked.len() >= 59));
}

#[test]
fn a_realistic_site_stays_well_under_100_kb() {
    let robots = "# Managed robots.txt\nUser-agent: *\nContent-Signal: search=yes, ai-train=no\nAllow: /\nDisallow: /cart\nDisallow: /account/\nDisallow: /*?sort=\n\nUser-agent: GPTBot\nDisallow: /\n\nUser-agent: ClaudeBot\nDisallow: /\n\nUser-agent: Google-Extended\nDisallow: /\n\nUser-agent: CCBot\nDisallow: /\n\nUser-agent: Applebot-Extended\nDisallow: /\n\nSitemap: https://example.com/sitemap.xml\n";
    let out = with_robots(site(70, |p| p), 200, robots);
    let r = sixty_urls(&out);
    let size = serde_json::to_vec(&r).expect("json").len();
    println!("realistic report: {size} bytes");
    assert!(size < 40_000, "{size} bytes");
}

#[test]
fn a_wildcard_rule_is_stored_once_for_every_bot_with_its_pattern_cut() {
    let long = format!("/{}", "a".repeat(300));
    let out = with_robots(
        site(3, |p| p),
        200,
        &format!("User-agent: *\nDisallow: /\nDisallow: {long}\n"),
    );
    let r = report_for(&out);
    assert_eq!(r.rules.len(), 1, "one rule, pooled once");
    assert!(r.bots.iter().all(|b| b.rules.is_empty()));
    let gpt = r.bot("GPTBot").expect("gptbot");
    assert_eq!(gpt.blocked.len(), 3);
    assert_eq!(r.rule_of(gpt, &gpt.blocked[0]).expect("rule").pattern, "/");

    // A long pattern that decides a block is kept cut, with a closing ellipsis.
    let out = with_robots(
        site(1, |p| p),
        200,
        &format!("User-agent: *\nDisallow: /*{}\n", "b".repeat(300)),
    );
    let mut out = out;
    out.pages[0].url =
        url::Url::parse(&format!("https://example.com/x{}", "b".repeat(300))).expect("url");
    let important = vec![codoseo_geo::report::ImportantUrl {
        url: out.pages[0].url.to_string(),
        reason: Reason::Home,
        status: Some(200),
        html_ok: true,
    }];
    let r = build_report(&out, &important);
    let pattern = &r.rules[0].pattern;
    assert_eq!(pattern.chars().count(), 201);
    assert!(pattern.ends_with('…'), "{pattern}");
    assert_eq!(
        r.bots[0].home_rule.as_ref().expect("home rule").pattern,
        *pattern
    );
}

#[test]
fn an_older_report_with_rules_per_bot_still_names_its_rules() {
    let old = r#"{"registry_version":1,"robots":{"status":200,"availability":"ok","hash":9},"important":[{"url":"https://example.com/","reason":"home","status":200,"html_ok":true}],"bots":[{"token":"GPTBot","home_allowed":false,"group":"named","rules":[{"line":2,"allow":false,"pattern":"/"}],"blocked":[{"url":0,"rule":0}]}],"pages_crawled":1}"#;
    let r: AccessReport = serde_json::from_str(old).expect("old shape");
    let gpt = r.bot("GPTBot").expect("gptbot");
    assert_eq!(r.rule_of(gpt, &gpt.blocked[0]).map(|x| x.line), Some(2));
}

#[test]
fn headers_and_meta_of_an_error_home_page_are_unknown() {
    let mut out = site(2, |p| p);
    out.signals.home_headers = vec![("content-signal".to_owned(), "ai-train=no".to_owned())];
    out.pages[0].fields.ai.tdm_reservation = Some("1".to_owned());
    let read = report_for(&out);
    assert!(read.declared.home_read);
    assert!(read.preferences_known());
    assert_eq!(read.declared.headers.content_signal.len(), 1);

    out.pages[0] = out.pages[0].clone().status(503);
    let r = report_for(&out);
    assert!(!r.declared.home_read);
    assert!(!r.preferences_known());
    assert!(r.declared.headers.content_signal.is_empty());
    assert_eq!(r.declared.tdm_meta.reservation, None);
}

#[test]
fn applebot_follows_googlebots_group_when_none_names_it() {
    let out = with_robots(
        site(3, |p| p),
        200,
        "User-agent: *\nDisallow: /\n\nUser-agent: Googlebot\nAllow: /\nDisallow: /p1\n",
    );
    let r = report_for(&out);
    let apple = r.bot("Applebot").expect("applebot");
    assert_eq!(apple.group, GroupMatch::Fallback);
    assert_eq!(apple.home_allowed, Some(true));
    assert_eq!(apple.blocked.len(), 1);
    // Apple's engine uses the same verdict; a bot without a documented fallback gets `*`.
    let engine = r.engine(EngineId::Apple).expect("apple");
    assert_eq!(engine.groups.iter().filter(|g| g.robots_blocked).count(), 1);
    let ext = r.bot("Applebot-Extended").expect("applebot-extended");
    assert_eq!(ext.group, GroupMatch::Wildcard);
    assert_eq!(ext.home_allowed, Some(false));
}
