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
            FindingKind::AnswersRestricted,
            FindingKind::ContentMissing
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

#[test]
fn an_accepted_directive_opens_no_finding_but_others_still_do() {
    let out = site(6, |p| p.robots_meta("nosnippet, noarchive"));
    let accepted = intent(r#"{"accepted_directives":["nosnippet"]}"#);
    let f = run(&out, &accepted);
    let subjects: Vec<&str> = f.iter().map(|f| f.subject.as_str()).collect();
    assert_eq!(subjects, ["noarchive"]);
    assert_eq!(run(&out, &Intent::default()).len(), 2);
    // The report, and so the matrix, still shows it.
    let google = report_for(&out);
    let google = google.engine(EngineId::Google).expect("google");
    assert_eq!(google.affected(), 6);
    // Unknown fields aside, the slugs read back as written.
    assert_eq!(
        serde_json::to_value(&accepted).expect("json"),
        serde_json::json!({"accepted_directives": ["nosnippet"]})
    );
}

#[test]
fn applebots_fallback_rule_is_described_as_googlebots() {
    let out = with_robots(
        site(3, |p| p),
        200,
        "User-agent: *\nAllow: /\n\nUser-agent: Googlebot\nDisallow: /\n",
    );
    let f = run(&out, &Intent::default());
    let search = f
        .iter()
        .find(|f| f.kind == FindingKind::BotsBlocked)
        .expect("search bots blocked");
    let tokens: Vec<&str> = search
        .evidence
        .bots
        .iter()
        .map(|b| b.token.as_str())
        .collect();
    assert_eq!(tokens, ["Googlebot", "Applebot"]);
    assert!(
        search.summary.contains("(User-agent: Googlebot)"),
        "{}",
        search.summary
    );
    let apple = &search.evidence.bots[1];
    assert_eq!(apple.group, codoseo_geo::robots::GroupMatch::Fallback);
}

#[test]
fn a_redirecting_robots_txt_is_missing_not_off_limits() {
    let out = with_robots(site(3, |p| p), 301, "");
    let f = run(&out, &intent(r#"{"purposes":{"training":"block"}}"#));
    assert!(f.iter().all(|f| f.kind != FindingKind::RobotsUnavailable));
    let not_blocked = f
        .iter()
        .find(|f| f.kind == FindingKind::BotsNotBlocked)
        .expect("training bots get in");
    assert!(
        not_blocked
            .summary
            .starts_with("robots.txt redirects without reaching a file (HTTP 301)."),
        "{}",
        not_blocked.summary
    );
}

// ---- Synthetic fetch probes ------------------------------------------------------------------

use codoseo_geo::findings::{Evidence, VENDOR_DOCS};
use codoseo_geo::probe::{
    InconclusiveReason, ProbeFetch, ProbeReport, ProbeResult, ProbedUrl, Vendor, Verdict,
};
use codoseo_geo::report::Reason;

fn fetch(status: u16, title: &str, bytes: u32) -> ProbeFetch {
    ProbeFetch {
        status: Some(status),
        html: true,
        bytes,
        title: Some(title.to_owned()),
        ..ProbeFetch::default()
    }
}

/// What a probe fetch looked like for a verdict, roughly.
fn fetch_for(verdict: &Verdict) -> Option<ProbeFetch> {
    Some(match verdict {
        Verdict::SkippedRobots => return None,
        Verdict::Challenged { vendor } => ProbeFetch {
            challenge: Some(codoseo_geo::probe::Challenge {
                vendor: *vendor,
                signal: "cf-mitigated: challenge".to_owned(),
            }),
            ..fetch(403, "Just a moment...", 4_000)
        },
        Verdict::Blocked { status } => ProbeFetch {
            status: *status,
            ..fetch(status.unwrap_or(0), "Forbidden", 300)
        },
        Verdict::Throttled => fetch(429, "Too many requests", 200),
        Verdict::Degraded => fetch(200, "Please enable JavaScript", 500),
        Verdict::Diverted { to } => ProbeFetch {
            final_url: Some(to.clone()),
            ..fetch(200, "Home", 20_000)
        },
        _ => fetch(200, "Shop", 20_000),
    })
}

/// A site of `pages` pages with these probe verdicts per page (home is index 0).
fn probed(pages: u32, urls: &[(u16, &[(&str, Verdict)])]) -> AccessReport {
    let mut report = report_for(&site(pages, |p| p));
    assert_eq!(report.important[0].reason, Reason::Home);
    let mut bots: Vec<String> = Vec::new();
    let urls = urls
        .iter()
        .map(|(url, results)| ProbedUrl {
            url: *url,
            baseline: fetch(200, "Shop", 20_000),
            results: results
                .iter()
                .map(|(token, verdict)| {
                    if !bots.iter().any(|b| b == token) {
                        bots.push((*token).to_owned());
                    }
                    ProbeResult {
                        token: (*token).to_owned(),
                        fetch: fetch_for(verdict),
                        verdict: verdict.clone(),
                    }
                })
                .collect(),
        })
        .collect();
    report.probes = Some(ProbeReport {
        version: 1,
        synthetic: true,
        from: "hello@codoseo.com".to_owned(),
        bots,
        skipped_bots: Vec::new(),
        urls,
        stop: None,
        requests: 0,
    });
    report
}

fn cf() -> Verdict {
    Verdict::Challenged {
        vendor: Vendor::Cloudflare,
    }
}

fn fetch_blocked(report: &AccessReport, intent: &Intent) -> Vec<Finding> {
    findings(report, intent)
        .into_iter()
        .filter(|f| f.kind == FindingKind::FetchBlocked)
        .collect()
}

#[test]
fn three_bots_challenged_by_cloudflare_are_one_finding_with_three_members() {
    let report = probed(
        3,
        &[(
            0,
            &[
                ("OAI-SearchBot", cf()),
                ("Claude-SearchBot", cf()),
                ("PerplexityBot", cf()),
                ("ChatGPT-User", Verdict::Same),
            ],
        )],
    );
    let f = fetch_blocked(&report, &Intent::default());
    assert_eq!(f.len(), 1, "{f:#?}");
    let f = &f[0];
    println!("{} | {}", f.title, f.summary);
    assert_eq!(f.subject, "challenge_cloudflare");
    assert_eq!(f.grade, Grade::B);
    assert!(f.evidence.synthetic);
    assert_eq!(
        f.title,
        "Cloudflare challenge served to OAI-SearchBot, Claude-SearchBot and PerplexityBot on the home page (synthetic check)"
    );
    let members: Vec<String> = f.evidence.members().into_iter().collect();
    assert_eq!(
        members,
        ["claude-searchbot", "oai-searchbot", "perplexitybot"]
    );
    let p = &f.evidence.probes[0];
    assert_eq!(p.token, "OAI-SearchBot");
    assert_eq!(
        (p.urls_affected, p.urls_probed, p.home_affected),
        (1, 1, true)
    );
    assert_eq!(p.vendor, Some(Vendor::Cloudflare));
    assert_eq!(p.signal.as_deref(), Some("cf-mitigated: challenge"));
    assert_eq!(p.status, Some(403));
    assert!(p.sample.is_some() && p.baseline.is_some());
    // Cloudflare refuses unverified copies of verified bots: the summary says so.
    assert!(f.summary.contains("AI Crawl Control"), "{}", f.summary);
    assert!(f.summary.contains("verified bot"), "{}", f.summary);
    assert!(f.summary.contains("logs"), "{}", f.summary);
    // Every bot's source, then Cloudflare's docs.
    assert!(
        f.sources
            .contains(&"https://developers.openai.com/api/docs/bots".to_owned())
    );
    for (vendor, url) in VENDOR_DOCS {
        if *vendor == Vendor::Cloudflare {
            assert!(f.sources.contains(&(*url).to_owned()), "{url}");
        }
    }
}

#[test]
fn a_403_for_one_bot_and_a_challenge_for_others_are_two_findings() {
    let report = probed(
        3,
        &[(
            0,
            &[
                ("OAI-SearchBot", Verdict::Blocked { status: Some(403) }),
                ("Claude-SearchBot", cf()),
                ("PerplexityBot", cf()),
            ],
        )],
    );
    let f = fetch_blocked(&report, &Intent::default());
    let subjects: Vec<&str> = f.iter().map(|f| f.subject.as_str()).collect();
    assert_eq!(subjects, ["blocked_403", "challenge_cloudflare"]);
    println!("{} | {}", f[0].title, f[0].summary);
    assert_eq!(f[0].evidence.probes.len(), 1);
    assert_eq!(f[0].evidence.probes[0].status, Some(403));
    assert!(f[0].title.contains("403"), "{}", f[0].title);
    // Not a Cloudflare page: no Cloudflare caveat.
    assert!(!f[0].summary.contains("Cloudflare"), "{}", f[0].summary);
    assert_eq!(f[1].evidence.probes.len(), 2);
}

#[test]
fn a_bot_the_owner_blocks_or_does_not_mind_is_not_reported() {
    let report = probed(
        3,
        &[(
            0,
            &[
                ("OAI-SearchBot", cf()),
                ("PerplexityBot", cf()),
                ("ChatGPT-User", Verdict::Blocked { status: Some(403) }),
            ],
        )],
    );
    let i = intent(r#"{"bots":{"OAI-SearchBot":"block","PerplexityBot":"any"}}"#);
    let f = fetch_blocked(&report, &i);
    let subjects: Vec<&str> = f.iter().map(|f| f.subject.as_str()).collect();
    assert_eq!(subjects, ["blocked_403"]);
    let i = intent(r#"{"purposes":{"search":"block","user_fetch":"any"}}"#);
    assert!(fetch_blocked(&report, &i).is_empty());
}

#[test]
fn the_home_page_is_a_warning_and_other_pages_a_notice() {
    let home = probed(3, &[(0, &[("OAI-SearchBot", cf())])]);
    assert_eq!(
        fetch_blocked(&home, &Intent::default())[0].severity,
        Severity::Warning
    );
    let inner = probed(
        3,
        &[
            (0, &[("OAI-SearchBot", Verdict::Same)]),
            (1, &[("OAI-SearchBot", cf())]),
            (2, &[("OAI-SearchBot", cf())]),
        ],
    );
    let f = fetch_blocked(&inner, &Intent::default());
    assert_eq!(f[0].severity, Severity::Notice);
    assert_eq!(
        f[0].title,
        "Cloudflare challenge served to OAI-SearchBot on 2 pages (synthetic check)"
    );
    let p = &f[0].evidence.probes[0];
    assert_eq!(
        (p.urls_affected, p.urls_probed, p.home_affected),
        (2, 3, false)
    );
    assert_eq!(f[0].evidence.urls.total, 2);
    // Throttling and a degraded page are never more than a notice, even on the home page.
    let soft = probed(
        3,
        &[(
            0,
            &[
                ("OAI-SearchBot", Verdict::Throttled),
                ("PerplexityBot", Verdict::Degraded),
            ],
        )],
    );
    let f = fetch_blocked(&soft, &Intent::default());
    let got: Vec<(&str, Severity)> = f.iter().map(|f| (f.subject.as_str(), f.severity)).collect();
    assert_eq!(
        got,
        [
            ("degraded", Severity::Notice),
            ("throttled", Severity::Notice)
        ]
    );
    // Never critical.
    let everything = probed(
        3,
        &[(
            0,
            &[
                ("OAI-SearchBot", Verdict::Blocked { status: None }),
                ("PerplexityBot", Verdict::Blocked { status: Some(451) }),
                (
                    "ChatGPT-User",
                    Verdict::Diverted {
                        to: "https://example.com/login".to_owned(),
                    },
                ),
            ],
        )],
    );
    let f = fetch_blocked(&everything, &Intent::default());
    assert_eq!(f.len(), 3);
    assert!(f.iter().all(|f| f.severity != Severity::Critical));
}

#[test]
fn all_inconclusive_probes_do_not_evaluate_the_kind() {
    let inconclusive = probed(
        3,
        &[(
            0,
            &[
                (
                    "OAI-SearchBot",
                    Verdict::Inconclusive {
                        reason: InconclusiveReason::BaselineFailed,
                    },
                ),
                ("PerplexityBot", Verdict::SkippedRobots),
            ],
        )],
    );
    assert!(!evaluated_kinds(&inconclusive).contains(&FindingKind::FetchBlocked));
    assert!(fetch_blocked(&inconclusive, &Intent::default()).is_empty());
    let fine = probed(3, &[(0, &[("OAI-SearchBot", Verdict::Same)])]);
    assert!(evaluated_kinds(&fine).contains(&FindingKind::FetchBlocked));
    assert!(fetch_blocked(&fine, &Intent::default()).is_empty());
    // No probes at all: not evaluated either.
    assert!(!evaluated_kinds(&report_for(&site(3, |p| p))).contains(&FindingKind::FetchBlocked));
}

#[test]
fn fetch_blocked_is_grade_b_synthetic_and_round_trips() {
    let report = probed(
        3,
        &[(
            0,
            &[
                ("OAI-SearchBot", Verdict::Throttled),
                ("PerplexityBot", Verdict::Degraded),
                (
                    "ChatGPT-User",
                    Verdict::Challenged {
                        vendor: Vendor::DataDome,
                    },
                ),
            ],
        )],
    );
    let f = fetch_blocked(&report, &Intent::default());
    assert_eq!(f.len(), 3);
    for f in &f {
        println!("{} | {}", f.title, f.summary);
        assert_eq!(f.grade, Grade::B);
        assert!(f.title.ends_with("(synthetic check)"), "{}", f.title);
        assert!(f.title.contains("synthetic"));
        assert!(f.evidence.synthetic);
    }
    let json = serde_json::to_string(&f).expect("json");
    assert!(json.contains(r#""grade":"B""#));
    assert!(json.contains(r#""kind":"fetch_blocked""#));
    assert!(json.contains(r#""synthetic":true"#));
    let back: Vec<Finding> = serde_json::from_str(&json).expect("back");
    assert_eq!(back, f);
    assert_eq!(
        FindingKind::from_slug("fetch_blocked"),
        Some(FindingKind::FetchBlocked)
    );
    assert_eq!(FindingKind::FetchBlocked.slug(), "fetch_blocked");
    // Phase A evidence still serialises without the probe fields.
    let plain = serde_json::to_value(Evidence::default()).expect("json");
    assert!(plain.get("probes").is_none() && plain.get("synthetic").is_none());
}

#[test]
fn a_broken_robots_file_hides_fetch_blocked() {
    let mut report = probed(3, &[(0, &[("OAI-SearchBot", cf())])]);
    let down = report_for(&with_robots(site(3, |p| p), 503, ""));
    report.robots = down.robots.clone();
    let f = findings(&report, &Intent::default());
    assert!(f.iter().any(|f| f.kind == FindingKind::RobotsUnavailable));
    assert!(f.iter().all(|f| f.kind != FindingKind::FetchBlocked));
}

// ---- Holding back bots a probe run could not judge (B1 review) -------------------------------

use codoseo_geo::findings::hold_unjudged_probes;
use codoseo_geo::incident::{OpenIncident, Transition, reconcile};

fn unstable() -> Verdict {
    Verdict::Inconclusive {
        reason: InconclusiveReason::Unstable,
    }
}

/// The open `fetch_blocked` incidents `before` would leave, as the store reads them back.
fn open_from(before: &[Finding]) -> (Vec<OpenIncident>, Vec<(String, Evidence)>) {
    let open = before
        .iter()
        .enumerate()
        .map(|(i, f)| OpenIncident {
            id: i as i64 + 1,
            kind: f.kind,
            subject: f.subject.clone(),
            severity: f.severity,
            members: f.evidence.members(),
        })
        .collect();
    let evidence = before
        .iter()
        .map(|f| (f.subject.clone(), f.evidence.clone()))
        .collect();
    (open, evidence)
}

/// The transitions of `after`'s report against the incidents `before` left open.
fn next(before: &[Finding], after: &AccessReport, intent: &Intent) -> Vec<Transition> {
    let (open, evidence) = open_from(before);
    let found: Vec<Finding> =
        hold_unjudged_probes(after, intent, &evidence, findings(after, intent))
            .into_iter()
            .filter(|f| f.kind == FindingKind::FetchBlocked)
            .collect();
    reconcile(&open, &found, &[FindingKind::FetchBlocked])
}

fn updated(t: &[Transition]) -> (&Finding, bool, bool) {
    match t {
        [
            Transition::Updated {
                finding,
                escalated,
                widened,
                ..
            },
        ] => (finding, *escalated, *widened),
        other => panic!("one update, got {other:#?}"),
    }
}

#[test]
fn a_bot_inconclusive_this_run_keeps_its_evidence_and_the_severity() {
    let first = probed(
        3,
        &[
            (
                0,
                &[("OAI-SearchBot", cf()), ("PerplexityBot", Verdict::Same)],
            ),
            (1, &[("OAI-SearchBot", cf()), ("PerplexityBot", cf())]),
        ],
    );
    let before = fetch_blocked(&first, &Intent::default());
    assert_eq!(before[0].severity, Severity::Warning);

    // OAI-SearchBot's home page re-fetch disagreed: it drops out of this run's finding, which
    // without it would be a notice about PerplexityBot alone.
    let second = probed(
        3,
        &[
            (
                0,
                &[
                    ("OAI-SearchBot", unstable()),
                    ("PerplexityBot", Verdict::Same),
                ],
            ),
            (1, &[("OAI-SearchBot", cf()), ("PerplexityBot", cf())]),
        ],
    );
    let raw = fetch_blocked(&second, &Intent::default());
    assert_eq!(
        raw[0].severity,
        Severity::Notice,
        "the flap this guards against"
    );
    let t = next(&before, &second, &Intent::default());
    let (f, escalated, widened) = updated(&t);
    assert!(!escalated && !widened);
    assert_eq!(f.severity, Severity::Warning, "{f:#?}");
    assert_eq!(f.evidence.members(), before[0].evidence.members());
    let oai = f
        .evidence
        .probes
        .iter()
        .find(|p| p.token == "OAI-SearchBot")
        .expect("OAI-SearchBot held");
    assert_eq!(oai, &before[0].evidence.probes[0], "its previous evidence");
    assert_eq!(f.title, before[0].title);

    // The run after that sees the home page again: back to the same incident, not escalated.
    let t = next(std::slice::from_ref(f), &first, &Intent::default());
    let (_, escalated, widened) = updated(&t);
    assert!(!escalated && !widened);
}

#[test]
fn a_bot_with_a_baseline_problem_neither_narrows_nor_resolves() {
    let first = probed(
        3,
        &[(0, &[("OAI-SearchBot", cf()), ("PerplexityBot", cf())])],
    );
    let before = fetch_blocked(&first, &Intent::default());
    let baseline_failed = Verdict::Inconclusive {
        reason: InconclusiveReason::BaselineFailed,
    };
    // Another page is fine for both, but the home page had no baseline: nobody is judged.
    let second = probed(
        3,
        &[
            (
                0,
                &[
                    ("OAI-SearchBot", baseline_failed.clone()),
                    ("PerplexityBot", baseline_failed),
                ],
            ),
            (
                1,
                &[
                    ("OAI-SearchBot", Verdict::Same),
                    ("PerplexityBot", Verdict::Same),
                ],
            ),
        ],
    );
    assert!(evaluated_kinds(&second).contains(&FindingKind::FetchBlocked));
    let t = next(&before, &second, &Intent::default());
    let (f, escalated, widened) = updated(&t);
    assert!(!escalated && !widened);
    assert_eq!(f, &before[0]);
}

#[test]
fn only_bots_judged_this_run_narrow_widen_or_resolve() {
    let first = probed(
        3,
        &[(0, &[("OAI-SearchBot", cf()), ("PerplexityBot", cf())])],
    );
    let before = fetch_blocked(&first, &Intent::default());

    // PerplexityBot is conclusively fine now; OAI-SearchBot could not be judged: narrowed to
    // OAI-SearchBot, not resolved.
    let second = probed(
        3,
        &[(
            0,
            &[
                ("OAI-SearchBot", unstable()),
                ("PerplexityBot", Verdict::Same),
            ],
        )],
    );
    let t = next(&before, &second, &Intent::default());
    let (f, escalated, widened) = updated(&t);
    assert!(!escalated && !widened);
    let members: Vec<String> = f.evidence.members().into_iter().collect();
    assert_eq!(members, ["oai-searchbot"]);
    assert!(f.title.contains("OAI-SearchBot") && !f.title.contains("PerplexityBot"));

    // A bot that is not judged this run does not widen the incident, even with a positive on
    // another page.
    let third = probed(
        3,
        &[
            (0, &[("OAI-SearchBot", cf()), ("ChatGPT-User", unstable())]),
            (1, &[("OAI-SearchBot", cf()), ("ChatGPT-User", cf())]),
        ],
    );
    let t = next(std::slice::from_ref(f), &third, &Intent::default());
    let (g, _, widened) = updated(&t);
    assert!(!widened, "{g:#?}");
    let members: Vec<String> = g.evidence.members().into_iter().collect();
    assert_eq!(members, ["oai-searchbot"]);

    // Resolved only once every bot it covers was judged and none has the cause.
    let fine = probed(
        3,
        &[(
            0,
            &[
                ("OAI-SearchBot", Verdict::Same),
                ("PerplexityBot", Verdict::SkippedRobots),
            ],
        )],
    );
    let t = next(&before, &fine, &Intent::default());
    assert!(
        matches!(t.as_slice(), [Transition::Resolved { .. }]),
        "{t:#?}"
    );
    let partly = probed(
        3,
        &[(
            0,
            &[
                ("OAI-SearchBot", Verdict::Same),
                ("PerplexityBot", unstable()),
            ],
        )],
    );
    let t = next(&before, &partly, &Intent::default());
    let (f, _, _) = updated(&t);
    let members: Vec<String> = f.evidence.members().into_iter().collect();
    assert_eq!(members, ["perplexitybot"]);
}

#[test]
fn a_held_bot_the_owner_no_longer_wants_is_dropped() {
    let first = probed(
        3,
        &[(0, &[("OAI-SearchBot", cf()), ("PerplexityBot", cf())])],
    );
    let before = fetch_blocked(&first, &Intent::default());
    let second = probed(
        3,
        &[(0, &[("OAI-SearchBot", unstable()), ("PerplexityBot", cf())])],
    );
    let i = intent(r#"{"bots":{"OAI-SearchBot":"block"}}"#);
    let t = next(&before, &second, &i);
    let (f, _, _) = updated(&t);
    let members: Vec<String> = f.evidence.members().into_iter().collect();
    assert_eq!(members, ["perplexitybot"]);
}

#[test]
fn urls_probed_counts_only_pages_the_bots_user_agent_was_sent_to() {
    let mut report = probed(
        3,
        &[
            (0, &[("OAI-SearchBot", cf())]),
            (
                1,
                &[(
                    "OAI-SearchBot",
                    Verdict::Inconclusive {
                        reason: InconclusiveReason::BaselineFailed,
                    },
                )],
            ),
            (2, &[("OAI-SearchBot", cf())]),
        ],
    );
    // The baseline failed on the second page, so the bot was never sent there.
    report.probes.as_mut().unwrap().urls[1].results[0].fetch = None;
    let f = fetch_blocked(&report, &Intent::default());
    let p = &f[0].evidence.probes[0];
    assert_eq!((p.urls_affected, p.urls_probed), (2, 2));
}

// ---- Content missing from the HTML as served (B2.5) ------------------------------------------

use codoseo_core::page::{MountPoint, PageRecord, RawSignals};
use codoseo_geo::content::{ContentVerdict, NON_RENDERING_BOTS, NON_RENDERING_SOURCE, ShellSignal};
use codoseo_geo::registry::registry;

/// A React shell: no words, an empty `#root`.
fn shell(mut p: PageRecord) -> PageRecord {
    p.fields.word_count = 0;
    p.fields.raw = RawSignals {
        scripts_external: 1,
        mount: Some(MountPoint::Root),
        mount_empty: true,
        ..RawSignals::default()
    };
    p
}

/// Pages that are shells: `/` when `home`, plus the first `others` of `/p1`, `/p2`...
fn shells(n: u32, home: bool, others: u32) -> CrawlOutput {
    let mut pages = vec![if home {
        shell(page("/", n))
    } else {
        page("/", n)
    }];
    for i in 1..n {
        let p = page(&format!("/p{i}"), n - i);
        pages.push(if i <= others { shell(p) } else { p });
    }
    crawl(pages)
}

fn content_missing(out: &CrawlOutput, intent: &Intent) -> Vec<Finding> {
    run(out, intent)
        .into_iter()
        .filter(|f| f.kind == FindingKind::ContentMissing)
        .collect()
}

#[test]
fn the_non_rendering_bots_are_in_the_registry() {
    for token in NON_RENDERING_BOTS {
        assert!(registry().bot(token).is_some(), "{token}");
    }
    assert!(NON_RENDERING_SOURCE.starts_with("https://vercel.com/"));
}

#[test]
fn a_shell_home_page_is_one_warning_with_grade_a_minus() {
    let f = content_missing(&shells(5, true, 0), &Intent::default());
    assert_eq!(f.len(), 1, "{f:#?}");
    let f = &f[0];
    println!("{} | {}", f.title, f.summary);
    assert_eq!(f.subject, "");
    assert_eq!(f.severity, Severity::Warning);
    assert_eq!(f.grade, Grade::AMinus);
    assert_eq!(
        f.title,
        "The home page is an empty app shell to AI crawlers that don't run JavaScript"
    );
    // The remedy comes first, so a clipped summary keeps it.
    assert!(
        f.summary.starts_with(
            "Render the main content on the server, or prerender the page, so it is in the HTML. In the HTML as served"
        ),
        "{}",
        f.summary
    );
    // The measurement is cited for the crawler it measured, not for the other fetchers.
    assert!(
        f.summary.contains(
            "Vercel and MERJ measured in 2024 that PerplexityBot doesn't run JavaScript."
        ),
        "{}",
        f.summary
    );
    assert!(
        f.summary.contains(
            "OAI-SearchBot, ChatGPT-User, Claude-SearchBot and 2 more come from the same operators as the crawlers they measured"
        ),
        "{}",
        f.summary
    );
    assert!(!f.summary.contains("that OAI-SearchBot"), "{}", f.summary);
    assert!(
        f.summary.contains("mount element is empty"),
        "{}",
        f.summary
    );
    // The search and user-fetch bots are wanted by default; GPTBot and ClaudeBot (training) are not.
    assert!(f.summary.contains("OAI-SearchBot"), "{}", f.summary);
    assert!(!f.summary.contains("that GPTBot"), "{}", f.summary);
    assert_eq!(f.sources, vec![NON_RENDERING_SOURCE.to_owned()]);
    let c = &f.evidence.content;
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].url, "https://example.com/");
    assert_eq!(c[0].verdict, ContentVerdict::Shell);
    assert_eq!(c[0].signals, [ShellSignal::MountEmpty]);
    assert_eq!(c[0].words, 0);
    assert_eq!(f.evidence.urls.total, 1);
}

#[test]
fn shells_below_the_home_page_are_a_notice_until_they_are_half() {
    let few = content_missing(&shells(6, false, 2), &Intent::default());
    assert_eq!(few.len(), 1);
    println!("{}", few[0].title);
    assert_eq!(few[0].severity, Severity::Notice);
    assert_eq!(
        few[0].title,
        "2 important pages are empty app shells to AI crawlers that don't run JavaScript"
    );
    let half = content_missing(&shells(6, false, 3), &Intent::default());
    assert_eq!(half[0].severity, Severity::Warning);
}

#[test]
fn a_likely_shell_is_worded_as_likely() {
    let mut p = page("/", 3);
    p.fields.word_count = 0;
    p.fields.raw.scripts_external = 4;
    let f = content_missing(
        &crawl(vec![p, page("/a", 2), page("/b", 1)]),
        &Intent::default(),
    );
    assert_eq!(f.len(), 1);
    assert_eq!(
        f[0].evidence.content[0].verdict,
        ContentVerdict::LikelyShell
    );
    assert_eq!(
        f[0].title,
        "The home page looks like an empty app shell to AI crawlers that don't run JavaScript"
    );
}

#[test]
fn thin_pages_and_present_pages_are_not_findings() {
    let thin = site(3, |mut p| {
        p.fields.word_count = 12;
        p.fields.h1 = vec!["Contact".to_owned()];
        p
    });
    let report = report_for(&thin);
    assert!(
        report
            .content
            .as_ref()
            .unwrap()
            .pages
            .iter()
            .all(|c| c.verdict == ContentVerdict::Thin)
    );
    assert!(content_missing(&thin, &Intent::default()).is_empty());
    assert!(content_missing(&site(3, |p| p), &Intent::default()).is_empty());
}

#[test]
fn no_finding_when_the_owner_wants_none_of_the_non_rendering_bots() {
    let out = shells(3, true, 2);
    let off = intent(r#"{"purposes":{"search":"block","user_fetch":"any"}}"#);
    assert!(content_missing(&out, &off).is_empty());
    // Wanting the training bots brings GPTBot and ClaudeBot in.
    let training =
        intent(r#"{"purposes":{"search":"block","user_fetch":"any","training":"allow"}}"#);
    let f = content_missing(&out, &training);
    assert_eq!(f.len(), 1);
    assert!(
        f[0].summary
            .contains("measured in 2024 that GPTBot and ClaudeBot don't run JavaScript."),
        "{}",
        f[0].summary
    );
    assert!(
        !f[0].summary.contains("from the same operators"),
        "{}",
        f[0].summary
    );
    // Only bots in the non-rendering list count: Googlebot-style renderers say nothing here.
    let others = intent(
        r#"{"purposes":{"search":"block","user_fetch":"block"},"bots":{"Applebot":"allow"}}"#,
    );
    assert!(content_missing(&out, &others).is_empty());
}

#[test]
fn pages_the_wanted_bots_may_not_fetch_are_left_out() {
    let wanted = [
        "OAI-SearchBot",
        "ChatGPT-User",
        "Claude-SearchBot",
        "Claude-User",
        "PerplexityBot",
        "Perplexity-User",
    ];
    // Every wanted non-rendering bot is kept off /p1; the home page and /p2 are still readable.
    let body: String = wanted
        .iter()
        .map(|t| format!("User-agent: {t}\nDisallow: /p1\n\n"))
        .collect();
    let out = with_robots(shells(4, true, 2), 200, &body);
    let f = content_missing(&out, &Intent::default());
    assert_eq!(f.len(), 1);
    let urls: Vec<&str> = f[0]
        .evidence
        .content
        .iter()
        .map(|c| c.url.as_str())
        .collect();
    assert_eq!(urls, ["https://example.com/", "https://example.com/p2"]);
    // One wanted bot that may still fetch /p1 is enough to count it.
    let some: String = wanted[1..]
        .iter()
        .map(|t| format!("User-agent: {t}\nDisallow: /p1\n\n"))
        .collect();
    let f = content_missing(
        &with_robots(shells(4, true, 2), 200, &some),
        &Intent::default(),
    );
    assert_eq!(f[0].evidence.content.len(), 3);
    // Blocked everywhere: nobody reads the shells, so there is nothing to report.
    let all = with_robots(shells(4, true, 2), 200, "User-agent: *\nDisallow: /\n");
    assert!(content_missing(&all, &Intent::default()).is_empty());
}

#[test]
fn no_robots_verdicts_means_content_missing_is_not_evaluated() {
    let down = report_for(&with_robots(shells(3, true, 2), 503, ""));
    assert!(!evaluated_kinds(&down).contains(&FindingKind::ContentMissing));
    assert!(
        findings(&down, &Intent::default())
            .iter()
            .all(|f| f.kind != FindingKind::ContentMissing)
    );
    // No HTML important page, no content report: not evaluated either.
    let pdf_only = {
        let mut p = page("/", 1);
        p.content_type = Some("application/pdf".to_owned());
        report_for(&crawl(vec![p]))
    };
    assert!(pdf_only.content.is_none());
    assert!(!evaluated_kinds(&pdf_only).contains(&FindingKind::ContentMissing));
}

#[test]
fn more_shell_pages_widen_the_incident() {
    use codoseo_geo::incident::{OpenIncident, Transition, reconcile};
    let before = content_missing(&shells(6, false, 1), &Intent::default());
    let members: Vec<String> = before[0].evidence.members().into_iter().collect();
    assert_eq!(members, ["url:/p1"]);
    let open = vec![OpenIncident {
        id: 1,
        kind: FindingKind::ContentMissing,
        subject: String::new(),
        severity: before[0].severity,
        members: before[0].evidence.members(),
    }];
    let after = content_missing(&shells(6, false, 2), &Intent::default());
    let t = reconcile(&open, &after, &[FindingKind::ContentMissing]);
    match t.as_slice() {
        [Transition::Updated { widened, .. }] => assert!(*widened),
        other => panic!("one update, got {other:#?}"),
    }
    // Fewer pages is not a widening.
    let t = reconcile(
        &[OpenIncident {
            members: after[0].evidence.members(),
            ..open[0].clone()
        }],
        &before,
        &[FindingKind::ContentMissing],
    );
    match t.as_slice() {
        [Transition::Updated { widened, .. }] => assert!(!*widened),
        other => panic!("one update, got {other:#?}"),
    }
}

#[test]
fn content_missing_serialises_with_grade_a_minus() {
    let f = content_missing(&shells(3, true, 0), &Intent::default());
    let json = serde_json::to_string(&f).expect("json");
    assert!(json.contains(r#""grade":"A-""#), "{json}");
    assert!(json.contains(r#""kind":"content_missing""#), "{json}");
    let back: Vec<Finding> = serde_json::from_str(&json).expect("back");
    assert_eq!(back, f);
    assert_eq!(serde_json::to_string(&Grade::AMinus).unwrap(), r#""A-""#);
    assert_eq!(
        FindingKind::from_slug("content_missing"),
        Some(FindingKind::ContentMissing)
    );
    assert_eq!(FindingKind::ContentMissing.slug(), "content_missing");
}

// ---- llms.txt unavailable --------------------------------------------------------------------

mod llms_txt {
    use super::*;
    use codoseo_core::output::LlmsTxtFetch;
    use codoseo_geo::llms::{LlmsState, carry_adoption};

    fn with_llms(status: u16, error: Option<&str>) -> CrawlOutput {
        let mut out = site(3, |p| p);
        out.signals.llms_txt = Some(LlmsTxtFetch {
            status,
            error: error.map(str::to_owned),
            content_type: Some("text/plain".into()),
            body: if (200..300).contains(&status) {
                "# Site\n\n> About the site.\n\n## Docs\n\n- [A](https://example.com/a): a page\n"
                    .into()
            } else {
                String::new()
            },
            ..LlmsTxtFetch::default()
        });
        out
    }

    /// The report of `now`, with adoption carried from the report of `prev` as the worker does.
    fn after(prev: &CrawlOutput, now: &CrawlOutput) -> AccessReport {
        let prev = report_for(prev);
        let mut report = report_for(now);
        if let Some(l) = report.llms_txt.as_mut() {
            l.adopted = carry_adoption(prev.llms_txt.as_ref(), l);
        }
        report
    }

    fn llms_findings(report: &AccessReport) -> Vec<Finding> {
        findings(report, &Intent::default())
            .into_iter()
            .filter(|f| f.kind == FindingKind::LlmsTxtUnavailable)
            .collect()
    }

    #[test]
    fn the_slug_round_trips() {
        let k = FindingKind::LlmsTxtUnavailable;
        assert_eq!(k.slug(), "llms_txt_unavailable");
        assert_eq!(FindingKind::from_slug("llms_txt_unavailable"), Some(k));
    }

    #[test]
    fn a_first_ever_failure_is_no_finding() {
        // A local run or a first crawl has no history: a 5xx is not a file the site lost.
        for out in [
            with_llms(503, None),
            with_llms(0, Some("request timed out")),
        ] {
            let report = report_for(&out);
            assert_eq!(
                report.llms_txt.as_ref().unwrap().state,
                LlmsState::Unavailable
            );
            assert!(llms_findings(&report).is_empty());
            assert!(evaluated_kinds(&report).contains(&FindingKind::LlmsTxtUnavailable));
        }
    }

    #[test]
    fn an_adopted_file_that_fails_opens_a_warning() {
        let report = after(&with_llms(200, None), &with_llms(503, None));
        let f = llms_findings(&report);
        assert_eq!(f.len(), 1, "{f:?}");
        let f = &f[0];
        println!("{} | {}", f.title, f.summary);
        assert_eq!(f.subject, "");
        assert_eq!(f.severity, Severity::Warning);
        assert_eq!(f.grade, Grade::A);
        assert_eq!(f.title, "llms.txt returns HTTP 503");
        assert_eq!(f.evidence.llms_status, Some(503));
        assert!(f.summary.contains("informational"), "{}", f.summary);
        // The remedy first: API and MCP clients clip summaries.
        assert!(
            f.summary.starts_with(
                "Make /llms.txt answer 200 again, or 404 if you removed it on purpose."
            ),
            "{}",
            f.summary
        );
        assert!(f.evidence.members().is_empty());
        assert!(f.sources.iter().any(|s| s.contains("llmstxt.org")));
        assert!(f.sources.iter().any(|s| s.contains("lighthouse")));
    }

    #[test]
    fn a_failure_without_a_response_names_the_error() {
        let report = after(
            &with_llms(200, None),
            &with_llms(0, Some("request timed out")),
        );
        let f = llms_findings(&report);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].title, "llms.txt could not be fetched");
        assert!(
            f[0].summary.contains("request timed out"),
            "{}",
            f[0].summary
        );
        assert_eq!(f[0].evidence.llms_status, None);
        // 429 is a failure too, as in Lighthouse.
        let report = after(&with_llms(200, None), &with_llms(429, None));
        assert_eq!(llms_findings(&report).len(), 1);
    }

    #[test]
    fn it_is_never_critical() {
        for (status, error) in [
            (500, None),
            (502, None),
            (429, None),
            (0, Some("connection failed")),
        ] {
            let report = after(&with_llms(200, None), &with_llms(status, error));
            for f in findings(&report, &Intent::default()) {
                if f.kind == FindingKind::LlmsTxtUnavailable {
                    assert_ne!(f.severity, Severity::Critical);
                }
            }
        }
    }

    #[test]
    fn adoption_survives_repeated_failures() {
        let first = after(&with_llms(200, None), &with_llms(503, None));
        // The next failure carries adoption from the failing report.
        let mut second = report_for(&with_llms(500, None));
        let l = second.llms_txt.as_mut().unwrap();
        l.adopted = carry_adoption(first.llms_txt.as_ref(), l);
        assert_eq!(llms_findings(&second).len(), 1);
    }

    #[test]
    fn removal_or_recovery_resolves_it() {
        // Evaluated with no finding: an open incident resolves.
        for now in [
            with_llms(404, None),
            with_llms(410, None),
            with_llms(200, None),
        ] {
            let report = after(&with_llms(200, None), &now);
            assert!(evaluated_kinds(&report).contains(&FindingKind::LlmsTxtUnavailable));
            assert!(llms_findings(&report).is_empty());
        }
        // A soft 404 is a removal too.
        let mut soft = with_llms(200, None);
        let l = soft.signals.llms_txt.as_mut().unwrap();
        l.content_type = Some("text/html".into());
        l.body = "<!doctype html><title>404</title>".into();
        let report = after(&with_llms(200, None), &soft);
        assert_eq!(
            report.llms_txt.as_ref().unwrap().state,
            LlmsState::SoftNotFound
        );
        assert!(llms_findings(&report).is_empty());
    }

    #[test]
    fn not_fetched_means_not_evaluated() {
        let report = report_for(&site(3, |p| p));
        assert!(!evaluated_kinds(&report).contains(&FindingKind::LlmsTxtUnavailable));
    }

    #[test]
    fn the_health_score_ignores_llms_txt() {
        let score = |mut out: CrawlOutput| codoseo_checks::run_checks(&mut out).health_score;
        let base = score(site(3, |p| p));
        assert_eq!(score(with_llms(503, None)), base);
        assert_eq!(score(with_llms(0, Some("request timed out"))), base);
        assert_eq!(score(with_llms(404, None)), base);
        assert_eq!(score(with_llms(200, None)), base);
    }
}

// ---- Holding back pages a run could not judge (B2 review) ------------------------------------

use codoseo_geo::findings::hold_unjudged_content;

/// The `ContentMissing` transitions of `after` against the incident `before` left open.
fn next_content(before: &[Finding], after: &CrawlOutput) -> Vec<Transition> {
    let intent = Intent::default();
    let report = report_for(after);
    let open: Vec<OpenIncident> = before
        .iter()
        .map(|f| OpenIncident {
            id: 1,
            kind: f.kind,
            subject: f.subject.clone(),
            severity: f.severity,
            members: f.evidence.members(),
        })
        .collect();
    let found: Vec<Finding> = hold_unjudged_content(
        &report,
        &intent,
        before.first().map(|f| &f.evidence),
        findings(&report, &intent),
    )
    .into_iter()
    .filter(|f| f.kind == FindingKind::ContentMissing)
    .collect();
    reconcile(&open, &found, &evaluated_kinds(&report))
}

/// `shells(n, home, others)` with `/p{i}` for each `i` in `failed` answering 503.
fn shells_with_failures(n: u32, home: bool, others: u32, failed: &[u32]) -> CrawlOutput {
    let mut out = shells(n, home, others);
    for &i in failed {
        let p = &mut out.pages[i as usize];
        p.status = 503;
        p.fields.word_count = 0;
        p.fields.raw = RawSignals::default();
    }
    out
}

#[test]
fn a_shell_page_that_failed_this_run_keeps_the_incident_open() {
    let before = content_missing(&shells(6, false, 1), &Intent::default());
    assert_eq!(before.len(), 1);
    // /p1 answers 503: no raw content to judge, so the incident stays as it was.
    let t = next_content(&before, &shells_with_failures(6, false, 0, &[1]));
    let (finding, escalated, widened) = updated(&t);
    assert!(!escalated && !widened);
    assert_eq!(finding.evidence.content, before[0].evidence.content);
    assert_eq!(finding.title, before[0].title);
}

#[test]
fn a_held_page_joins_this_runs_shells_and_judged_pages_still_resolve() {
    let before = content_missing(&shells(6, false, 2), &Intent::default());
    // /p1 fails and /p2 is fixed: /p1 is held, /p2 leaves.
    let t = next_content(&before, &shells_with_failures(6, false, 0, &[1]));
    let (finding, _, widened) = updated(&t);
    assert!(!widened);
    let urls: Vec<&str> = finding
        .evidence
        .content
        .iter()
        .map(|c| c.url.as_str())
        .collect();
    assert_eq!(urls, ["https://example.com/p1"]);
    // /p1 judged fine: resolved.
    let t = next_content(&before, &shells(6, false, 0));
    assert!(
        matches!(t.as_slice(), [Transition::Resolved { .. }]),
        "{t:#?}"
    );
}

#[test]
fn a_page_no_longer_important_leaves_the_incident() {
    let before = content_missing(&shells(6, false, 1), &Intent::default());
    // /p1 is gone from the crawl, so it is not an important page any more.
    let mut after = shells(6, false, 0);
    after.pages.remove(1);
    let t = next_content(&before, &after);
    assert!(
        matches!(t.as_slice(), [Transition::Resolved { .. }]),
        "{t:#?}"
    );
}

#[test]
fn a_held_page_no_wanted_bot_may_fetch_now_is_dropped() {
    let before = content_missing(&shells(4, false, 1), &Intent::default());
    let after = with_robots(
        shells_with_failures(4, false, 0, &[1]),
        200,
        "User-agent: *\nDisallow: /p1\n",
    );
    let t = next_content(&before, &after);
    assert!(
        matches!(t.as_slice(), [Transition::Resolved { .. }]),
        "{t:#?}"
    );
}
