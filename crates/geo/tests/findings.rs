mod common;

use std::collections::HashSet;

use codoseo_core::check::Severity;
use codoseo_core::output::CrawlOutput;
use codoseo_geo::eligibility::{Effect, EngineId};
use codoseo_geo::findings::{Finding, FindingKind, Grade, evaluated_kinds, findings};
use codoseo_geo::intent::Intent;
use codoseo_geo::report::{AccessReport, build_report, important_urls};
use common::*;

fn report_for(out: &CrawlOutput) -> AccessReport {
    let important = important_urls(&out.pages, &out.origin, &HashSet::new());
    build_report(out, &important)
}

fn run(out: &CrawlOutput, intent: &Intent) -> Vec<Finding> {
    findings(&report_for(out), intent)
}

fn intent(json: &str) -> Intent {
    serde_json::from_str(json).expect("intent")
}

#[test]
fn a_clean_site_has_no_findings() {
    assert!(run(&site(5, |p| p), &Intent::default()).is_empty());
    // Noindex is the SEO checks' business, not an AI-answers finding.
    assert!(run(&site(5, |p| p.robots_meta("noindex")), &Intent::default()).is_empty());
}

#[test]
fn blocking_an_ai_search_bot_is_one_critical_finding_with_the_line() {
    let out = with_robots(
        site(6, |p| p),
        200,
        "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /\n",
    );
    let f = run(&out, &Intent::default());
    println!("{} | {}", f[0].title, f[0].summary);
    assert_eq!(f.len(), 1);
    let f = &f[0];
    assert_eq!(
        (f.kind, f.subject.as_str()),
        (FindingKind::BotsBlocked, "search")
    );
    assert_eq!(f.severity, Severity::Critical);
    assert_eq!(f.grade, Grade::A);
    assert_eq!(f.title, "OAI-SearchBot is blocked by robots.txt");
    assert!(f.summary.contains("ChatGPT search"), "{}", f.summary);
    assert!(
        f.summary
            .contains("line 5: `Disallow: /` (User-agent: OAI-SearchBot)"),
        "{}",
        f.summary
    );
    let b = &f.evidence.bots[0];
    assert_eq!(b.token, "OAI-SearchBot");
    assert_eq!((b.urls_blocked, b.urls_total, b.home_blocked), (6, 6, true));
    assert_eq!(b.rule.as_ref().map(|r| r.line), Some(5));
    assert_eq!(f.evidence.urls.total, 6);
    assert_eq!(
        f.sources,
        vec!["https://developers.openai.com/api/docs/bots".to_owned()]
    );
}

#[test]
fn two_blocked_search_bots_share_one_finding() {
    let out = with_robots(
        site(4, |p| p),
        200,
        "User-agent: OAI-SearchBot\nUser-agent: PerplexityBot\nDisallow: /\n",
    );
    let f = run(&out, &Intent::default());
    println!("{}", f[0].title);
    assert_eq!(f.len(), 1);
    assert_eq!(
        f[0].title,
        "OAI-SearchBot and PerplexityBot are blocked by robots.txt"
    );
    assert!(f[0].summary.contains("ChatGPT search and Perplexity"));
    assert_eq!(f[0].sources.len(), 2);
}

#[test]
fn blocking_everything_with_a_wildcard_is_grouped_by_purpose_and_counted() {
    let out = with_robots(site(3, |p| p), 200, "User-agent: *\nDisallow: /\n");
    let f = run(&out, &Intent::default());
    let subjects: Vec<_> = f.iter().map(|f| f.subject.as_str()).collect();
    // Search and user fetchers are wanted by default; the rest have no stance.
    assert_eq!(subjects, ["search", "user_fetch"]);
    println!("{}", f[0].title);
    assert_eq!(f[0].title, "10 AI search bots are blocked by robots.txt");
    assert!(f[0].summary.contains("(User-agent: *)"));
}

