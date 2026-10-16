//! A recheck of one incident: what to fetch again ([`plan`]) and the stored report with what was
//! fetched laid over it ([`overlay`]).
//!
//! A recheck never crawls. It re-reads only what the incident is about (robots.txt for the bot
//! findings, the named pages for the page findings, the named bots on the named pages for a
//! fetch block, `/llms.txt`, the home page as Markdown) and lays that over the newest stored
//! report, so the findings drawn from the result are the crawl's everywhere else. It speaks only for the kinds its
//! observations were conclusive for: a probe that got no verdict, a page that did not load or a
//! robots.txt that could not be read judge nothing, and the incident stays as it is.

use std::collections::BTreeSet;

use codoseo_core::crawl::RobotsFile;
use codoseo_core::output::{LlmsTxtFetch, MarkdownProbe};
use codoseo_core::page::PageRecord;

use crate::content::{ContentReport, raw_content};
use crate::eligibility::{Cause, Effect, engines, record_effect};
use crate::findings::{Evidence, FindingKind, evaluated_kinds};
use crate::llms;
use crate::markdown::{self, MarkdownState};
use crate::probe::ProbeReport;
use crate::registry::registry;
use crate::report::{
    AccessReport, EngineGroup, RobotsBody, RobotsInfo, agent_rules, bot_access, path_and_query,
    slug_from_str,
};
use crate::robots::{RobotsAvailability, RobotsTxt, availability};

/// The most pages one recheck fetches again. A finding about more pages is rechecked on its
/// first ones; the rest keep what the crawl saw, so such a recheck can find the incident still
/// there but never clears pages it did not fetch.
pub const MAX_RECHECK_PAGES: usize = 20;

/// The bots to probe again and the pages to probe them on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeScope {
    pub urls: Vec<String>,
    /// Registry tokens, in the incident's order.
    pub tokens: Vec<String>,
}

/// What one recheck fetches again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecheckPlan {
    /// Fetch `/robots.txt`.
    pub robots: bool,
    /// Fetch these pages (important URLs, in the report's order) as a crawl would.
    pub pages: Vec<String>,
    /// Probe these bots on these pages.
    pub probe: Option<ProbeScope>,
    /// Fetch `/llms.txt`.
    pub llms: bool,
    /// Ask for the home page as Markdown.
    pub markdown: bool,
    /// The kinds the recheck may judge: the incident's own.
    pub kinds: Vec<FindingKind>,
}

/// The plan for the incident of `kind` and `subject` whose evidence is `ev`, over the newest
/// stored report: only the incident's bots and pages, at most [`MAX_RECHECK_PAGES`] pages and at
/// most `probe_urls_cap` probed pages (the plan's per-crawl probe allowance; 0 plans no probe).
pub fn plan(
    kind: FindingKind,
    subject: &str,
    ev: &Evidence,
    stored: &AccessReport,
    probe_urls_cap: u8,
) -> RecheckPlan {
    let mut plan = RecheckPlan {
        robots: false,
        pages: Vec::new(),
        probe: None,
        llms: false,
        markdown: false,
        kinds: vec![kind],
    };
    let position = |u: &str| {
        stored
            .important
            .iter()
            .position(|i| i.url == u)
            .unwrap_or(usize::MAX)
    };
    match kind {
        FindingKind::RobotsUnavailable
        | FindingKind::BotsBlocked
        | FindingKind::BotsNotBlocked
        | FindingKind::ContentSignalsMissing => {
            plan.robots = true;
        }
        FindingKind::MarkdownUnavailable => plan.markdown = true,
        FindingKind::AnswersRestricted => {
            // The pages the stored report shows the directive on, for the incident's engines;
            // the evidence's sample when the report no longer has them.
            let directive = slug_from_str(subject);
            let mut idx: BTreeSet<u16> = BTreeSet::new();
            for e in &ev.engines {
                let Some(access) = stored.engine(e.engine) else {
                    continue;
                };
                for g in access.groups.iter().filter(|g| !g.robots_blocked) {
                    if g.causes
                        .iter()
                        .any(|c| directive.is_none_or(|d| c.directive == d))
                    {
                        idx.extend(&g.urls);
                    }
                }
            }
            plan.pages = if idx.is_empty() {
                ev.urls.sample.clone()
            } else {
                idx.iter()
                    .filter_map(|i| stored.important.get(usize::from(*i)))
                    .map(|u| u.url.clone())
                    .collect()
            };
            plan.pages.truncate(MAX_RECHECK_PAGES);
        }
        FindingKind::ContentMissing => {
            plan.pages = ev.content.iter().map(|c| c.url.clone()).collect();
            plan.pages.sort_by_key(|u| position(u));
            plan.pages.dedup();
            plan.pages.truncate(MAX_RECHECK_PAGES);
        }
        FindingKind::FetchBlocked => {
            let mut urls: Vec<String> = Vec::new();
            for u in ev.probes.iter().flat_map(|p| &p.urls) {
                if !urls.contains(u) {
                    urls.push(u.clone());
                }
            }
            urls.sort_by_key(|u| position(u));
            urls.truncate(usize::from(probe_urls_cap));
            let tokens: Vec<String> = ev.probes.iter().map(|p| p.token.clone()).collect();
            if !urls.is_empty() && !tokens.is_empty() {
                plan.probe = Some(ProbeScope { urls, tokens });
            }
        }
        FindingKind::LlmsTxtUnavailable => plan.llms = true,
    }
    plan
}

