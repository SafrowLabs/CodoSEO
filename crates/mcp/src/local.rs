//! `LocalBackend`: runs crawls directly against the crawler/checks/diff crates, with
//! no database. Running audits live in memory; finished ones are read back from the
//! `AuditCache`, so a backend restart still finds them (not expected to matter for
//! `codoseo mcp`'s process lifetime, but cheap to support).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use codoseo_checks::{def, run_checks};
use codoseo_core::audit::{AUDIT_FORMAT_VERSION, Audit};
use codoseo_core::change::Change;
use codoseo_core::check::CheckId;
use codoseo_core::crawl::{AddressPolicy, CrawlConfig, CrawlLimits, Politeness, USER_AGENT};
use codoseo_core::output::StopReason;
use codoseo_core::page::PageRecord;
use codoseo_core::snapshot::Snapshot;
use codoseo_crawler::crawl::{crawl, inspect_page};
use codoseo_crawler::fetch::{FetchError, Fetcher, FetcherConfig, Hop};
use codoseo_crawler::robots::fetch_robots;
use codoseo_diff::{diff, key_pages};
use codoseo_geo::eligibility::{Effect, engines, record_effect};
use codoseo_geo::report::{BotVerdict, RobotsDeclared, bot_verdicts};
use codoseo_geo::robots::{RobotsTxt, availability};
use url::Url;

use crate::backend::{Backend, BackendError};
use crate::cache::{AuditCache, CacheError};
use crate::types::{
    AiAccessReport, AiEngine, AiRobots, AuditHandle, AuditId, AuditState, AuditStatus,
    AuditSummary, FailingCheck, MAX_FAILING_CHECKS, RedirectReport, RobotsReport, UrlRow,
    rank_failing,
};

pub struct LocalBackend {
    cache: AuditCache,
    running: Arc<Mutex<HashMap<AuditId, Arc<Mutex<AuditState>>>>>,
}

impl LocalBackend {
    pub fn new(cache: AuditCache) -> LocalBackend {
        LocalBackend {
            cache,
            running: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Distinguishes "no such audit" from a real I/O or corruption problem, so a
    /// corrupted cache file doesn't get reported as a typo'd id.
    fn load_finished(&self, id: &AuditId) -> Result<Audit, BackendError> {
        self.cache.load(id).map_err(|e| match e {
            CacheError::NotFound(id) => BackendError::AuditNotFound(id),
            other => BackendError::Other(format!("could not load the cached audit: {other}")),
        })
    }
}

impl Backend for LocalBackend {
    async fn audit(&self, url: Url, max_pages: u32) -> Result<AuditHandle, BackendError> {
        let id = AuditId::new();
        let state = Arc::new(Mutex::new(AuditState {
            id: id.clone(),
            status: AuditStatus::Running,
            progress: None,
            summary: None,
        }));
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), Arc::clone(&state));

        let cache = self.cache.clone();
        let running = Arc::clone(&self.running);
        let task_id = id.clone();
        let cfg = CrawlConfig {
            start_url: url,
            limits: CrawlLimits {
                max_pages,
                ..CrawlLimits::default()
            },
            politeness: Politeness::default(),
            address_policy: AddressPolicy::AllowPrivate,
            user_agent: USER_AGENT.to_owned(),
            site_signals: true,
        };
        let progress_state = Arc::clone(&state);
        tokio::spawn(async move {
            let result = crawl(cfg.clone(), move |p| {
                progress_state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .progress = Some(p);
            })
            .await;
            // Resolved fully before taking the lock, so a concurrent `get_audit` never
            // observes `summary` set while `status` is still `Running` (or vice versa).
            let (status, summary) = match result {
                Ok(mut out) => {
                    let report = run_checks(&mut out);
                    // The same section `codoseo crawl` saves, under the default intent.
                    let ai_access = codoseo_geo::assess_with_defaults(&out);
                    let audit = Audit {
                        format_version: AUDIT_FORMAT_VERSION,
                        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
                        created_at: now_secs(),
                        duration_ms: out.duration_ms,
                        start_url: cfg.start_url.clone(),
                        report,
                        snapshot: Snapshot::from_output_owned(out),
                        ai_access: Some(ai_access),
                    };
                    match cache.save(&task_id, &audit) {
                        Ok(()) => (AuditStatus::Done, Some(build_summary(&task_id, &audit))),
                        Err(e) => (
                            AuditStatus::Failed(format!("could not cache audit: {e}")),
                            None,
                        ),
                    }
                }
                Err(e) => (AuditStatus::Failed(e.to_string()), None),
            };
            let is_done = matches!(status, AuditStatus::Done);
            {
                let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                guard.status = status;
                guard.summary = summary;
            }
            // A `Done` audit is now safely served from the cache (which correctly
            // reports no progress), so drop it here - otherwise a long-lived `codoseo
            // mcp` process accumulates one entry per audit forever. `Failed` audits
            // were never written to the cache, so they stay in memory; it's the only
            // record of why they failed.
            if is_done {
                running
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&task_id);
            }
        });