#[test]
fn a_block_below_the_home_page_is_a_warning_and_says_so() {
    let out = with_robots(
        site(5, |p| p),
        200,
        "User-agent: PerplexityBot\nDisallow: /p1\n",
    );
    let f = run(&out, &Intent::default());
    assert_eq!(f.len(), 1);
    println!("{} | {}", f[0].title, f[0].summary);
    assert_eq!(f[0].severity, Severity::Warning);
    assert_eq!(f[0].title, "PerplexityBot is partly blocked by robots.txt");
    assert!(f[0].summary.contains("home page is allowed"));
    assert!(!f[0].evidence.bots[0].home_blocked);
}

#[test]
fn a_bot_that_may_ignore_robots_is_only_a_notice() {
    // Perplexity-User is a user-triggered fetcher the operator says doesn't honour robots.txt.
    let out = with_robots(
        site(3, |p| p),
        200,
        "User-agent: Perplexity-User\nDisallow: /\n",
    );
    let f = run(&out, &Intent::default());
    assert_eq!(f.len(), 1);
    println!("{} | {}", f[0].title, f[0].summary);
    assert_eq!(
        (f[0].subject.as_str(), f[0].severity),
        ("user_fetch", Severity::Notice)
    );
    assert!(f[0].summary.contains("may not hold"));
}

