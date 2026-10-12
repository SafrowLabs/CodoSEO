//! B3.7: what a recheck of one incident fetches, and the stored report with what it saw laid
//! over it.

mod common;

use std::collections::HashSet;

use codoseo_core::crawl::RobotsFile;
use codoseo_core::output::{CrawlOutput, LlmsTxtFetch};
use codoseo_core::page::PageRecord;
use codoseo_geo::findings::{Finding, FindingKind, findings};
use codoseo_geo::intent::Intent;
use codoseo_geo::llms;
use codoseo_geo::probe::{
    InconclusiveReason, ProbeFetch, ProbeReport, ProbeResult, ProbedUrl, Vendor, Verdict,
};
use codoseo_geo::recheck::{MAX_RECHECK_PAGES, Observations, RecheckPlan, overlay, plan};
use codoseo_geo::report::{AccessReport, build_report, important_urls};
use common::*;

const BLOCK_OAI: &str = "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /\n";
const ALLOW_ALL: &str = "User-agent: *\nAllow: /\n";

fn report_for(out: &CrawlOutput) -> AccessReport {
    let important = important_urls(&out.pages, &out.origin, &HashSet::new());
    build_report(out, &important)
}

fn finding_of(report: &AccessReport, kind: FindingKind) -> Option<Finding> {
    findings(report, &Intent::default())
        .into_iter()
        .find(|f| f.kind == kind)
}

fn robots(status: u16, body: &str) -> RobotsFile {
    RobotsFile {
        status,
        body: body.to_owned(),
        hash: 1,
    }
}

fn shell(mut p: PageRecord) -> PageRecord {
    p.fields.word_count = 0;
    p.fields.raw.mount_empty = true;
    p
}

fn fetch(status: u16, title: &str, bytes: u32) -> ProbeFetch {
    ProbeFetch {
        status: Some(status),
        html: true,
        bytes,
        title: Some(title.to_owned()),
        ..ProbeFetch::default()
    }
}

/// A probe report of the home page with these verdicts.
fn probes(verdicts: &[(&str, Verdict)]) -> ProbeReport {
    ProbeReport {
        version: 1,
        synthetic: true,
        from: "hello@codoseo.com".to_owned(),
        bots: verdicts.iter().map(|(t, _)| (*t).to_owned()).collect(),
        skipped_bots: Vec::new(),
        urls: vec![ProbedUrl {
            url: 0,
            baseline: fetch(200, "Home", 20_000),
            results: verdicts
                .iter()
                .map(|(t, v)| ProbeResult {
                    token: (*t).to_owned(),
                    fetch: match v {
                        Verdict::Inconclusive { .. } => None,
                        Verdict::Same => Some(fetch(200, "Home", 20_000)),
                        _ => Some(fetch(403, "Just a moment...", 4_000)),
                    },
                    verdict: v.clone(),
                })
                .collect(),
        }],
        stop: None,
        requests: 0,
    }
}

fn cloudflare() -> Verdict {
    Verdict::Challenged {
        vendor: Vendor::Cloudflare,
    }
}

// ---- plan ------------------------------------------------------------------------------------

#[test]
fn a_robots_finding_rechecks_robots_txt_and_nothing_else() {
    let stored = report_for(&with_robots(site(5, |p| p), 200, BLOCK_OAI));
    let f = finding_of(&stored, FindingKind::BotsBlocked).unwrap();
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);
    assert_eq!(
        p,
        RecheckPlan {
            robots: true,
            pages: Vec::new(),
            probe: None,
            llms: false,
            kinds: vec![FindingKind::BotsBlocked],
        }
    );
    let stored = report_for(&with_robots(site(5, |p| p), 503, ""));
    let f = finding_of(&stored, FindingKind::RobotsUnavailable).unwrap();
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);
    assert!(p.robots && p.pages.is_empty() && p.probe.is_none() && !p.llms);
    assert_eq!(p.kinds, vec![FindingKind::RobotsUnavailable]);
}