        Ok(AuditHandle {
            id,
            status: AuditStatus::Running,
        })
    }

    async fn get_audit(&self, id: &AuditId) -> Result<AuditState, BackendError> {
        let running = {
            let map = self.running.lock().unwrap_or_else(|e| e.into_inner());
            map.get(id)
                .map(|s| s.lock().unwrap_or_else(|e| e.into_inner()).clone())
        };
        if let Some(state) = running {
            return Ok(state);
        }
        let audit = self.load_finished(id)?;
        Ok(AuditState {
            id: id.clone(),
            status: AuditStatus::Done,
            progress: None,
            summary: Some(build_summary(id, &audit)),
        })
    }

    async fn issue_urls(
        &self,
        id: &AuditId,
        check: CheckId,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<UrlRow>, BackendError> {
        let audit = self.load_finished(id)?;
        Ok(audit
            .snapshot
            .pages
            .iter()
            .filter(|p| p.issues.has_check(check))
            .skip(offset as usize)
            .take(limit as usize)
            .map(|p| UrlRow {
                url: p.url.clone(),
                status: p.status,
                title: p.fields.title.clone(),
                indexability: p.indexability,
            })
            .collect())
    }

    async fn page(&self, id: &AuditId, url: &Url) -> Result<PageRecord, BackendError> {
        let audit = self.load_finished(id)?;
        audit
            .snapshot
            .pages
            .into_iter()
            .find(|p| &p.url == url)
            .ok_or_else(|| BackendError::PageNotFound(url.to_string()))
    }

    async fn check_page(&self, url: Url) -> Result<PageRecord, BackendError> {
        let cfg = CrawlConfig {
            start_url: url,
            limits: CrawlLimits::default(),
            politeness: Politeness::default(),
            address_policy: AddressPolicy::AllowPrivate,
            user_agent: USER_AGENT.to_owned(),
            site_signals: true,
        };
        let mut page = inspect_page(&cfg)
            .await
            .map_err(|e| BackendError::Fetch(e.to_string()))?;
        codoseo_checks::check_page(&mut page);
        Ok(page)
    }

    async fn check_robots(
        &self,
        url: Url,
        path: Option<String>,
    ) -> Result<RobotsReport, BackendError> {
        let fetcher = Fetcher::new(FetcherConfig::new(AddressPolicy::AllowPrivate))
            .map_err(|e| BackendError::Fetch(e.to_string()))?;
        let (rules, file) = fetch_robots(&fetcher, &url)
            .await
            .map_err(|e| BackendError::Fetch(e.to_string()))?;
        let path = path.unwrap_or_else(|| url.path().to_owned());
        Ok(RobotsReport {
            status: file.status,
            allowed: rules.allowed(&path),
            path,
            crawl_delay_secs: rules.crawl_delay().map(|d| d.as_secs_f64()),
            sitemaps: rules.sitemaps().to_vec(),
        })
    }

    async fn check_ai_access(&self, url: Url) -> Result<AiAccessReport, BackendError> {
        let fetcher = Fetcher::new(FetcherConfig::new(AddressPolicy::AllowPrivate))
            .map_err(|e| BackendError::Fetch(e.to_string()))?;
        let (_, file) = fetch_robots(&fetcher, &url)
            .await
            .map_err(|e| BackendError::Fetch(e.to_string()))?;
        // The path the verdicts are for includes the query, as robots.txt patterns see it.
        let path = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_owned(),
        };
        let parsed = RobotsTxt::from_response(Some(file.status), file.body.as_bytes());
        let (bots, declared) = match &parsed {
            Some(txt) => (bot_verdicts(txt, &path), RobotsDeclared::of(txt)),
            None => (Vec::new(), RobotsDeclared::default()),
        };

        // A page that cannot be fetched still gets the robots answer; only the engines are empty.
        let cfg = CrawlConfig {
            start_url: url.clone(),
            limits: CrawlLimits::default(),
            politeness: Politeness::default(),
            address_policy: AddressPolicy::AllowPrivate,
            user_agent: USER_AGENT.to_owned(),
            site_signals: true,
        };
        let page = inspect_page(&cfg).await.ok();
        let engines: Vec<AiEngine> = page
            .iter()
            .flat_map(|p| {
                engines().iter().filter_map(move |e| {
                    record_effect(e, p).map(|(effect, causes)| AiEngine {
                        id: e.id,
                        name: e.name.to_owned(),
                        effect,
                        causes,
                        note: e.note().map(str::to_owned),
                    })
                })
            })
            .collect();

        let summary = ai_access_summary(&path, file.status, parsed.is_some(), &bots, &engines);
        Ok(AiAccessReport {
            url: url.to_string(),
            path,
            summary,
            robots: AiRobots {
                status: file.status,
                availability: availability(Some(file.status)),
            },
            bots,
            declared,
            engines,
            page_status: page.map(|p| p.status),
        })
    }

    async fn check_redirects(&self, url: Url) -> Result<RedirectReport, BackendError> {
        let fetcher = Fetcher::new(FetcherConfig::new(AddressPolicy::AllowPrivate))
            .map_err(|e| BackendError::Fetch(e.to_string()))?;
        match fetcher.fetch(&url).await {
            Ok(res) => Ok(RedirectReport {
                hops: hops(res.chain),
                final_status: Some(res.status),
                final_url: Some(res.final_url),
                problem: None,
            }),
            Err(FetchError::RedirectLoop { chain }) => Ok(settled_not(chain, "redirect_loop")),
            Err(FetchError::TooManyRedirects { chain }) => {
                Ok(settled_not(chain, "too_many_redirects"))
            }
            Err(e) => Err(BackendError::Fetch(e.to_string())),
        }
    }

    async fn compare(&self, a: &AuditId, b: &AuditId) -> Result<Vec<Change>, BackendError> {
        let before = self.load_finished(a)?;
        let after = self.load_finished(b)?;
        let key = key_pages(&before.snapshot, &HashSet::new());
        Ok(diff(&before.snapshot, &after.snapshot, &key))
    }
}