#[test]
fn blocking_a_bot_the_owner_does_not_want_is_fine() {
    let out = with_robots(site(3, |p| p), 200, "User-agent: GPTBot\nDisallow: /\n");
    assert!(run(&out, &Intent::default()).is_empty());
    let out = with_robots(
        site(3, |p| p),
        200,
        "User-agent: OAI-SearchBot\nDisallow: /\n",
    );
    assert!(run(&out, &intent(r#"{"bots":{"OAI-SearchBot":"block"}}"#)).is_empty());
    assert!(run(&out, &intent(r#"{"purposes":{"search":"any"}}"#)).is_empty());
}

#[test]
fn a_training_bot_that_gets_in_against_the_owners_wishes() {
    let block_training = intent(r#"{"purposes":{"training":"block"}}"#);
    // Nothing blocked: eight honouring training bots (control tokens included).
    let f = run(&site(3, |p| p), &block_training);
    assert_eq!(f.len(), 1);
    println!("{}", f[0].title);
    assert_eq!(
        (f[0].kind, f[0].subject.as_str()),
        (FindingKind::BotsNotBlocked, "training")
    );
    assert_eq!(f[0].severity, Severity::Warning);
    assert_eq!(f[0].title, "8 AI training bots can still use your content");
    assert!(
        f[0].summary
            .starts_with("You set AI training to Block, but robots.txt doesn't stop them.")
    );

    // Only three left over: they are named.
    let robots = "User-agent: Google-Extended\nUser-agent: Applebot-Extended\nUser-agent: meta-externalagent\nUser-agent: Amazonbot\nUser-agent: MistralAI-Training\nDisallow: /\n";
    let f = run(&with_robots(site(3, |p| p), 200, robots), &block_training);
    println!("{} | {}", f[0].title, f[0].summary);
    assert_eq!(
        f[0].title,
        "GPTBot, ClaudeBot and CCBot can still crawl for AI training"
    );
    assert!(f[0].summary.contains("Add `User-agent: GPTBot`"));

    // Control tokens never crawl, so the title must not say they can.
    let robots = "User-agent: GPTBot\nUser-agent: ClaudeBot\nUser-agent: CCBot\nUser-agent: meta-externalagent\nUser-agent: Amazonbot\nUser-agent: MistralAI-Training\nDisallow: /\n";
    let f = run(&with_robots(site(3, |p| p), 200, robots), &block_training);
    assert_eq!(
        f[0].title,
        "Google-Extended and Applebot-Extended can still use your content for AI training"
    );

    // The same bots blocked: nothing to report.
    let all = "User-agent: GPTBot\nUser-agent: ClaudeBot\nUser-agent: CCBot\nUser-agent: Google-Extended\nUser-agent: Applebot-Extended\nUser-agent: meta-externalagent\nUser-agent: Amazonbot\nUser-agent: MistralAI-Training\nDisallow: /\n";
    assert!(run(&with_robots(site(3, |p| p), 200, all), &block_training).is_empty());
}

#[test]
fn a_per_bot_block_that_is_not_applied_is_named_as_the_owners_choice() {
    let f = run(&site(3, |p| p), &intent(r#"{"bots":{"GPTBot":"block"}}"#));
    assert_eq!(f.len(), 1);
    println!("{} | {}", f[0].title, f[0].summary);
    assert_eq!(f[0].title, "GPTBot can still crawl for AI training");
    assert!(
        f[0].summary
            .starts_with("You chose to block GPTBot, but robots.txt doesn't stop it.")
    );
}

#[test]
fn a_missing_robots_file_is_mentioned_when_a_block_is_not_applied() {
    let out = with_robots(site(3, |p| p), 404, "");
    let f = run(&out, &intent(r#"{"bots":{"CCBot":"block"}}"#));
    assert!(
        f[0].summary
            .starts_with("There is no robots.txt (HTTP 404). "),
        "{}",
        f[0].summary
    );
    // And nothing is blocked there.
    assert!(run(&out, &Intent::default()).is_empty());
}

#[test]
fn a_broken_robots_file_is_one_critical_finding_and_nothing_else() {
    let out = with_robots(site(4, |p| p.robots_meta("nosnippet")), 503, "");
    let f = run(&out, &Intent::default());
    println!("{} | {}", f[0].title, f[0].summary);
    let robots: Vec<_> = f
        .iter()
        .filter(|f| f.kind == FindingKind::RobotsUnavailable)
        .collect();
    assert_eq!(robots.len(), 1);
    assert_eq!(robots[0].severity, Severity::Critical);
    assert_eq!(
        robots[0].title,
        "robots.txt returns HTTP 503, so bots treat the whole site as off limits"
    );
    assert_eq!(robots[0].subject, "");
    assert_eq!(robots[0].evidence.robots_status, Some(503));
    assert!(robots[0].sources.len() > 2);
    assert!(f.iter().all(|f| !matches!(
        f.kind,
        FindingKind::BotsBlocked | FindingKind::BotsNotBlocked
    )));

    // Without markup that restricts answers it really is the only finding.
    let f = run(&with_robots(site(4, |p| p), 503, ""), &Intent::default());
    assert_eq!(f.len(), 1);
    // 429 counts as unavailable too.
    let f = run(&with_robots(site(4, |p| p), 429, ""), &Intent::default());
    assert!(f[0].title.contains("HTTP 429"));
}

#[test]
fn a_broken_robots_file_is_a_notice_when_no_honouring_bot_is_wanted() {
    let none = intent(r#"{"purposes":{"search":"any","user_fetch":"any"}}"#);
    let f = run(&with_robots(site(4, |p| p), 500, ""), &none);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].severity, Severity::Notice);
}

#[test]
fn nosnippet_everywhere_is_one_grouped_critical_finding() {
    let out = site(21, |p| p.robots_meta("nosnippet"));
    let f = run(&out, &Intent::default());
    assert_eq!(f.len(), 1);
    let f = &f[0];
    println!("{} | {}", f.title, f.summary);
    assert_eq!(
        (f.kind, f.subject.as_str()),
        (FindingKind::AnswersRestricted, "nosnippet")
    );
    assert_eq!(f.severity, Severity::Critical);
    assert_eq!(
        f.title,
        "nosnippet on all 20 important pages, including the home page"
    );
    let effects: Vec<_> = f
        .evidence
        .engines
        .iter()
        .map(|e| (e.engine, e.effect))
        .collect();
    assert_eq!(
        effects,
        [
            (EngineId::Google, Effect::Excluded),
            (EngineId::Bing, Effect::Limited),
            (EngineId::Apple, Effect::Limited)
        ]
    );
    assert_eq!(f.evidence.urls.total, 20);
    assert_eq!(f.evidence.urls.sample.len(), 10);
    assert_eq!(f.evidence.engines[0].pages_excluded, 20);
    assert_eq!(f.evidence.engines[0].site_pages, 21);
    assert_eq!(f.sources.len(), 3);
    assert!(
        f.summary
            .contains("keeps these pages out of Google Search, AI Overviews & AI Mode")
    );
    assert!(f.summary.contains(
        "limits what Bing & Microsoft Copilot and Apple (Siri, Spotlight, Safari) can quote"
    ));
}

#[test]
fn nosnippet_on_some_pages_counts_them_and_names_the_home_page() {
    // 21 pages crawled, but the home page is among the 20 most linked: 20 important pages.
    let mut out = site(21, |p| p);
    for p in out.pages.iter_mut().take(12) {
        p.fields.meta_robots = Some("nosnippet".to_owned());
    }
    let f = run(&out, &Intent::default());
    println!("{}", f[0].title);
    assert_eq!(f.len(), 1);
    assert_eq!(
        f[0].title,
        "nosnippet on 12 of 20 important pages, including the home page"
    );
    assert_eq!(f[0].severity, Severity::Critical);
}

#[test]
fn an_exclusion_that_spares_the_home_page_and_most_pages_is_a_warning() {
    let mut out = site(21, |p| p);
    out.pages[5].fields.meta_robots = Some("nosnippet".to_owned());
    let f = run(&out, &Intent::default());
    assert_eq!(f[0].title, "nosnippet on 1 of 20 important pages");
    assert_eq!(f[0].severity, Severity::Warning);
}

#[test]
fn limits_alone_are_a_notice() {
    // Apple limits on nosnippet but only Google excludes; narrow the wanted engines to Apple.
    let out = site(4, |p| p.robots_meta("nosnippet"));
    let only_apple = intent(r#"{"bots":{"Googlebot":"any","Bingbot":"any"}}"#);
    let f = run(&out, &only_apple);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].severity, Severity::Notice);
    assert_eq!(f[0].evidence.engines.len(), 1);
    assert_eq!(f[0].evidence.engines[0].engine, EngineId::Apple);
}

#[test]
fn site_wide_noarchive_names_bing_only() {
    // Amazon documents `noarchive` as a training control, not an answers control.
    let f = run(
        &site(10, |p| p.robots_meta("noarchive")),
        &Intent::default(),
    );
    assert_eq!(f.len(), 1);
    println!("{} | {}", f[0].title, f[0].summary);
    assert_eq!(
        (f[0].kind, f[0].subject.as_str()),
        (FindingKind::AnswersRestricted, "noarchive")
    );
    assert_eq!(f[0].severity, Severity::Critical);
    let engines: Vec<_> = f[0]
        .evidence
        .engines
        .iter()
        .map(|e| (e.engine, e.effect))
        .collect();
    assert_eq!(engines, [(EngineId::Bing, Effect::Excluded)]);
}

#[test]
fn a_scoped_directive_only_speaks_for_its_engine() {
    let f = run(
        &site(4, |p| p.bot_meta("bingbot", "noarchive")),
        &Intent::default(),
    );
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].evidence.engines.len(), 1);
    let none = run(
        &site(4, |p| p.bot_meta("googlebot", "noarchive")),
        &Intent::default(),
    );
    assert!(none.is_empty());
}

#[test]
fn separate_directives_are_separate_findings() {
    let f = run(
        &site(4, |p| p.robots_meta("nosnippet, noarchive")),
        &Intent::default(),
    );
    let subjects: Vec<_> = f.iter().map(|f| (f.kind, f.subject.as_str())).collect();
    assert_eq!(
        subjects,
        [
            (FindingKind::AnswersRestricted, "noarchive"),
            (FindingKind::AnswersRestricted, "nosnippet")
        ]
    );
}

#[test]
fn data_nosnippet_and_max_snippet_are_findings_of_their_own() {
    let f = run(&site(4, |p| p.nosnippet_words(60)), &Intent::default());
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].subject, "data_nosnippet");
    assert_eq!(f[0].severity, Severity::Notice);
    assert_eq!(
        f[0].title,
        "data-nosnippet on all 4 important pages, including the home page"
    );
    let f = run(
        &site(4, |p| p.robots_meta("max-snippet:0")),
        &Intent::default(),
    );
    assert_eq!(
        (f[0].subject.as_str(), f[0].severity),
        ("max_snippet", Severity::Critical)
    );
}

#[test]
fn an_engine_the_owner_does_not_want_is_left_out() {
    let out = site(4, |p| p.robots_meta("nosnippet"));
    let none = intent(r#"{"bots":{"Googlebot":"block","Bingbot":"any","Applebot":"block"}}"#);
    assert!(
        run(&out, &none)
            .iter()
            .all(|f| f.kind != FindingKind::AnswersRestricted)
    );
}

#[test]
fn an_engine_blocked_by_robots_is_a_robots_finding_not_an_answers_one() {
    // Bing can't fetch anything, so its page controls are moot; Google still sees the markup.
    let out = with_robots(
        site(4, |p| p.robots_meta("noarchive")),
        200,
        "User-agent: Bingbot\nDisallow: /\n",
    );
    let f = run(&out, &Intent::default());
    assert_eq!(f.len(), 1);
    assert_eq!(
        (f[0].kind, f[0].subject.as_str()),
        (FindingKind::BotsBlocked, "search")
    );
    assert_eq!(f[0].title, "Bingbot is blocked by robots.txt");
}

#[test]
fn single_page_sites_read_naturally() {
    let f = run(&site(1, |p| p.robots_meta("nosnippet")), &Intent::default());
    assert_eq!(f[0].title, "nosnippet on your only important page");
}

#[test]
fn findings_are_ordered_and_serialise() {
    let out = with_robots(
        site(4, |p| p.robots_meta("nosnippet")),
        200,
        "User-agent: PerplexityBot\nDisallow: /\n",
    );
    let f = run(&out, &intent(r#"{"bots":{"CCBot":"block"}}"#));
    let kinds: Vec<_> = f.iter().map(|f| f.kind).collect();
    assert_eq!(
        kinds,
        [
            FindingKind::BotsBlocked,
            FindingKind::BotsNotBlocked,
            FindingKind::AnswersRestricted
        ]
    );
    let json = serde_json::to_string(&f).expect("json");
    let back: Vec<Finding> = serde_json::from_str(&json).expect("back");
    assert_eq!(back, f);
    assert!(json.contains(r#""grade":"A""#));
    assert!(json.contains(r#""kind":"bots_blocked""#));
    assert_eq!(
        FindingKind::from_slug("answers_restricted"),
        Some(FindingKind::AnswersRestricted)
    );
    assert_eq!(FindingKind::from_slug("nope"), None);
}

#[test]
fn evaluated_kinds_follow_what_the_report_can_see() {
    let ok = report_for(&site(3, |p| p));
    assert_eq!(
        evaluated_kinds(&ok),
        [
            FindingKind::RobotsUnavailable,
            FindingKind::BotsBlocked,
            FindingKind::BotsNotBlocked,
            FindingKind::AnswersRestricted
        ]
    );
    let down = report_for(&with_robots(site(3, |p| p), 503, ""));
    assert_eq!(
        evaluated_kinds(&down),
        [
            FindingKind::RobotsUnavailable,
            FindingKind::AnswersRestricted
        ]
    );
    let mut unknown = site(3, |p| p);
    unknown.robots = None;
    assert_eq!(
        evaluated_kinds(&report_for(&unknown)),
        [FindingKind::AnswersRestricted]
    );
    // No HTML page at all: robots.txt questions only.
    let no_html = report_for(&crawl(Vec::new()));
    assert_eq!(
        evaluated_kinds(&no_html),
        [
            FindingKind::RobotsUnavailable,
            FindingKind::BotsBlocked,
            FindingKind::BotsNotBlocked
        ]
    );
}

#[test]
fn home_page_severity_counts_only_excluding_engines() {
    // Google excludes one inner page; only Bing (which limits) is affected on the home page.
    let mut out = site(10, |p| p);
    out.pages[0]
        .fields
        .ai
        .bot_meta
        .push(("bingbot".to_owned(), "nosnippet".to_owned()));
    out.pages[5]
        .fields
        .ai
        .bot_meta
        .push(("googlebot".to_owned(), "nosnippet".to_owned()));
    let f = run(&out, &Intent::default());
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].subject, "nosnippet");
    assert_eq!(f[0].severity, Severity::Warning);

    // The home page excluded in Google is Critical.
    let mut out = site(10, |p| p);
    out.pages[0]
        .fields
        .ai
        .bot_meta
        .push(("googlebot".to_owned(), "nosnippet".to_owned()));
    let f = run(&out, &Intent::default());
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].severity, Severity::Critical);
}