#[test]
fn a_fetch_blocked_recheck_probes_only_its_bots_and_pages_up_to_the_cap() {
    let mut stored = report_for(&site(5, |p| p));
    let mut report = probes(&[
        ("OAI-SearchBot", cloudflare()),
        ("Claude-SearchBot", cloudflare()),
        ("PerplexityBot", Verdict::Same),
    ]);
    let mut second = report.urls[0].clone();
    second.url = 1;
    report.urls.push(second);
    stored.probes = Some(report);
    let f = finding_of(&stored, FindingKind::FetchBlocked).unwrap();
    assert_eq!(f.evidence.urls.total, 2);

    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 1);
    let scope = p.probe.expect("probes");
    assert_eq!(scope.urls, vec![stored.important[0].url.clone()]);
    assert_eq!(scope.tokens, vec!["OAI-SearchBot", "Claude-SearchBot"]);
    assert!(!p.robots && p.pages.is_empty() && !p.llms);
    assert_eq!(p.kinds, vec![FindingKind::FetchBlocked]);

    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 5);
    assert_eq!(p.probe.unwrap().urls.len(), 2);
}

#[test]
fn a_page_finding_refetches_only_its_pages_up_to_the_cap() {
    // Content: the shell pages the evidence names.
    let out = crawl(vec![
        shell(page("/", 9)),
        page("/fine", 8),
        shell(page("/app", 7)),
    ]);
    let stored = report_for(&out);
    let f = finding_of(&stored, FindingKind::ContentMissing).unwrap();
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);
    assert_eq!(p.pages, vec![url("/").to_string(), url("/app").to_string()]);
    assert!(!p.robots && p.probe.is_none() && !p.llms);
    assert_eq!(p.kinds, vec![FindingKind::ContentMissing]);

    // Answers: the pages that carry the directive, capped.
    let out = site(40, |p| p.robots_meta("nosnippet"));
    let stored = report_for(&out);
    let f = finding_of(&stored, FindingKind::AnswersRestricted).unwrap();
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);
    assert_eq!(p.pages.len(), MAX_RECHECK_PAGES);
    assert_eq!(p.pages[0], stored.important[0].url);
    assert_eq!(p.kinds, vec![FindingKind::AnswersRestricted]);
}

#[test]
fn an_llms_txt_recheck_fetches_only_llms_txt() {
    let mut out = site(3, |p| p);
    out.signals.llms_txt = Some(LlmsTxtFetch {
        status: 503,
        ..LlmsTxtFetch::default()
    });
    let mut stored = report_for(&out);
    stored.llms_txt.as_mut().unwrap().adopted = true;
    let f = finding_of(&stored, FindingKind::LlmsTxtUnavailable).unwrap();
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);
    assert!(p.llms && !p.robots && p.pages.is_empty() && p.probe.is_none());
    assert_eq!(p.kinds, vec![FindingKind::LlmsTxtUnavailable]);
}

// ---- overlay ---------------------------------------------------------------------------------

#[test]
fn a_restored_robots_txt_clears_the_block() {
    let stored = report_for(&with_robots(site(5, |p| p), 200, BLOCK_OAI));
    let f = finding_of(&stored, FindingKind::BotsBlocked).unwrap();
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);

    let obs = Observations {
        robots: Some(robots(200, ALLOW_ALL)),
        ..Observations::default()
    };
    let (now, kinds) = overlay(&stored, &obs, &p);
    assert_eq!(kinds, vec![FindingKind::BotsBlocked]);
    assert!(finding_of(&now, FindingKind::BotsBlocked).is_none());
    // Only robots.txt was re-read: the pages are the stored ones.
    assert_eq!(now.important, stored.important);
    assert_eq!(now.content, stored.content);

    // Still blocked: still found, and judged.
    let obs = Observations {
        robots: Some(robots(200, BLOCK_OAI)),
        ..Observations::default()
    };
    let (now, kinds) = overlay(&stored, &obs, &p);
    assert_eq!(kinds, vec![FindingKind::BotsBlocked]);
    assert_eq!(
        finding_of(&now, FindingKind::BotsBlocked).unwrap().subject,
        "search"
    );
    assert_eq!(now.bots, stored.bots);
    assert_eq!(now.rules, stored.rules);
    assert_eq!(
        now.engines, stored.engines,
        "the same file regroups the same"
    );
}

#[test]
fn robots_txt_that_could_not_be_read_judges_nothing() {
    let stored = report_for(&with_robots(site(5, |p| p), 200, BLOCK_OAI));
    let f = finding_of(&stored, FindingKind::BotsBlocked).unwrap();
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);
    // Not fetched at all.
    let (_, kinds) = overlay(&stored, &Observations::default(), &p);
    assert!(kinds.is_empty());
    // Now failing: that is robots_unavailable's business, not a verdict on the bots.
    let obs = Observations {
        robots: Some(robots(503, "")),
        ..Observations::default()
    };
    let (_, kinds) = overlay(&stored, &obs, &p);
    assert!(kinds.is_empty());
}