/// A few lines an agent can relay: who is blocked and which engines the page is held back from.
fn ai_access_summary(
    path: &str,
    robots_status: u16,
    has_verdicts: bool,
    bots: &[BotVerdict],
    engines: &[AiEngine],
) -> String {
    if !has_verdicts {
        return format!(
            "robots.txt answered HTTP {robots_status}, so crawlers are told to stay away for now; \
             no per-bot verdicts."
        );
    }
    let blocked: Vec<&str> = bots
        .iter()
        .filter(|b| !b.allowed)
        .map(|b| b.token.as_str())
        .collect();
    let mut text = if blocked.is_empty() {
        format!(
            "robots.txt allows all {} known AI bots on {path}.",
            bots.len()
        )
    } else {
        format!(
            "robots.txt blocks {} of {} known AI bots on {path}: {}.",
            blocked.len(),
            bots.len(),
            blocked.join(", ")
        )
    };
    let held: Vec<&str> = engines
        .iter()
        .filter(|e| e.effect != Effect::Eligible)
        .map(|e| e.name.as_str())
        .collect();
    if !held.is_empty() {
        text.push_str(&format!(
            " The page's own controls limit or exclude it in: {}.",
            held.join(", ")
        ));
    }
    text
}

fn hops(chain: Vec<Hop>) -> Vec<(u16, Url)> {
    chain.into_iter().map(|h| (h.status, h.url)).collect()
}

