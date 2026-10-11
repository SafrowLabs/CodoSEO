//! The synthetic probe executor against local fake sites: headers, robots.txt, retries, pacing,
//! the budget, the request cap, confirmation re-fetches and the address guard. No network.
//!
//! These run on the real clock with short gaps: a paused tokio clock jumps ahead while a request
//! waits on the local socket and fires the client's timeout. The 10 s caps and the 2 s default
//! retry wait are tested on the pure helpers in `src/probe.rs`.

use std::time::Duration;

use codoseo_core::Url;
use codoseo_core::check::IssueBits;
use codoseo_core::crawl::{AddressPolicy, PROBE_FROM, RobotsFile, SitemapSummary, USER_AGENT};
use codoseo_core::output::{CrawlOutput, LinkGraph, SiteSignals, StopReason};
use codoseo_core::page::{Indexability, PageFields, PageRecord};
use codoseo_core::url::url_hash;
use codoseo_crawler::probe::{ProbeConfig, probe_page, run_probes, select_bots};
use codoseo_geo::probe::{
    FetchErrorKind, InconclusiveReason, ProbeBot, ProbePlan, ProbeReport, ProbeResult, ProbeStop,
    Vendor, Verdict, candidates, plan_probes,
};
use codoseo_geo::report::{build_report, important_urls};
use codoseo_testkit::{Page, SiteBuilder, TestSite, html_page};
use tokio::time::Instant;

const OAI: &str = "OAI-SearchBot";
const PPLX: &str = "PerplexityBot";
const DUCK: &str = "DuckAssistBot";

fn bot(token: &str) -> ProbeBot {
    candidates()
        .0
        .into_iter()
        .find(|b| b.token == token)
        .unwrap_or_else(|| panic!("{token} is a probe candidate with a user agent"))
}

fn bots() -> Vec<ProbeBot> {
    [OAI, PPLX, DUCK].into_iter().map(bot).collect()
}

fn ok_page(title: &str) -> Page {
    Page::html(&html_page(title, &[]))
}

/// A config probing `paths` (URL indexes 0, 1, …) on `site` with `bots`, a 50 ms gap, a generous
/// budget and request cap, and private addresses allowed.
fn config(site: &TestSite, paths: &[&str], bots: Vec<ProbeBot>) -> ProbeConfig {
    let origin_urls: Vec<(u16, Url)> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| (i as u16, site.url(p)))
        .collect();
    ProbeConfig {
        plan: ProbePlan {
            urls: origin_urls.iter().map(|(i, _)| *i).collect(),
            bots,
            skipped_bots: Vec::new(),
            crawl_delay: None,
        },
        origin_urls,
        disallowed: Vec::new(),
        from: PROBE_FROM.to_owned(),
        address_policy: AddressPolicy::AllowPrivate,
        budget: Duration::from_secs(600),
        max_requests: 1000,
        min_gap: Duration::from_millis(50),
        request_timeout: Duration::from_secs(5),
    }
}

fn result<'a>(report: &'a ProbeReport, url: usize, token: &str) -> &'a ProbeResult {
    report.urls[url]
        .results
        .iter()
        .find(|r| r.token == token)
        .unwrap_or_else(|| panic!("a result for {token}"))
}

fn verdict(report: &ProbeReport, url: usize, token: &str) -> Verdict {
    result(report, url, token).verdict.clone()
}