#[test]
fn a_robots_txt_that_answers_again_resolves_robots_unavailable() {
    let stored = report_for(&with_robots(site(5, |p| p), 503, ""));
    let f = finding_of(&stored, FindingKind::RobotsUnavailable).unwrap();
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);
    let obs = Observations {
        robots: Some(robots(200, ALLOW_ALL)),
        ..Observations::default()
    };
    let (now, kinds) = overlay(&stored, &obs, &p);
    assert_eq!(kinds, vec![FindingKind::RobotsUnavailable]);
    assert!(finding_of(&now, FindingKind::RobotsUnavailable).is_none());
}

#[test]
fn inconclusive_probes_judge_nothing_and_clean_ones_resolve() {
    let mut stored = report_for(&site(5, |p| p));
    stored.probes = Some(probes(&[("OAI-SearchBot", cloudflare())]));
    let f = finding_of(&stored, FindingKind::FetchBlocked).unwrap();
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);

    let obs = Observations {
        probes: Some(probes(&[(
            "OAI-SearchBot",
            Verdict::Inconclusive {
                reason: InconclusiveReason::ProbeError,
            },
        )])),
        ..Observations::default()
    };
    let (_, kinds) = overlay(&stored, &obs, &p);
    assert!(!kinds.contains(&FindingKind::FetchBlocked));
    let (_, kinds) = overlay(&stored, &Observations::default(), &p);
    assert!(kinds.is_empty());

    let obs = Observations {
        probes: Some(probes(&[("OAI-SearchBot", Verdict::Same)])),
        ..Observations::default()
    };
    let (now, kinds) = overlay(&stored, &obs, &p);
    assert_eq!(kinds, vec![FindingKind::FetchBlocked]);
    assert!(finding_of(&now, FindingKind::FetchBlocked).is_none());

    let obs = Observations {
        probes: Some(probes(&[("OAI-SearchBot", cloudflare())])),
        ..Observations::default()
    };
    let (now, kinds) = overlay(&stored, &obs, &p);
    assert_eq!(kinds, vec![FindingKind::FetchBlocked]);
    assert!(finding_of(&now, FindingKind::FetchBlocked).is_some());
}

#[test]
fn reprobing_one_bot_keeps_the_others_results() {
    let mut stored = report_for(&site(5, |p| p));
    stored.probes = Some(probes(&[
        ("OAI-SearchBot", cloudflare()),
        ("PerplexityBot", Verdict::Same),
    ]));
    let f = finding_of(&stored, FindingKind::FetchBlocked).unwrap();
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);
    let obs = Observations {
        probes: Some(probes(&[("OAI-SearchBot", Verdict::Same)])),
        ..Observations::default()
    };
    let (now, _) = overlay(&stored, &obs, &p);
    let results = &now.probes.unwrap().urls[0].results;
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.verdict == Verdict::Same));
}

#[test]
fn a_refetched_page_that_is_still_a_shell_is_still_found() {
    let out = crawl(vec![
        shell(page("/", 9)),
        page("/fine", 8),
        shell(page("/app", 7)),
    ]);
    let stored = report_for(&out);
    let f = finding_of(&stored, FindingKind::ContentMissing).unwrap();
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);

    // Both still shells.
    let obs = Observations {
        pages: vec![shell(page("/", 9)), shell(page("/app", 7))],
        ..Observations::default()
    };
    let (now, kinds) = overlay(&stored, &obs, &p);
    assert_eq!(kinds, vec![FindingKind::ContentMissing]);
    assert!(finding_of(&now, FindingKind::ContentMissing).is_some());

    // One fixed, one still a shell: still found, about the one.
    let obs = Observations {
        pages: vec![page("/", 9), shell(page("/app", 7))],
        ..Observations::default()
    };
    let (now, kinds) = overlay(&stored, &obs, &p);
    assert_eq!(kinds, vec![FindingKind::ContentMissing]);
    let still = finding_of(&now, FindingKind::ContentMissing).unwrap();
    assert_eq!(still.evidence.content.len(), 1);
    assert_eq!(still.evidence.content[0].url, url("/app").to_string());

    // Both fixed: gone.
    let obs = Observations {
        pages: vec![page("/", 9), page("/app", 7)],
        ..Observations::default()
    };
    let (now, kinds) = overlay(&stored, &obs, &p);
    assert_eq!(kinds, vec![FindingKind::ContentMissing]);
    assert!(finding_of(&now, FindingKind::ContentMissing).is_none());

    // A page that failed to load (or was left out) judges nothing.
    let obs = Observations {
        pages: vec![page("/", 9), page("/app", 7).status(503)],
        ..Observations::default()
    };
    let (_, kinds) = overlay(&stored, &obs, &p);
    assert!(kinds.is_empty());
    let obs = Observations {
        pages: vec![page("/", 9)],
        ..Observations::default()
    };
    let (_, kinds) = overlay(&stored, &obs, &p);
    assert!(kinds.is_empty());
}