fn settled_not(chain: Vec<Hop>, problem: &str) -> RedirectReport {
    RedirectReport {
        hops: hops(chain),
        final_status: None,
        final_url: None,
        problem: Some(problem.to_owned()),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A stop reason as a stable machine name, for callers that branch on it.
pub fn stop_reason_code(stop: &StopReason) -> &'static str {
    match stop {
        StopReason::Completed => "completed",
        StopReason::PageLimit => "page_limit",
        StopReason::TimeLimit => "time_limit",
        StopReason::Unreachable(_) => "unreachable",
        StopReason::Blocked(_) => "blocked",
        StopReason::RobotsBlocked => "robots_blocked",
    }
}

/// A stop reason in the words summaries use.
pub fn stop_reason_words(stop: &StopReason) -> String {
    match stop {
        StopReason::Completed => "completed".to_owned(),
        StopReason::PageLimit => "page limit reached".to_owned(),
        StopReason::TimeLimit => "time limit reached".to_owned(),
        StopReason::Unreachable(why) => format!("site unreachable: {why}"),
        StopReason::Blocked(why) => format!("crawler blocked: {why}"),
        StopReason::RobotsBlocked => "blocked by robots.txt".to_owned(),
    }
}

/// Built from a saved [`Audit`], so a freshly finished crawl and one reloaded from the
/// cache (after a restart) produce the same summary shape.
fn build_summary(id: &AuditId, audit: &Audit) -> AuditSummary {
    let ranked = rank_failing(audit.report.counts.iter().copied());
    let total = ranked.len();
    let failing_checks = ranked
        .iter()
        .take(MAX_FAILING_CHECKS)
        .map(|&(check, count)| {
            let d = def(check);
            let example_urls = audit
                .snapshot
                .pages
                .iter()
                .filter(|p| p.issues.has_check(check))
                .take(3)
                .map(|p| p.url.clone())
                .collect();
            FailingCheck {
                check,
                title: d.title.to_owned(),
                severity: d.severity,
                count,
                example_urls,
            }
        })
        .collect();
    AuditSummary {
        audit_id: id.clone(),
        start_url: audit.start_url.clone(),
        health_score: audit.report.health_score,
        checks_passed: audit.report.checks_passed,
        checks_total: audit.report.checks_total,
        pages_crawled: audit.snapshot.pages.len() as u32,
        stop_reason: stop_reason_words(&audit.snapshot.stop),
        failing_checks,
        more_failing_checks: (total.saturating_sub(MAX_FAILING_CHECKS)) as u16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_testkit::SiteBuilder;

    fn backend() -> (LocalBackend, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let backend = LocalBackend::new(AuditCache::new(dir.path().to_owned()));
        (backend, dir)
    }

    /// Polls `get_audit` until the fixture crawl (a handful of localhost pages) finishes.
    async fn audit_fast(backend: &LocalBackend, url: Url) -> AuditId {
        let handle = backend.audit(url, 50).await.unwrap();
        for _ in 0..200 {
            let state = backend.get_audit(&handle.id).await.unwrap();
            match state.status {
                AuditStatus::Done => return handle.id,
                AuditStatus::Failed(why) => panic!("audit failed: {why}"),
                AuditStatus::Running => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await
                }
            }
        }
        panic!("audit did not finish in time");
    }

    #[tokio::test]
    async fn audit_reaches_done_and_matches_the_cached_file() {
        let site = SiteBuilder::new()
            .html("/", "Home", &["/missing-title"])
            .page(
                "/missing-title",
                codoseo_testkit::Page::html("<html><body>no title here</body></html>"),
            )
            .start()
            .await;
        let (backend, _dir) = backend();
        let id = audit_fast(&backend, site.url("/")).await;

        let state = backend.get_audit(&id).await.unwrap();
        assert_eq!(state.status, AuditStatus::Done);
        let summary = state.summary.unwrap();
        assert_eq!(summary.pages_crawled, 2);
        assert!(
            summary
                .failing_checks
                .iter()
                .any(|f| f.check == CheckId::TitleMissing)
        );
    }

    /// `audit_site` saves the AI access section `codoseo crawl` saves: the same report and
    /// findings, under the default intent.
    #[tokio::test]
    async fn a_cached_audit_keeps_the_ai_access_section_the_cli_writes() {
        let site = SiteBuilder::new()
            .robots(
                200,
                "User-agent: *\nAllow: /\n\nUser-agent: OAI-SearchBot\nDisallow: /\n",
            )
            .html("/", "Home", &["/about"])
            .html("/about", "About", &[])
            .start()
            .await;
        let (backend, _dir) = backend();
        let id = audit_fast(&backend, site.url("/")).await;
        let audit = backend.cache.load(&id).unwrap();
        let section = audit.ai_access.expect("an ai_access section");
        assert_eq!(
            section["report"]["important"].as_array().map(Vec::len),
            Some(2)
        );
        let kinds: Vec<&str> = section["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|f| f["kind"].as_str())
            .collect();
        assert_eq!(kinds, ["bots_blocked"], "{section}");
    }

    /// A `Done` audit must stop being served from the in-memory map once it's cached:
    /// otherwise `progress` never gets cleared (it would still hold the last `Running`
    /// value forever) and the map leaks one entry per audit for the life of the process.
    #[tokio::test]
    async fn a_done_audit_is_evicted_from_memory_and_served_only_from_the_cache() {
        let site = SiteBuilder::new().html("/", "Home", &[]).start().await;
        let (backend, dir) = backend();
        let id = audit_fast(&backend, site.url("/")).await;

        let state = backend.get_audit(&id).await.unwrap();
        assert_eq!(
            state.progress, None,
            "a Done audit must not report stale progress"
        );

        // If the entry were still held in memory, deleting its cache file would have no
        // effect on get_audit; since it's evicted, deleting the file must make it 404.
        std::fs::remove_file(dir.path().join(format!("{}.json", id.0))).unwrap();
        let err = backend.get_audit(&id).await.unwrap_err();
        assert!(matches!(err, BackendError::AuditNotFound(_)));
    }

    #[tokio::test]
    async fn issue_urls_paginate_without_gaps_or_repeats() {
        let links: Vec<String> = (0..6).map(|i| format!("/no-title-{i}")).collect();
        let link_refs: Vec<&str> = links.iter().map(String::as_str).collect();
        let mut builder = SiteBuilder::new().html("/", "Home", &link_refs);
        for i in 0..6 {
            builder = builder.page(
                &format!("/no-title-{i}"),
                codoseo_testkit::Page::html("<html><body>no title</body></html>"),
            );
        }
        let site = builder.start().await;
        let (backend, _dir) = backend();
        let id = audit_fast(&backend, site.url("/")).await;

        let page1 = backend
            .issue_urls(&id, CheckId::TitleMissing, 4, 0)
            .await
            .unwrap();
        let page2 = backend
            .issue_urls(&id, CheckId::TitleMissing, 4, 4)
            .await
            .unwrap();
        assert_eq!(page1.len(), 4);
        assert_eq!(page2.len(), 2);
        let all: HashSet<_> = page1
            .iter()
            .chain(page2.iter())
            .map(|r| r.url.clone())
            .collect();
        assert_eq!(all.len(), 6, "no gaps or repeats across the two pages");
    }

    #[tokio::test]
    async fn page_errors_for_a_url_not_in_the_snapshot() {
        let site = SiteBuilder::new().html("/", "Home", &[]).start().await;
        let (backend, _dir) = backend();
        let id = audit_fast(&backend, site.url("/")).await;

        let err = backend
            .page(&id, &site.url("/not-crawled"))
            .await
            .unwrap_err();
        assert!(matches!(err, BackendError::PageNotFound(_)));
    }

    #[tokio::test]
    async fn check_page_matches_the_cli_check_commands_fields() {
        let site = SiteBuilder::new().html("/", "Hello", &[]).start().await;
        let (backend, _dir) = backend();
        let page = backend.check_page(site.url("/")).await.unwrap();
        assert_eq!(page.fields.title.as_deref(), Some("Hello"));
        assert_eq!(page.status, 200);
    }

    #[tokio::test]
    async fn check_robots_reports_disallow() {
        let site = SiteBuilder::new()
            .robots(200, "User-agent: *\nDisallow: /private\n")
            .start()
            .await;
        let (backend, _dir) = backend();
        let report = backend
            .check_robots(site.url("/"), Some("/private/page".to_owned()))
            .await
            .unwrap();
        assert!(!report.allowed);
    }

    #[tokio::test]
    async fn check_ai_access_reports_bots_declared_and_engines() {
        let site = SiteBuilder::new()
            .robots(
                200,
                "User-agent: GPTBot\nDisallow: /\n\nUser-agent: *\nAllow: /\nContent-Signal: ai-train=no\n",
            )
            .page(
                "/",
                codoseo_testkit::Page::html(
                    "<html><head><title>Home</title><meta name=\"robots\" content=\"noindex\"></head><body>hello</body></html>",
                ),
            )
            .start()
            .await;
        let (backend, _dir) = backend();
        let report = backend.check_ai_access(site.url("/")).await.unwrap();
        let gpt = report.bots.iter().find(|b| b.token == "GPTBot").unwrap();
        assert!(!gpt.allowed);
        assert_eq!(gpt.line, Some(2));
        let search = report
            .bots
            .iter()
            .find(|b| b.token == "OAI-SearchBot")
            .unwrap();
        assert!(search.allowed);
        assert_eq!(report.declared.content_signals.len(), 1);
        assert_eq!(report.page_status, Some(200));
        assert!(
            report
                .engines
                .iter()
                .any(|e| e.effect == codoseo_geo::eligibility::Effect::Excluded)
        );
        assert!(report.summary.contains("GPTBot"), "{}", report.summary);
    }

    #[tokio::test]
    async fn check_redirects_follows_hops() {
        let site = SiteBuilder::new()
            .page("/old", codoseo_testkit::Page::redirect(301, "/new"))
            .html("/new", "New", &[])
            .start()
            .await;
        let (backend, _dir) = backend();
        let report = backend.check_redirects(site.url("/old")).await.unwrap();
        assert_eq!(report.hops.len(), 1);
        assert_eq!(report.final_status, Some(200));
    }

    #[tokio::test]
    async fn compare_diffs_two_saved_audits() {
        let site = SiteBuilder::new().html("/", "Before", &[]).start().await;
        let (backend, _dir) = backend();
        let before = audit_fast(&backend, site.url("/")).await;
        let after = audit_fast(&backend, site.url("/")).await;

        let changes = backend.compare(&before, &after).await.unwrap();
        assert!(changes.is_empty(), "same page crawled twice has no changes");
    }
}