#[tokio::test]
async fn every_request_carries_the_verbatim_user_agent_and_from() {
    let site = SiteBuilder::new().every_path(ok_page("Home")).start().await;
    let report = run_probes(config(&site, &["/", "/a"], bots())).await;

    let seen = site.requests();
    assert_eq!(seen.len(), 8, "{seen:?}");
    let mut want = Vec::new();
    for path in ["/", "/a"] {
        want.push((path.to_owned(), USER_AGENT.to_owned()));
        for b in bots() {
            want.push((path.to_owned(), b.user_agent));
        }
    }
    let got: Vec<(String, String)> = seen
        .iter()
        .map(|r| (r.path.clone(), r.user_agent.clone().unwrap_or_default()))
        .collect();
    assert_eq!(got, want, "baseline first, then each bot in plan order");
    assert!(
        seen.iter().all(|r| r.from.as_deref() == Some(PROBE_FROM)),
        "{seen:?}"
    );

    assert!(report.synthetic);
    assert_eq!(report.from, PROBE_FROM);
    assert_eq!(report.bots, [OAI, PPLX, DUCK]);
    assert_eq!(report.requests, 8);
    assert_eq!(report.stop, None);
    assert_eq!(report.urls.len(), 2);
    for u in &report.urls {
        assert!(u.baseline.is_html_ok(), "{:?}", u.baseline);
        assert_eq!(u.baseline.title.as_deref(), Some("Home"));
        assert_eq!(u.results.len(), 3);
        assert!(u.results.iter().all(|r| r.verdict == Verdict::Same));
    }
}

#[tokio::test]
async fn headers_are_sent_on_every_redirect_hop() {
    let site = SiteBuilder::new()
        .page("/old", Page::redirect(301, "/new"))
        .page("/new", ok_page("New"))
        .start()
        .await;
    let report = run_probes(config(&site, &["/old"], vec![bot(OAI)])).await;
    let seen = site.requests();
    assert_eq!(seen.len(), 4, "{seen:?}");
    assert!(seen.iter().all(|r| r.from.as_deref() == Some(PROBE_FROM)));
    let oai: Vec<&str> = seen
        .iter()
        .filter(|r| r.user_agent.as_deref() == Some(bot(OAI).user_agent.as_str()))
        .map(|r| r.path.as_str())
        .collect();
    assert_eq!(oai, ["/old", "/new"]);
    let base = &report.urls[0].baseline;
    assert_eq!(base.redirects, 1);
    assert_eq!(base.final_url.as_deref(), Some(site.url("/new").as_str()));
    assert_eq!(verdict(&report, 0, OAI), Verdict::Same);
}

/// A crawl of `site` with the pages given, `/` first, and the robots.txt body.
fn crawl_of(site: &TestSite, paths: &[&str], robots: &str) -> CrawlOutput {
    let pages = paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let url = site.url(p);
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
                fields: PageFields::default(),
                inlinks: 10 - i as u32,
                outlinks_internal: 0,
                outlinks_external: 0,
                issues: IssueBits::default(),
                key_hash: 0,
                redirect_target: None,
                outlinks_nofollow: 0,
                error: None,
            }
        })
        .collect();
    CrawlOutput {
        origin: site.url("/"),
        pages,
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
    }
}

#[tokio::test]
async fn a_token_robots_txt_disallows_is_never_requested() {
    // The server would refuse PerplexityBot, but robots.txt already keeps it off /private.
    let site = SiteBuilder::new()
        .every_path(Page::by_user_agent(
            vec![(PPLX, Page::status(403, "<title>Forbidden</title>"))],
            ok_page("Page"),
        ))
        .start()
        .await;
    let out = crawl_of(
        &site,
        &["/", "/private"],
        "User-agent: PerplexityBot\nDisallow: /\n\nUser-agent: *\nAllow: /\n",
    );
    let important = important_urls(&out.pages, &out.origin, &Default::default());
    let report = build_report(&out, &important);
    let mut plan = plan_probes(&report, None, 5).expect("a plan");
    plan.bots
        .retain(|b| [OAI, PPLX].contains(&b.token.as_str()));
    let cfg = ProbeConfig::new(
        &report,
        plan,
        AddressPolicy::AllowPrivate,
        Duration::from_secs(600),
        100,
    );
    assert_eq!(cfg.from, PROBE_FROM);
    assert_eq!(cfg.min_gap, Duration::from_millis(500));
    assert_eq!(cfg.origin_urls.len(), 2);

    let probes = run_probes(cfg).await;
    assert_eq!(site.hits_from(PPLX), 0);
    assert_eq!(site.hits_from(OAI), 2);
    assert_eq!(probes.urls.len(), 2);
    for u in 0..2 {
        let r = result(&probes, u, PPLX);
        assert_eq!(r.verdict, Verdict::SkippedRobots);
        assert_eq!(r.fetch, None);
        assert_eq!(verdict(&probes, u, OAI), Verdict::Same);
    }
    assert_eq!(probes.requests, 4);
}

