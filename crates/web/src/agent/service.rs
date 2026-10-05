//! The agent API's one service layer. The REST routes (`/api/v1`) and the cloud MCP tools both
//! call these methods, so they return the same JSON ([`codoseo_mcp::cloud::types`]) and charge
//! the same daily quota.
//!
//! Every keyed method takes the already-authenticated [`ApiCaller`], counts one call against
//! the account's allowance first (a refused call counts nothing), and only then looks at its
//! arguments. Arguments arrive as the strings a caller sent (ids, check slugs, severities) and
//! are validated here, after the charge, so a malformed call costs the same as any other.
//! Another account's site is the same `NotFound` as an unknown id.

use std::cmp::Reverse;
use std::collections::HashMap;

use codoseo_checks::def;
use codoseo_core::check::{CheckId, IssueBits, Severity};
use codoseo_core::plan::PlanLimits;
use codoseo_mcp::cloud::types::{
    ActiveCrawl, ChangeInfo, ChangesPage, CrawlHealth, CrawlQueued, IssueUrlsPage, PageInfo,
    PageIssue, SiteHealth, SiteInfo, Usage,
};
use codoseo_mcp::local::stop_reason_words;
use codoseo_mcp::types::{FailingCheck, MAX_FAILING_CHECKS, UrlRow};
use codoseo_store::api_keys::{self, Charge};
use codoseo_store::crawls::{self, Crawl, CrawlStatus, ManualOutcome, ManualWindow};
use codoseo_store::explorer::{self, PageFilter};
use codoseo_store::reports;
use codoseo_store::sites::{self, Site};
use time::{OffsetDateTime, Time};
use url::Url;
use uuid::Uuid;

use super::auth::ApiCaller;
use super::error::AgentError;
use crate::routes::crawls::{limit_message, manual_priority};
use crate::state::AppState;

/// Rows a list returns when the caller doesn't say.
pub const DEFAULT_LIMIT: u32 = 50;
/// The most rows one call returns.
pub const MAX_LIMIT: u32 = 200;
/// Example URLs listed per failing check.
const EXAMPLES_PER_CHECK: i64 = 3;

/// Where the caller stands against today's allowance after a call. `None` fields mean the plan
/// has no limit (self-hosted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quota {
    pub limit: Option<u32>,
    pub remaining: Option<u32>,
}

/// A keyed call's result, with the quota it left behind (`None` when the charge itself failed
/// on a database error). The REST layer puts the quota in headers; MCP only needs the outcome.
#[derive(Debug)]
pub struct Reply<T> {
    pub quota: Option<Quota>,
    pub outcome: Result<T, AgentError>,
}

impl<T> Reply<T> {
    pub fn into_result(self) -> Result<T, AgentError> {
        self.outcome
    }
}

/// The next 00:00 UTC after `now`, when the daily count starts again.
pub fn next_reset(now: OffsetDateTime) -> OffsetDateTime {
    let now = now.to_offset(time::UtcOffset::UTC);
    now.replace_time(Time::MIDNIGHT) + time::Duration::days(1)
}

pub struct AgentService<'a> {
    state: &'a AppState,
}