/// What a recheck saw. `None` (or a missing page) is something it did not get: not fetched, or
/// no answer at all.
#[derive(Debug, Clone, Default)]
pub struct Observations {
    /// `/robots.txt` as fetched (`None` when no response arrived).
    pub robots: Option<RobotsFile>,
    /// The pages fetched again, as the crawler records them.
    pub pages: Vec<PageRecord>,
    /// The probes, their URL indexes into the stored report's `important`.
    pub probes: Option<ProbeReport>,
    /// `/llms.txt` as fetched (an error inside is an answer: the file is unavailable).
    pub llms: Option<LlmsTxtFetch>,
    /// The home page asked for as Markdown.
    pub markdown: Option<MarkdownProbe>,
}

/// The stored report with what the recheck saw laid over it, and the kinds of `plan` the
/// observations were conclusive for (the only kinds the result may judge):
///
/// - robots.txt: its availability, every bot's verdicts on the stored important pages and which
///   pages each engine's crawler may fetch. Conclusive when it was fetched and the result can
///   judge the kind (a failing file judges `RobotsUnavailable`, not the bots).
/// - pages: each planned page that loaded as HTML replaces its engine view and its content
///   verdict; the site-level counts and the important pages are the crawl's. Conclusive when
///   every planned page loaded.
/// - probes: the re-probed bots' results replace theirs on the re-probed pages, other bots' stay.
///   Conclusive when every planned bot got a verdict on every page it was sent to.
/// - llms.txt: replaced. Conclusive when fetched.
/// - Markdown: replaced. Conclusive when the home page answered.
/// - robots.txt also brings its Content-Signal lines, for the Content Signals check.
pub fn overlay(
    stored: &AccessReport,
    obs: &Observations,
    plan: &RecheckPlan,
) -> (AccessReport, Vec<FindingKind>) {
    let mut now = stored.clone();

    // robots.txt
    let mut robots_seen = false;
    let mut new_robots: Option<Option<RobotsTxt>> = None;
    if plan.robots
        && let Some(file) = &obs.robots
    {
        let avail = availability(Some(file.status));
        let parsed = match avail {
            RobotsAvailability::Ok => Some(RobotsTxt::parse(file.body.as_bytes())),
            RobotsAvailability::Missing => Some(RobotsTxt::parse(b"")),
            _ => None,
        };
        let mut body = (avail == RobotsAvailability::Ok).then(|| RobotsBody::new(&file.body));
        if let (Some(b), Some(was)) = (body.as_mut(), stored.robots.file.as_ref())
            && b.sha256 == was.sha256
        {
            b.since = was.since;
        }
        now.robots = RobotsInfo {
            status: Some(file.status),
            availability: avail,
            file: body,
        };
        // The Content-Signal lines come with the file; a failing one keeps what the crawl read.
        if let Some(robots) = &parsed {
            now.declared.content_signals = robots.content_signals().to_vec();
        }
        let paths: Vec<String> = now
            .important
            .iter()
            .map(|i| path_and_query(&i.url))
            .collect();
        now.rules = Vec::new();
        now.bots = match &parsed {
            Some(robots) => registry()
                .bots
                .iter()
                .map(|bot| {
                    let agent = agent_rules(robots, &bot.token);
                    bot_access(robots, &agent, &bot.token, &paths, &mut now.rules)
                })
                .collect(),
            None => Vec::new(),
        };
        robots_seen = true;
        new_robots = Some(parsed);
    }

    // pages
    let mut refetched: Vec<(u16, &PageRecord)> = Vec::new();
    let mut pages_ok = !plan.pages.is_empty();
    for url in &plan.pages {
        let page = obs
            .pages
            .iter()
            .find(|p| p.url.as_str() == url)
            .filter(|p| p.is_html_ok());
        let at = now
            .important
            .iter()
            .position(|i| &i.url == url)
            .and_then(|i| u16::try_from(i).ok());
        match (page, at) {
            (Some(page), Some(i)) => refetched.push((i, page)),
            _ => pages_ok = false,
        }
    }
    if !refetched.is_empty() {
        let content = now.content.get_or_insert_with(ContentReport::default);
        for (i, page) in &refetched {
            let Some(raw) = raw_content(*i, page) else {
                continue;
            };
            match content.pages.iter_mut().find(|p| p.url == *i) {
                Some(p) => *p = raw,
                None => content.pages.push(raw),
            }
        }
        content.pages.sort_by_key(|p| p.url);
    }
    if new_robots.is_some() || !refetched.is_empty() {
        regroup(&mut now, new_robots.as_ref(), &refetched);
    }

    // probes
    let mut probes_ok = false;
    if let (Some(scope), Some(fresh)) = (&plan.probe, &obs.probes) {
        let judged = fresh.judged_bots();
        probes_ok = fresh.conclusive()
            && scope
                .tokens
                .iter()
                .all(|t| judged.iter().any(|j| j.eq_ignore_ascii_case(t)));
        now.probes = Some(match now.probes.take() {
            None => fresh.clone(),
            Some(mut merged) => {
                for fu in &fresh.urls {
                    match merged.urls.iter_mut().find(|u| u.url == fu.url) {
                        Some(mu) => {
                            mu.baseline = fu.baseline.clone();
                            for r in &fu.results {
                                match mu
                                    .results
                                    .iter_mut()
                                    .find(|x| x.token.eq_ignore_ascii_case(&r.token))
                                {
                                    Some(x) => *x = r.clone(),
                                    None => mu.results.push(r.clone()),
                                }
                            }
                        }
                        None => merged.urls.push(fu.clone()),
                    }
                }
                for b in &fresh.bots {
                    if !merged.bots.iter().any(|m| m.eq_ignore_ascii_case(b)) {
                        merged.bots.push(b.clone());
                    }
                }
                merged.stop = fresh.stop;
                merged
            }
        });
    }

    // llms.txt
    let mut llms_seen = false;
    if plan.llms
        && let Some(fetch) = &obs.llms
    {
        // The incident exists because the site had adopted the file.
        let adopted = stored.llms_txt.as_ref().is_none_or(|r| r.adopted);
        now.llms_txt = Some(llms::assess(fetch, adopted));
        now.llms_txt_skipped = None;
        llms_seen = true;
    }

    // Markdown
    let mut markdown_seen = false;
    if plan.markdown
        && let Some(probe) = &obs.markdown
    {
        let report = markdown::assess(probe);
        markdown_seen = report.state != MarkdownState::Unreachable;
        now.markdown = Some(report);
    }

    let evaluated = evaluated_kinds(&now);
    let kinds = plan
        .kinds
        .iter()
        .copied()
        .filter(|k| {
            let seen = match k {
                FindingKind::RobotsUnavailable
                | FindingKind::BotsBlocked
                | FindingKind::BotsNotBlocked
                | FindingKind::ContentSignalsMissing => robots_seen,
                FindingKind::MarkdownUnavailable => markdown_seen,
                FindingKind::AnswersRestricted | FindingKind::ContentMissing => pages_ok,
                FindingKind::FetchBlocked => probes_ok,
                FindingKind::LlmsTxtUnavailable => llms_seen,
            };
            seen && evaluated.contains(k)
        })
        .collect();
    (now, kinds)
}