#[tokio::test]
async fn a_429_then_200_is_retried_once_and_ends_the_same() {
    let site = SiteBuilder::new()
        .every_path(Page::by_user_agent(
            vec![(
                OAI,
                Page::sequence(vec![
                    Page::status(429, "slow down").header("retry-after", "1"),
                    ok_page("Home"),
                ]),
            )],
            ok_page("Home"),
        ))
        .start()
        .await;
    let start = Instant::now();
    let report = run_probes(config(&site, &["/"], vec![bot(OAI)])).await;
    let r = result(&report, 0, OAI);
    assert_eq!(r.verdict, Verdict::Same);
    let fetch = r.fetch.as_ref().expect("fetched");
    assert!(fetch.retried);
    assert_eq!(fetch.status, Some(200));
    assert_eq!(site.hits_from(OAI), 2);
    assert_eq!(report.requests, 3);
    // The retry waits the 1 s the 429 asked for.
    let took = start.elapsed();
    assert!(
        took >= Duration::from_millis(1_050) && took < Duration::from_secs(3),
        "{took:?}"
    );
}

#[tokio::test]
async fn a_timeout_is_retried_once_then_inconclusive() {
    let site = SiteBuilder::new()
        .every_path(Page::by_user_agent(
            vec![(OAI, ok_page("Home").delayed(Duration::from_secs(3)))],
            ok_page("Home"),
        ))
        .start()
        .await;
    let mut cfg = config(&site, &["/"], vec![bot(OAI)]);
    cfg.request_timeout = Duration::from_millis(500);
    let start = Instant::now();
    let report = run_probes(cfg).await;
    // Two 0.5 s timeouts and the default 2 s wait between them.
    assert!(
        start.elapsed() >= Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
    let r = result(&report, 0, OAI);
    assert_eq!(
        r.verdict,
        Verdict::Inconclusive {
            reason: InconclusiveReason::ProbeError
        }
    );
    let fetch = r.fetch.as_ref().expect("attempted");
    assert_eq!(fetch.error, Some(FetchErrorKind::Timeout));
    assert!(fetch.retried);
    assert_eq!(site.hits_from(OAI), 2);
}

#[tokio::test]
async fn crawl_delay_and_the_minimum_gap_space_every_request() {
    let site = SiteBuilder::new().every_path(ok_page("Home")).start().await;

    let mut cfg = config(&site, &["/"], vec![bot(OAI), bot(PPLX)]);
    cfg.plan.crawl_delay = Some(0.4);
    let start = Instant::now();
    run_probes(cfg).await;
    let took = start.elapsed();
    assert!(
        took >= Duration::from_millis(800),
        "three requests 0.4 s apart: {took:?}"
    );

    // A crawl delay shorter than the minimum gap doesn't speed things up.
    let mut cfg = config(&site, &["/"], bots());
    cfg.min_gap = Duration::from_millis(300);
    cfg.plan.crawl_delay = Some(0.1);
    let start = Instant::now();
    run_probes(cfg).await;
    let took = start.elapsed();
    assert!(
        took >= Duration::from_millis(900),
        "four requests 0.3 s apart: {took:?}"
    );
}

#[tokio::test]
async fn the_budget_stops_between_urls_never_inside_one() {
    let site = SiteBuilder::new().every_path(ok_page("Home")).start().await;
    // Four requests per URL at 0.3 s need 1.2 s; the first URL takes 0.9 s, which leaves less
    // than that for the second.
    let mut cfg = config(&site, &["/", "/a", "/b"], bots());
    cfg.min_gap = Duration::from_millis(300);
    cfg.budget = Duration::from_millis(1_800);
    let report = run_probes(cfg).await;
    assert_eq!(report.stop, Some(ProbeStop::Budget));
    assert_eq!(report.urls.len(), 1);
    assert_eq!(report.urls[0].results.len(), 3);
    assert_eq!(site.hits(), 4);
    assert_eq!(site.path_hits("/a"), 0);

    // No budget for even one URL: nothing is requested.
    let mut cfg = config(&site, &["/"], bots());
    cfg.min_gap = Duration::from_millis(300);
    cfg.budget = Duration::from_secs(1);
    let report = run_probes(cfg).await;
    assert_eq!(report.stop, Some(ProbeStop::Budget));
    assert!(report.urls.is_empty());
    assert_eq!(report.requests, 0);
    assert_eq!(site.hits(), 4);
}

#[tokio::test]
async fn the_request_cap_holds_with_retries_and_confirmations() {
    let site = SiteBuilder::new().every_path(ok_page("Home")).start().await;
    let mut cfg = config(&site, &["/", "/a", "/b"], bots());
    cfg.max_requests = 10;
    let report = run_probes(cfg).await;
    assert_eq!(report.stop, Some(ProbeStop::RequestCap));
    assert_eq!(report.urls.len(), 2);
    assert_eq!(report.requests, 8);
    assert_eq!(site.hits(), 8);

    // A bot that is always throttled wants a retry and a confirmation; the cap leaves room for
    // the retry only, after the other bots' planned requests are kept aside.
    let site = SiteBuilder::new()
        .every_path(Page::by_user_agent(
            vec![(
                OAI,
                Page::status(429, "slow down").header("retry-after", "1"),
            )],
            ok_page("Home"),
        ))
        .start()
        .await;
    let mut cfg = config(&site, &["/"], bots());
    cfg.max_requests = 5;
    let report = run_probes(cfg).await;
    assert_eq!(report.stop, None);
    assert_eq!(report.requests, 5);
    assert_eq!(site.hits(), 5);
    assert_eq!(site.hits_from(OAI), 2);
    assert_eq!(verdict(&report, 0, OAI), Verdict::Throttled);
    assert_eq!(verdict(&report, 0, PPLX), Verdict::Same);
    assert_eq!(verdict(&report, 0, DUCK), Verdict::Same);
}

#[tokio::test]
async fn the_first_positive_per_bot_and_cause_is_confirmed_once() {
    let site = SiteBuilder::new()
        .every_path(Page::by_user_agent(
            vec![(OAI, Page::status(403, "<title>Forbidden</title>"))],
            ok_page("Home"),
        ))
        .start()
        .await;
    let report = run_probes(config(&site, &["/", "/a"], vec![bot(OAI), bot(PPLX)])).await;
    for u in 0..2 {
        assert_eq!(
            verdict(&report, u, OAI),
            Verdict::Blocked { status: Some(403) }
        );
        assert_eq!(verdict(&report, u, PPLX), Verdict::Same);
    }
    // Confirmed on the first page (two fetches), taken as is on the second.
    assert_eq!(site.path_hits("/"), 4);
    assert_eq!(site.path_hits("/a"), 3);
    assert_eq!(site.hits_from(OAI), 3);
    assert_eq!(report.requests, 7);
}

#[tokio::test]
async fn a_positive_the_re_fetch_disagrees_with_is_unstable() {
    let site = SiteBuilder::new()
        .every_path(Page::by_user_agent(
            vec![(
                OAI,
                Page::sequence(vec![
                    Page::status(403, "<title>Forbidden</title>"),
                    ok_page("Home"),
                ]),
            )],
            ok_page("Home"),
        ))
        .start()
        .await;
    let report = run_probes(config(&site, &["/"], vec![bot(OAI)])).await;
    let r = result(&report, 0, OAI);
    assert_eq!(
        r.verdict,
        Verdict::Inconclusive {
            reason: InconclusiveReason::Unstable
        }
    );
    assert_eq!(r.fetch.as_ref().and_then(|f| f.status), Some(200));
    assert_eq!(site.hits_from(OAI), 2);
}

#[tokio::test]
async fn challenges_and_dropped_connections_are_confirmed_too() {
    let site = SiteBuilder::new()
        .every_path(Page::by_user_agent(
            vec![
                (
                    OAI,
                    Page::status(403, "<title>Just a moment...</title>")
                        .header("cf-mitigated", "challenge"),
                ),
                (PPLX, Page::drop_connection()),
            ],
            ok_page("Home"),
        ))
        .start()
        .await;
    let report = run_probes(config(&site, &["/"], vec![bot(OAI), bot(PPLX)])).await;
    assert_eq!(
        verdict(&report, 0, OAI),
        Verdict::Challenged {
            vendor: Vendor::Cloudflare
        }
    );
    let challenge = result(&report, 0, OAI)
        .fetch
        .as_ref()
        .and_then(|f| f.challenge.clone())
        .expect("a challenge");
    assert_eq!(challenge.signal, "cf-mitigated: challenge");
    assert_eq!(site.hits_from(OAI), 2);
    // Dropped, retried, and the same again on the confirmation.
    assert_eq!(verdict(&report, 0, PPLX), Verdict::Blocked { status: None });
    assert_eq!(site.hits_from(PPLX), 4);
}

#[tokio::test]
async fn bots_are_not_requested_when_the_baseline_has_nothing_to_compare() {
    let site = SiteBuilder::new()
        .every_path(Page::by_user_agent(
            vec![(
                "CodoSEObot",
                Page::status(503, "<title>Down</title>").header("retry-after", "0"),
            )],
            ok_page("Home"),
        ))
        .start()
        .await;
    let report = run_probes(config(&site, &["/"], bots())).await;
    assert_eq!(report.urls[0].baseline.status, Some(503));
    assert!(report.urls[0].baseline.retried);
    for b in [OAI, PPLX, DUCK] {
        let r = result(&report, 0, b);
        assert_eq!(
            r.verdict,
            Verdict::Inconclusive {
                reason: InconclusiveReason::BaselineFailed
            }
        );
        assert_eq!(r.fetch, None);
        assert_eq!(site.hits_from(b), 0);
    }
    assert_eq!(report.requests, 2);
    assert!(!report.conclusive());
}

#[tokio::test]
async fn the_address_guard_holds_on_every_probe() {
    // The test site lives on 127.0.0.1 and redirects to a private address. Under the public
    // policy neither the site nor the redirect target is reached, for the baseline or a bot.
    let site = SiteBuilder::new()
        .every_path(Page::redirect(302, "http://10.0.0.1/admin"))
        .start()
        .await;
    let mut cfg = config(&site, &["/"], bots());
    cfg.address_policy = AddressPolicy::Public;
    let report = run_probes(cfg).await;
    assert_eq!(site.hits(), 0);
    let base = &report.urls[0].baseline;
    assert_eq!(base.error, Some(FetchErrorKind::Blocked));
    assert!(
        report.urls[0]
            .results
            .iter()
            .all(|r| r.fetch.is_none() && !r.verdict.is_positive())
    );
}

// ---- One page on its own (`codoseo probe`, the local MCP's `probe_ai_fetch`) ----------------

fn tokens(list: &[&str]) -> Vec<String> {
    list.iter().map(|t| (*t).to_owned()).collect()
}

#[test]
fn select_bots_matches_tokens_case_insensitively_in_registry_order() {
    let picked = select_bots(&tokens(&["perplexitybot", "OAI-SearchBot"])).expect("known tokens");
    let names: Vec<&str> = picked.iter().map(|b| b.token.as_str()).collect();
    assert_eq!(names, [OAI, PPLX]);
    assert_eq!(picked[0].user_agent, bot(OAI).user_agent);
}

#[test]
fn select_bots_rejects_unknown_and_unprobeable_tokens_listing_the_valid_ones() {
    let err = select_bots(&tokens(&["OAI-SearchBot", "NoSuchBot"])).unwrap_err();
    assert!(err.contains("NoSuchBot"), "{err}");
    for valid in [OAI, PPLX, DUCK] {
        assert!(err.contains(valid), "{err}");
    }
    let err = select_bots(&tokens(&["Googlebot"])).unwrap_err();
    assert!(err.contains("Googlebot"), "{err}");
    assert!(err.contains("verified search engine"), "{err}");
    let err = select_bots(&tokens(&["GPTBot"])).unwrap_err();
    assert!(
        err.contains("GPTBot") && err.contains("not probed"),
        "{err}"
    );
    assert!(select_bots(&[]).is_err(), "an empty list is a mistake");
}

#[tokio::test]
async fn probe_page_probes_one_page_against_its_robots_txt() {
    let site = SiteBuilder::new()
        .robots(
            200,
            "User-agent: DuckAssistBot\nDisallow: /\n\nUser-agent: *\nAllow: /\n",
        )
        .every_path(Page::by_user_agent(
            vec![(
                PPLX,
                Page::status(403, "<title>Just a moment...</title>")
                    .header("cf-mitigated", "challenge"),
            )],
            ok_page("Pricing"),
        ))
        .start()
        .await;
    let url = site.url("/pricing");
    let probed = probe_page(
        &url,
        Some(&tokens(&[OAI, PPLX, DUCK])),
        AddressPolicy::AllowPrivate,
        PROBE_FROM,
        Duration::from_millis(20),
    )
    .await
    .expect("a probe");
    let probes = probed.report.probes.as_ref().expect("probes in the report");
    assert!(probes.synthetic);
    assert_eq!(probes.bots, [OAI, PPLX, DUCK]);
    assert_eq!(probes.urls.len(), 1);
    let idx = usize::from(probes.urls[0].url);
    assert_eq!(probed.report.important[idx].url, url.as_str());
    assert_eq!(verdict(probes, 0, OAI), Verdict::Same);
    assert_eq!(
        verdict(probes, 0, PPLX),
        Verdict::Challenged {
            vendor: Vendor::Cloudflare
        }
    );
    assert_eq!(verdict(probes, 0, DUCK), Verdict::SkippedRobots);
    assert_eq!(site.hits_from(DUCK), 0);
    // Only the page: the home page is not requested.
    assert!(
        site.requests().iter().all(|r| r.path != "/"),
        "{:?}",
        site.requests()
    );
}

#[tokio::test]
async fn probe_page_refuses_when_robots_txt_gives_no_verdicts() {
    let site = SiteBuilder::new()
        .robots(503, "down")
        .every_path(ok_page("Home"))
        .start()
        .await;
    let err = probe_page(
        &site.url("/"),
        None,
        AddressPolicy::AllowPrivate,
        PROBE_FROM,
        Duration::from_millis(20),
    )
    .await
    .unwrap_err();
    assert!(err.contains("503"), "{err}");
    assert_eq!(site.hits_from(OAI), 0);
}

#[tokio::test]
async fn probe_page_refuses_a_page_robots_txt_keeps_codoseobot_from() {
    let site = SiteBuilder::new()
        .robots(200, "User-agent: CodoSEObot\nDisallow: /private\n")
        .every_path(ok_page("Private"))
        .start()
        .await;
    let err = probe_page(
        &site.url("/private/report"),
        None,
        AddressPolicy::AllowPrivate,
        PROBE_FROM,
        Duration::from_millis(20),
    )
    .await
    .unwrap_err();
    assert!(err.contains("CodoSEObot"), "{err}");
    assert!(
        site.requests().iter().all(|r| r.path == "/robots.txt"),
        "{:?}",
        site.requests()
    );
}