impl<'a> AgentService<'a> {
    pub fn new(state: &'a AppState) -> AgentService<'a> {
        AgentService { state }
    }

    /// Counts one call, then runs `work`. `work` is a future that hasn't started: nothing in it
    /// runs when the charge is refused.
    async fn metered<T>(
        &self,
        caller: &ApiCaller,
        work: impl Future<Output = Result<T, AgentError>>,
    ) -> Reply<T> {
        let limit = PlanLimits::for_plan(caller.account.plan).api_calls_per_day;
        match api_keys::charge(&self.state.pool, caller.account.id, limit).await {
            Err(e) => Reply {
                quota: None,
                outcome: Err(e.into()),
            },
            Ok(Charge::OverQuota { limit }) => Reply {
                quota: Some(Quota {
                    limit: Some(limit),
                    remaining: Some(0),
                }),
                outcome: Err(AgentError::QuotaExceeded {
                    limit,
                    retry_after_secs: seconds_until_reset(),
                }),
            },
            Ok(Charge::Ok { used, limit }) => Reply {
                quota: Some(Quota {
                    limit,
                    remaining: limit.map(|l| l.saturating_sub(clamp_u32(used))),
                }),
                outcome: work.await,
            },
        }
    }

    pub async fn list_sites(&self, caller: &ApiCaller) -> Reply<Vec<SiteInfo>> {
        self.metered(caller, self.list_sites_work(caller)).await
    }

    pub async fn site_health(&self, caller: &ApiCaller, site: &str) -> Reply<SiteHealth> {
        self.metered(caller, self.site_health_work(caller, site))
            .await
    }

    pub async fn issue_urls(
        &self,
        caller: &ApiCaller,
        site: &str,
        check: &str,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Reply<IssueUrlsPage> {
        self.metered(
            caller,
            self.issue_urls_work(caller, site, check, limit, offset),
        )
        .await
    }

    pub async fn page(&self, caller: &ApiCaller, site: &str, url: &str) -> Reply<PageInfo> {
        self.metered(caller, self.page_work(caller, site, url))
            .await
    }

    pub async fn changes(
        &self,
        caller: &ApiCaller,
        site: &str,
        severity: Option<&str>,
        limit: Option<u32>,
    ) -> Reply<ChangesPage> {
        self.metered(caller, self.changes_work(caller, site, severity, limit))
            .await
    }

    /// Queues a manual crawl exactly as the Run crawl button does: the plan's allowance and
    /// lane, one crawl at a time.
    pub async fn run_crawl(&self, caller: &ApiCaller, site: &str) -> Reply<CrawlQueued> {
        self.metered(caller, self.run_crawl_work(caller, site))
            .await
    }

    /// Today's calls and the allowance. Free: it isn't counted.
    pub async fn usage(&self, caller: &ApiCaller) -> Reply<Usage> {
        let limit = PlanLimits::for_plan(caller.account.plan).api_calls_per_day;
        let outcome = api_keys::usage_today(&self.state.pool, caller.account.id)
            .await
            .map_err(AgentError::from)
            .map(|calls| {
                let calls = clamp_u32(calls);
                Usage {
                    calls_today: calls,
                    limit,
                    remaining: limit.map(|l| l.saturating_sub(calls)),
                    resets_at: next_reset(OffsetDateTime::now_utc()),
                }
            });
        Reply {
            quota: outcome.as_ref().ok().map(|u| Quota {
                limit: u.limit,
                remaining: u.remaining,
            }),
            outcome,
        }
    }

    /// One of the caller's sites; another account's looks exactly like an unknown id.
    async fn site(&self, caller: &ApiCaller, raw: &str) -> Result<Site, AgentError> {
        let id: Uuid = raw
            .trim()
            .parse()
            .map_err(|_| AgentError::site_not_found())?;
        sites::get_for_account(&self.state.pool, caller.account.id, id)
            .await?
            .ok_or_else(AgentError::site_not_found)
    }

    async fn list_sites_work(&self, caller: &ApiCaller) -> Result<Vec<SiteInfo>, AgentError> {
        let pool = &self.state.pool;
        let list = sites::list_for_account(pool, caller.account.id).await?;
        let latest: HashMap<Uuid, (Option<i16>, Option<OffsetDateTime>)> =
            crawls::latest_done_for_account(pool, caller.account.id)
                .await?
                .into_iter()
                .map(|(id, score, at)| (id, (score, at)))
                .collect();
        Ok(list
            .into_iter()
            .map(|s| {
                let (score, at) = latest.get(&s.id).copied().unwrap_or((None, None));
                SiteInfo {
                    id: s.id,
                    domain: s.domain,
                    start_url: s.start_url,
                    monitoring_active: s.monitoring_active,
                    schedule: s.schedule,
                    health_score: score.and_then(|n| u8::try_from(n).ok()),
                    last_crawled_at: at,
                }
            })
            .collect())
    }

    async fn site_health_work(
        &self,
        caller: &ApiCaller,
        site: &str,
    ) -> Result<SiteHealth, AgentError> {
        let pool = &self.state.pool;
        let site = self.site(caller, site).await?;
        let latest = match crawls::latest_done(pool, site.id).await? {
            Some(crawl) => Some(self.crawl_health(&crawl).await?),
            None => None,
        };
        let active = crawls::active(pool, site.id).await?.map(|c| ActiveCrawl {
            crawl_id: c.id,
            number: c.number,
            status: if c.status == CrawlStatus::Running {
                "running"
            } else {
                "queued"
            }
            .to_owned(),
            pages_done: c.progress().map(|p| p.pages_done),
        });
        let audit_url = self
            .state
            .config
            .base_url
            .join(&format!("s/{}/audit", site.id))
            .map_or_else(|_| format!("/s/{}/audit", site.id), |u| u.to_string());
        Ok(SiteHealth {
            id: site.id,
            next_crawl_at: sites::next_crawl_at(pool, site.id).await?,
            domain: site.domain,
            start_url: site.start_url,
            monitoring_active: site.monitoring_active,
            schedule: site.schedule,
            latest_crawl: latest,
            active_crawl: active,
            audit_url,
        })
    }

    /// A finished crawl's score, counts and failing checks (most severe first, with example
    /// URLs fetched in one query).
    async fn crawl_health(&self, crawl: &Crawl) -> Result<CrawlHealth, AgentError> {
        let summary = crawl.summary();
        let mut failing: Vec<(CheckId, u32)> = summary
            .as_ref()
            .map(|s| {
                s.counts
                    .iter()
                    .filter_map(|(slug, n)| Some((CheckId::from_slug(slug)?, *n)))
                    .collect()
            })
            .unwrap_or_default();
        failing.sort_by_key(|&(id, n)| (def(id).severity, Reverse(n), id));
        let more = failing.len().saturating_sub(MAX_FAILING_CHECKS);
        failing.truncate(MAX_FAILING_CHECKS);
        let ids: Vec<CheckId> = failing.iter().map(|&(id, _)| id).collect();
        let mut examples =
            explorer::example_urls(&self.state.pool, crawl.id, &ids, EXAMPLES_PER_CHECK).await?;
        let failing_checks = failing
            .into_iter()
            .map(|(check, count)| {
                let d = def(check);
                FailingCheck {
                    check,
                    title: d.title.to_owned(),
                    severity: d.severity,
                    count,
                    example_urls: examples
                        .remove(&check)
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|u| Url::parse(u).ok())
                        .collect(),
                }
            })
            .collect();
        Ok(CrawlHealth {
            crawl_id: crawl.id,
            number: crawl.number,
            finished_at: crawl.finished_at,
            health_score: crawl.health_score.and_then(|n| u8::try_from(n).ok()),
            checks_passed: crawl.checks_passed.and_then(|n| u16::try_from(n).ok()),
            checks_total: crawl.checks_total.and_then(|n| u16::try_from(n).ok()),
            pages_crawled: summary.as_ref().map_or(0, |s| s.report_summary.pages),
            stop_reason: summary.as_ref().map_or_else(
                || "unknown".to_owned(),
                |s| stop_reason_words(&s.stop_reason),
            ),
            failing_checks,
            more_failing_checks: u16::try_from(more).unwrap_or(u16::MAX),
        })
    }

    async fn issue_urls_work(
        &self,
        caller: &ApiCaller,
        site: &str,
        check: &str,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<IssueUrlsPage, AgentError> {
        let pool = &self.state.pool;
        let site = self.site(caller, site).await?;
        let check = CheckId::from_slug(check.trim()).ok_or_else(|| {
            AgentError::BadRequest(format!(
                "Unknown check \"{check}\". Use a check slug such as title_missing; \
                 get_site_health lists the failing ones."
            ))
        })?;
        let limit = clamp_limit(limit);
        let offset = offset.unwrap_or(0);
        let mut page = IssueUrlsPage {
            site_id: site.id,
            check,
            title: def(check).title.to_owned(),
            crawl_number: None,
            total: 0,
            limit,
            offset,
            urls: Vec::new(),
            next_offset: None,
        };
        let Some(crawl) = crawls::latest_done(pool, site.id).await? else {
            return Ok(page);
        };
        let filter = PageFilter::Check(check);
        let (matching, _) = explorer::match_count(pool, crawl.id, filter, "").await?;
        let rows =
            explorer::rows_at(pool, crawl.id, filter, i64::from(offset), i64::from(limit)).await?;
        page.crawl_number = Some(crawl.number);
        page.total = clamp_u32(matching);
        let shown = u32::try_from(rows.len()).unwrap_or(u32::MAX);
        page.urls = rows
            .into_iter()
            .filter_map(|r| {
                Some(UrlRow {
                    url: Url::parse(&r.url).ok()?,
                    status: r.status,
                    title: r.title,
                    indexability: r.indexability,
                })
            })
            .collect();
        page.next_offset = offset.checked_add(shown).filter(|next| *next < page.total);
        Ok(page)
    }

    async fn page_work(
        &self,
        caller: &ApiCaller,
        site: &str,
        url: &str,
    ) -> Result<PageInfo, AgentError> {
        let pool = &self.state.pool;
        let site = self.site(caller, site).await?;
        if url.trim().is_empty() {
            return Err(AgentError::BadRequest("The url is required.".to_owned()));
        }
        let base = Url::parse(&site.start_url)
            .map_err(|e| AgentError::Internal(format!("site start url: {e}")))?;
        let url = codoseo_core::url::normalize(&base, url)
            .ok_or_else(|| AgentError::BadRequest("That is not a valid page URL.".to_owned()))?;
        let crawl = crawls::latest_done(pool, site.id).await?.ok_or_else(|| {
            AgentError::NotFound("This site has no finished crawl yet.".to_owned())
        })?;
        let page = explorer::page(pool, site.id, crawl.id, codoseo_core::url::url_hash(&url))
            .await?
            .ok_or_else(|| {
                AgentError::NotFound("That URL is not in the latest crawl of this site.".to_owned())
            })?;
        let mut issues: Vec<PageIssue> = IssueBits(page.issues)
            .iter()
            .filter_map(CheckId::from_bit)
            .map(|check| PageIssue {
                check,
                title: def(check).title.to_owned(),
                severity: def(check).severity,
            })
            .collect();
        issues.sort_by_key(|i| (i.severity, i.check));
        Ok(PageInfo {
            site_id: site.id,
            crawl_number: crawl.number,
            url: page.url,
            status: page.status,
            redirect_chain: page.redirect_chain,
            redirect_target: page.redirect_target,
            response_ms: page.response_ms,
            size_bytes: page.size_bytes,
            content_type: page.content_type,
            depth: page.depth,
            in_sitemap: page.in_sitemap,
            indexability: page.indexability,
            title: page.title,
            meta_description: page.meta_description,
            meta_robots: page.meta_robots,
            x_robots_tag: page.x_robots_tag,
            canonical: page.canonical,
            h1: page.h1,
            h2: page.h2,
            word_count: page.word_count,
            inlinks: page.inlinks,
            outlinks_internal: page.outlinks_internal,
            outlinks_external: page.outlinks_external,
            issues,
        })
    }

    async fn changes_work(
        &self,
        caller: &ApiCaller,
        site: &str,
        severity: Option<&str>,
        limit: Option<u32>,
    ) -> Result<ChangesPage, AgentError> {
        let pool = &self.state.pool;
        let site = self.site(caller, site).await?;
        let severity = severity
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(parse_severity)
            .transpose()?;
        let mut out = ChangesPage {
            site_id: site.id,
            crawl_number: None,
            crawl_finished_at: None,
            total: 0,
            changes: Vec::new(),
        };
        let Some(crawl) = crawls::latest_done(pool, site.id).await? else {
            return Ok(out);
        };
        let counts = reports::change_kind_counts(pool, crawl.id).await?;
        let rows =
            reports::changes_for_crawl(pool, crawl.id, severity, i64::from(clamp_limit(limit)))
                .await?;
        out.crawl_number = Some(crawl.number);
        out.crawl_finished_at = crawl.finished_at;
        out.total = clamp_u32(severity.map_or_else(|| counts.total(), |s| counts.severity(s)));
        out.changes = rows
            .into_iter()
            .map(|c| ChangeInfo {
                kind: c.kind,
                severity: c.severity,
                url: c.url,
                before: c.before,
                after: c.after,
            })
            .collect();
        Ok(out)
    }

    async fn run_crawl_work(
        &self,
        caller: &ApiCaller,
        site: &str,
    ) -> Result<CrawlQueued, AgentError> {
        let site = self.site(caller, site).await?;
        let plan = caller.account.plan;
        let allowance = PlanLimits::for_plan(plan).manual_crawls;
        let outcome = crawls::enqueue_manual_checked(
            &self.state.pool,
            site.id,
            &site.domain,
            manual_priority(plan),
            ManualWindow::for_allowance(allowance),
        )
        .await?;
        match outcome {
            ManualOutcome::Queued { id, number } => Ok(CrawlQueued {
                site_id: site.id,
                crawl_id: id,
                number,
                status: "queued".to_owned(),
            }),
            ManualOutcome::Busy(status) => {
                let what = if status == CrawlStatus::Running {
                    "running"
                } else {
                    "queued"
                };
                Err(AgentError::CrawlInProgress(format!(
                    "A crawl is already {what} for this site."
                )))
            }
            ManualOutcome::LimitReached { frees_at } => Err(AgentError::PlanLimit(limit_message(
                plan,
                allowance,
                frees_at - OffsetDateTime::now_utc(),
            ))),
        }
    }
}

fn seconds_until_reset() -> u64 {
    let now = OffsetDateTime::now_utc();
    u64::try_from((next_reset(now) - now).whole_seconds().max(1)).unwrap_or(1)
}

fn clamp_limit(limit: Option<u32>) -> u32 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn clamp_u32(n: i64) -> u32 {
    u32::try_from(n.max(0)).unwrap_or(u32::MAX)
}

fn parse_severity(s: &str) -> Result<Severity, AgentError> {
    match s.to_ascii_lowercase().as_str() {
        "critical" => Ok(Severity::Critical),
        "warning" => Ok(Severity::Warning),
        "notice" => Ok(Severity::Notice),
        _ => Err(AgentError::BadRequest(format!(
            "Unknown severity \"{s}\". Use critical, warning or notice."
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_count_starts_again_at_the_next_midnight_utc() {
        let now = time::macros::datetime!(2026-10-05 23:59:30 UTC);
        assert_eq!(
            next_reset(now),
            time::macros::datetime!(2026-10-06 0:00 UTC)
        );
        let now = time::macros::datetime!(2026-12-31 0:00 UTC);
        assert_eq!(
            next_reset(now),
            time::macros::datetime!(2027-01-01 0:00 UTC)
        );
    }

    #[test]
    fn limits_are_clamped() {
        assert_eq!(clamp_limit(None), 50);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(10_000)), 200);
    }

    #[test]
    fn severities_parse_in_any_case() {
        assert_eq!(parse_severity("Critical").unwrap(), Severity::Critical);
        assert!(parse_severity("severe").is_err());
    }
}