/// Rebuilds each engine's groups after a recheck: the refetched pages' own effect, and, when
/// robots.txt was read again (`Some`), whether the engine's crawler may fetch each page (`None`
/// inside: no verdicts, so none is blocked, as a crawl would say). Pages counted for the engine
/// stay the crawl's (`html_ok`), as do the site-level counts.
fn regroup(
    report: &mut AccessReport,
    robots: Option<&Option<RobotsTxt>>,
    refetched: &[(u16, &PageRecord)],
) {
    let paths: Vec<String> = report
        .important
        .iter()
        .map(|i| path_and_query(&i.url))
        .collect();
    let counted: Vec<bool> = report.important.iter().map(|i| i.html_ok).collect();
    for access in &mut report.engines {
        let Some(engine) = engines().iter().find(|e| e.id == access.engine) else {
            continue;
        };
        let crawler = match robots {
            Some(Some(r)) => Some((r, agent_rules(r, engine.crawler))),
            _ => None,
        };
        let mut state: Vec<Option<(Effect, Vec<Cause>, bool)>> = counted
            .iter()
            .map(|c| c.then(|| (Effect::Eligible, Vec::new(), false)))
            .collect();
        for g in &access.groups {
            for u in &g.urls {
                if let Some(Some(s)) = state.get_mut(usize::from(*u)) {
                    *s = (g.effect, g.causes.clone(), g.robots_blocked);
                }
            }
        }
        for (i, page) in refetched {
            if let (Some(Some(s)), Some((effect, causes))) =
                (state.get_mut(usize::from(*i)), record_effect(engine, page))
            {
                s.0 = effect;
                s.1 = causes;
            }
        }
        if let Some(robots) = robots {
            for (i, s) in state.iter_mut().enumerate() {
                if let Some(s) = s {
                    s.2 = match (robots, &crawler) {
                        (Some(_), Some((r, agent))) => !r.allowed_for(agent, &paths[i]),
                        _ => false,
                    };
                }
            }
        }
        let mut groups: Vec<EngineGroup> = Vec::new();
        for (i, s) in state.into_iter().enumerate() {
            let Some((effect, causes, robots_blocked)) = s else {
                continue;
            };
            if !robots_blocked && effect == Effect::Eligible {
                continue;
            }
            let i = i as u16;
            match groups.iter_mut().find(|g| {
                g.effect == effect && g.robots_blocked == robots_blocked && g.causes == causes
            }) {
                Some(g) => g.urls.push(i),
                None => groups.push(EngineGroup {
                    effect,
                    causes,
                    robots_blocked,
                    urls: vec![i],
                }),
            }
        }
        access.groups = groups;
    }
}