#[test]
fn refetched_pages_replace_their_engine_view_and_keep_the_site_counts() {
    let out = site(5, |p| p.robots_meta("nosnippet"));
    let stored = report_for(&out);
    let f = finding_of(&stored, FindingKind::AnswersRestricted).unwrap();
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);
    assert_eq!(p.pages.len(), 5);

    // The same pages again: the same report.
    let obs = Observations {
        pages: out.pages.clone(),
        ..Observations::default()
    };
    let (now, _) = overlay(&stored, &obs, &p);
    assert_eq!(now, stored);

    // Every page fixed.
    let fixed = site(5, |p| p);
    let obs = Observations {
        pages: fixed.pages.clone(),
        ..Observations::default()
    };
    let (now, kinds) = overlay(&stored, &obs, &p);
    assert_eq!(kinds, vec![FindingKind::AnswersRestricted]);
    assert!(finding_of(&now, FindingKind::AnswersRestricted).is_none());
    assert_eq!(now.html_important(), stored.html_important());
    for (a, b) in now.engines.iter().zip(&stored.engines) {
        assert_eq!(a.site, b.site, "site counts come from the crawl");
        assert_eq!(a.html_pages, b.html_pages);
    }

    // Only the home page fixed: the others are as stored.
    let obs = Observations {
        pages: vec![fixed.pages[0].clone()]
            .into_iter()
            .chain(out.pages[1..].iter().cloned())
            .collect(),
        ..Observations::default()
    };
    let (now, kinds) = overlay(&stored, &obs, &p);
    assert_eq!(kinds, vec![FindingKind::AnswersRestricted]);
    let still = finding_of(&now, FindingKind::AnswersRestricted).unwrap();
    assert_eq!(still.evidence.urls.total, 4);
}

#[test]
fn an_llms_txt_served_again_resolves_and_a_failure_keeps_it() {
    let mut out = site(3, |p| p);
    out.signals.llms_txt = Some(LlmsTxtFetch {
        status: 503,
        ..LlmsTxtFetch::default()
    });
    let mut stored = report_for(&out);
    stored.llms_txt.as_mut().unwrap().adopted = true;
    let f = finding_of(&stored, FindingKind::LlmsTxtUnavailable).unwrap();
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);

    let obs = Observations {
        llms: Some(LlmsTxtFetch {
            status: 200,
            content_type: Some("text/plain".to_owned()),
            body: "# Example\n\n> A site.\n".to_owned(),
            ..LlmsTxtFetch::default()
        }),
        ..Observations::default()
    };
    let (now, kinds) = overlay(&stored, &obs, &p);
    assert_eq!(kinds, vec![FindingKind::LlmsTxtUnavailable]);
    assert!(finding_of(&now, FindingKind::LlmsTxtUnavailable).is_none());
    assert_eq!(
        now.llms_txt.as_ref().unwrap().state,
        llms::LlmsState::Available
    );

    let obs = Observations {
        llms: Some(LlmsTxtFetch {
            status: 502,
            ..LlmsTxtFetch::default()
        }),
        ..Observations::default()
    };
    let (now, kinds) = overlay(&stored, &obs, &p);
    assert_eq!(kinds, vec![FindingKind::LlmsTxtUnavailable]);
    assert!(finding_of(&now, FindingKind::LlmsTxtUnavailable).is_some());
}

#[test]
fn observations_outside_the_plan_judge_nothing_else() {
    // A robots.txt recheck that also happened to see pages judges only its own kind.
    let stored = report_for(&with_robots(site(5, |p| p), 200, BLOCK_OAI));
    let f = finding_of(&stored, FindingKind::BotsBlocked).unwrap();
    let p = plan(f.kind, &f.subject, &f.evidence, &stored, 3);
    let obs = Observations {
        robots: Some(robots(200, ALLOW_ALL)),
        pages: site(5, |p| p).pages,
        ..Observations::default()
    };
    let (_, kinds) = overlay(&stored, &obs, &p);
    assert_eq!(kinds, vec![FindingKind::BotsBlocked]);
}
