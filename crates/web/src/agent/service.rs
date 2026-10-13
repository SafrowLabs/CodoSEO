//! The agent API's one service layer. The REST routes (`/api/v1`) and the cloud MCP tools both
//! call these methods, so they return the same JSON ([`codoseo_mcp::cloud::types`]) and charge
//! the same daily quota.
//!
//! Every keyed method takes the already-authenticated [`ApiCaller`], counts one call against
//! the account's allowance first (a refused call counts nothing), and only then looks at its
//! arguments. Arguments arrive as the strings a caller sent (ids, check slugs, severities) and
//! are validated here, after the charge, so a malformed call costs the same as any other.
//! Another account's site is the same `NotFound` as an unknown id.

use std::collections::HashMap;

use codoseo_checks::def;
use codoseo_core::check::{CheckId, IssueBits, Severity};
use codoseo_core::plan::PlanLimits;
use codoseo_geo::eligibility::{Effect, engines};
use codoseo_geo::findings::Grade;
use codoseo_geo::probe::{ProbeReport, Verdict};
use codoseo_geo::report::AccessReport;
use codoseo_geo::{Honours, Intent, Stance, registry};
use codoseo_mcp::cloud::types::{
    ActiveCrawl, AiAccessInfo, AiBotInfo, AiContentInfo, AiContentPage, AiEngineInfo, AiFixInfo,
    AiIncidentInfo, AiLagInfo, AiLlmsTxtInfo, AiNotProbedInfo, AiProbeBotInfo, AiProbesInfo,
    AiRecheckInfo, AiRecheckQueued, AiRemedyInfo, AiRobotsInfo, AiSnippetInfo, ChangeInfo,
    ChangesPage, CrawlHealth, CrawlQueued, IssueUrlsPage, MAX_AI_CONTENT_PAGES, PageInfo,
    PageIssue, RedirectHop, SiteHealth, SiteInfo, Usage, clip, clip_change_text,
};
use codoseo_mcp::local::{stop_reason_code, stop_reason_words};
use codoseo_mcp::types::{FailingCheck, MAX_FAILING_CHECKS, UrlRow, rank_failing};
use codoseo_store::accounts::Account;
use codoseo_store::api_keys::{self, Charge};
use codoseo_store::crawls::{self, Crawl, CrawlStatus, ManualOutcome, ManualWindow};
use codoseo_store::explorer::{self, PageFilter};
use codoseo_store::geo::{self, Incident, Recheck};
use codoseo_store::reports;
use codoseo_store::sites::{self, Site};
use sqlx::PgPool;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

use super::auth::ApiCaller;
use super::error::AgentError;
use crate::ai_incident::{self, IncidentRecord, Phase};
use crate::crawl_policy::{limit_message, manual_priority};
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

pub struct AgentService<'a> {
    pub(super) state: &'a AppState,
}

impl<'a> AgentService<'a> {
    pub fn new(state: &'a AppState) -> AgentService<'a> {
        AgentService { state }
    }

    /// Counts one call, then runs `work`. `work` is a future that hasn't started: nothing in it
    /// runs when the charge is refused.
    pub(super) async fn metered<T>(
        &self,
        caller: &ApiCaller,
        work: impl Future<Output = Result<T, AgentError>>,
    ) -> Reply<T> {
        self.metered_account(&caller.account, work).await
    }

    /// [`Self::metered`] for a call that carries no key but acts for an account (a deploy hook's
    /// secret URL): the account's daily allowance is the same.
    pub(super) async fn metered_account<T>(
        &self,
        account: &Account,
        work: impl Future<Output = Result<T, AgentError>>,
    ) -> Reply<T> {
        let limit = PlanLimits::for_plan(account.plan).api_calls_per_day;
        match api_keys::charge(&self.state.pool, account.id, limit).await {
            Err(e) => Reply {
                quota: None,
                outcome: Err(e.into()),
            },
            Ok(Charge::OverQuota { limit }) => {
                // The reset comes from Postgres' clock, the one the day was counted by.
                let outcome = match api_keys::day_end(&self.state.pool, None).await {
                    Ok(end) => Err(AgentError::QuotaExceeded {
                        limit,
                        retry_after_secs: u64::try_from(end.seconds_left).unwrap_or(1),
                    }),
                    Err(e) => Err(e.into()),
                };
                Reply {
                    quota: Some(Quota {
                        limit: Some(limit),
                        remaining: Some(0),
                    }),
                    outcome,
                }
            }
            Ok(Charge::Ok { used, limit }) => Reply {
                quota: Some(Quota {
                    limit,
                    remaining: limit.map(|l| l.saturating_sub(clamp_u32(used))),
                }),
                outcome: work.await,
            },
        }
    }

    /// Counts a call that the caller's own arguments already made unanswerable (a query string
    /// that doesn't parse) and answers with `error`: every authenticated request costs one.
    pub async fn refuse<T>(&self, caller: &ApiCaller, error: AgentError) -> Reply<T> {
        self.metered(caller, async { Err(error) }).await
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
        offset: Option<u32>,
    ) -> Reply<ChangesPage> {
        self.metered(
            caller,
            self.changes_work(caller, site, severity, limit, offset),
        )
        .await
    }

    pub async fn ai_access(&self, caller: &ApiCaller, site: &str) -> Reply<AiAccessInfo> {
        self.metered(caller, self.ai_access_work(caller, site))
            .await
    }

    /// The site's open AI access incidents and those resolved in the last 90 days, one record
    /// each: what the incident export has.
    pub async fn incident_export(
        &self,
        caller: &ApiCaller,
        site: &str,
    ) -> Reply<Vec<IncidentRecord>> {
        self.metered(caller, async {
            let site = self.site(caller, site).await?;
            Ok(
                ai_incident::export_records(&self.state.pool, site.id, OffsetDateTime::now_utc())
                    .await?,
            )
        })
        .await
    }

    /// What to change for one incident, and the texts to hand to whoever changes it.
    pub async fn incident_fix(
        &self,
        caller: &ApiCaller,
        site: &str,
        incident: i64,
    ) -> Reply<AiFixInfo> {
        self.metered(caller, self.incident_fix_work(caller, site, incident))
            .await
    }

    /// Queues a recheck of one incident like the Recheck now button: the site owner's plan's
    /// daily allowance, one at a time per incident.
    pub async fn recheck_incident(
        &self,
        caller: &ApiCaller,
        site: &str,
        incident: i64,
    ) -> Reply<AiRecheckQueued> {
        self.metered(caller, async {
            let site = self.site(caller, site).await?;
            let limits = PlanLimits::for_plan(caller.account.plan);
            let q = ai_incident::request_recheck(
                &self.state.pool,
                site.id,
                incident,
                caller.account.id,
                &limits,
            )
            .await?;
            Ok(AiRecheckQueued {
                recheck_id: q.id,
                due_at: q.due_at,
            })
        })
        .await
    }

    /// Snoozes an incident for 1, 7 or 30 days. Answers with the incident as it is now.
    pub async fn snooze_incident(
        &self,
        caller: &ApiCaller,
        site: &str,
        incident: i64,
        days: Option<i64>,
    ) -> Reply<AiIncidentInfo> {
        self.metered(caller, async {
            let site = self.site(caller, site).await?;
            let days = ai_incident::snooze_days(days)?;
            let until = OffsetDateTime::now_utc() + time::Duration::days(days);
            geo::snooze(&self.state.pool, site.id, incident, until)
                .await
                .map_err(ai_incident::Refusal::from)?;
            self.incident_now(site.id, incident).await
        })
        .await
    }

    pub async fn unsnooze_incident(
        &self,
        caller: &ApiCaller,
        site: &str,
        incident: i64,
    ) -> Reply<AiIncidentInfo> {
        self.metered(caller, async {
            let site = self.site(caller, site).await?;
            geo::unsnooze(&self.state.pool, site.id, incident)
                .await
                .map_err(ai_incident::Refusal::from)?;
            self.incident_now(site.id, incident).await
        })
        .await
    }

    /// Accepts an incident as it is: it stays listed but no longer counts or alerts.
    pub async fn accept_incident(
        &self,
        caller: &ApiCaller,
        site: &str,
        incident: i64,
    ) -> Reply<AiIncidentInfo> {
        self.metered(caller, async {
            let site = self.site(caller, site).await?;
            geo::accept(&self.state.pool, site.id, incident, caller.account.id)
                .await
                .map_err(ai_incident::Refusal::from)?;
            self.incident_now(site.id, incident).await
        })
        .await
    }

    pub async fn unaccept_incident(
        &self,
        caller: &ApiCaller,
        site: &str,
        incident: i64,
    ) -> Reply<AiIncidentInfo> {
        self.metered(caller, async {
            let site = self.site(caller, site).await?;
            geo::unaccept(&self.state.pool, site.id, incident)
                .await
                .map_err(ai_incident::Refusal::from)?;
            self.incident_now(site.id, incident).await
        })
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
        let pool = &self.state.pool;
        let outcome = async {
            let calls = clamp_u32(api_keys::usage_today(pool, caller.account.id).await?);
            let end = api_keys::day_end(pool, None).await?;
            Ok::<_, AgentError>(Usage {
                calls_today: calls,
                limit,
                remaining: limit.map(|l| l.saturating_sub(calls)),
                resets_at: end.resets_at,
            })
        }
        .await;
        Reply {
            quota: outcome.as_ref().ok().map(|u| Quota {
                limit: u.limit,
                remaining: u.remaining,
            }),
            outcome,
        }
    }

    /// One of the caller's sites; another account's looks exactly like an unknown id.
    pub(super) async fn site(&self, caller: &ApiCaller, raw: &str) -> Result<Site, AgentError> {
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
            Some(crawl) => Some(crawl_health(pool, &crawl).await?),
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
        let check = parse_check(check)?;
        let mut page = IssueUrlsPage {
            site_id: site.id,
            check,
            title: def(check).title.to_owned(),
            crawl_number: None,
            total: 0,
            limit: clamp_limit(limit),
            offset: offset.unwrap_or(0),
            urls: Vec::new(),
            next_offset: None,
        };
        let Some(crawl) = crawls::latest_done(pool, site.id).await? else {
            return Ok(page);
        };
        let rows = issue_page(pool, crawl.id, check, limit, offset).await?;
        page.crawl_number = Some(crawl.number);
        page.total = rows.total;
        page.urls = rows.urls;
        page.next_offset = rows.next_offset;
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
            redirect_chain: page
                .redirect_chain
                .into_iter()
                .map(|(status, url)| RedirectHop { status, url })
                .collect(),
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
        offset: Option<u32>,
    ) -> Result<ChangesPage, AgentError> {
        let pool = &self.state.pool;
        let site = self.site(caller, site).await?;
        let severity = severity
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(parse_severity)
            .transpose()?;
        let (limit, offset) = (clamp_limit(limit), offset.unwrap_or(0));
        let mut out = ChangesPage {
            site_id: site.id,
            crawl_number: None,
            crawl_finished_at: None,
            total: 0,
            limit,
            offset,
            changes: Vec::new(),
            next_offset: None,
        };
        let Some(crawl) = crawls::latest_done(pool, site.id).await? else {
            return Ok(out);
        };
        let counts = reports::change_kind_counts(pool, crawl.id).await?;
        let rows = reports::changes_for_crawl_at(
            pool,
            crawl.id,
            severity,
            i64::from(offset),
            i64::from(limit),
        )
        .await?;
        out.crawl_number = Some(crawl.number);
        out.crawl_finished_at = crawl.finished_at;
        out.total = clamp_u32(severity.map_or_else(|| counts.total(), |s| counts.severity(s)));
        let shown = u32::try_from(rows.len()).unwrap_or(u32::MAX);
        out.changes = rows
            .into_iter()
            .map(|c| ChangeInfo {
                kind: c.kind,
                severity: c.severity,
                url: c.url,
                before: clip_change_text(&c.before),
                after: clip_change_text(&c.after),
            })
            .collect();
        out.next_offset = next_offset(offset, shown, out.total);
        Ok(out)
    }

    /// One incident of the site as it is now, after an action on it.
    async fn incident_now(&self, site_id: Uuid, id: i64) -> Result<AiIncidentInfo, AgentError> {
        let pool = &self.state.pool;
        let i = geo::incident(pool, site_id, id)
            .await?
            .ok_or_else(AgentError::incident_not_found)?;
        let rechecks = geo::latest_rechecks(pool, site_id).await?;
        Ok(incident_info(
            i,
            rechecks.get(&id),
            OffsetDateTime::now_utc(),
        ))
    }

    async fn incident_fix_work(
        &self,
        caller: &ApiCaller,
        site: &str,
        id: i64,
    ) -> Result<AiFixInfo, AgentError> {
        let pool = &self.state.pool;
        let site = self.site(caller, site).await?;
        let i = geo::incident(pool, site.id, id)
            .await?
            .ok_or_else(AgentError::incident_not_found)?;
        let latest = geo::latest_report(pool, site.id).await?;
        let ev = serde_json::from_value(i.evidence.clone()).unwrap_or_default();
        let fix = ai_incident::fix(&site.start_url, latest.as_ref().map(|l| &l.report), &i, &ev);
        let r = &fix.remedy;
        Ok(AiFixInfo {
            copy_fix: codoseo_geo::remedy::copy_fix(r),
            developer_task: codoseo_geo::remedy::developer_task(r, &fix.facts),
            agent_prompt: codoseo_geo::remedy::agent_prompt(r, &fix.facts),
            remedy: AiRemedyInfo {
                title: r.title.clone(),
                why: r.why.clone(),
                limits: r.limits.clone(),
                steps: r.steps.clone(),
                snippet: r.snippet.as_ref().map(|s| AiSnippetInfo {
                    label: s.label.clone(),
                    text: s.text.clone(),
                }),
                verify: r.verify.clone(),
                sources: r.sources.clone(),
                grade: r.grade,
            },
        })
    }

    async fn ai_access_work(
        &self,
        caller: &ApiCaller,
        site: &str,
    ) -> Result<AiAccessInfo, AgentError> {
        let pool = &self.state.pool;
        let site = self.site(caller, site).await?;
        let open = geo::open_incidents(pool, site.id).await?;
        let rechecks = geo::latest_rechecks(pool, site.id).await?;
        let now = OffsetDateTime::now_utc();
        let open_incidents = open
            .into_iter()
            .map(|i| {
                let latest = rechecks.get(&i.id);
                incident_info(i, latest, now)
            })
            .collect();
        let probes_enabled = geo::get_ai_probes(pool, site.id).await?;
        let Some(latest) = geo::latest_report(pool, site.id).await? else {
            return Ok(AiAccessInfo {
                site_id: site.id,
                checked_at: None,
                crawl_id: None,
                note: Some(
                    "No AI access report yet: it appears after the site's next crawl.".to_owned(),
                ),
                robots: None,
                open_incidents,
                bots: Vec::new(),
                engines: Vec::new(),
                declared: Default::default(),
                probes_enabled,
                probes: None,
                content: None,
                llms_txt: None,
                llms_txt_skipped: None,
            });
        };
        let intent = geo::get_intent(pool, site.id).await?;
        let report = latest.report;
        Ok(AiAccessInfo {
            site_id: site.id,
            checked_at: Some(latest.created_at),
            crawl_id: Some(latest.crawl_id),
            note: None,
            robots: Some(AiRobotsInfo {
                status: report.robots.status,
                availability: report.robots.availability,
            }),
            open_incidents,
            bots: ai_bots(&report, &intent),
            engines: ai_engines(&report),
            probes_enabled,
            probes: report
                .probes
                .as_ref()
                .filter(|_| probes_enabled)
                .map(|p| ai_probes(p, &intent)),
            content: ai_content(&report),
            llms_txt: report.llms_txt.as_ref().map(ai_llms_txt),
            llms_txt_skipped: report.llms_txt_skipped,
            declared: report.declared,
        })
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
        .await
        // The site was deleted between the lookup and the insert.
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => AgentError::site_not_found(),
            e => e.into(),
        })?;
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

/// An incident as the API and the MCP tools show it.
fn incident_info(i: Incident, latest: Option<&Recheck>, now: OffsetDateTime) -> AiIncidentInfo {
    let ev: codoseo_geo::findings::Evidence =
        serde_json::from_value(i.evidence.clone()).unwrap_or_default();
    let phase = ai_incident::phase(&i, latest);
    let snoozed_until = i.snoozed_until.filter(|t| *t > now);
    let accepted = i.accepted_at.is_some();
    let state = ai_incident::state(&i, latest, now);
    let lag = codoseo_geo::ledger::lag(i.kind, &ev);
    AiIncidentInfo {
        id: i.id,
        kind: i.kind.slug().to_owned(),
        severity: i.severity,
        title: clip(&i.title, MAX_AI_TEXT),
        summary: clip(&i.summary, MAX_AI_TEXT),
        opened_at: i.opened_at,
        last_seen_at: i.last_seen_at,
        first_seen: i.opened_at,
        grade: codoseo_geo::ledger::for_kind(i.kind).grade,
        sources: codoseo_geo::ledger::finding_sources(i.kind, &ev),
        state,
        snoozed_until,
        accepted,
        recovered_at: i.recovered_at,
        verified_at: i.verified_at,
        verify_due_at: match phase {
            Some(Phase::Verifying { due_at }) => Some(due_at),
            _ => None,
        },
        lag: AiLagInfo {
            hours: lag.hours,
            source: lag.source.map(str::to_owned),
        },
        affected_urls: ai_incident::affected_urls(&ev)
            .into_iter()
            .take(MAX_AFFECTED_URLS)
            .map(|u| clip(&u, MAX_AI_TEXT))
            .collect(),
        last_recheck: latest
            .and_then(ai_incident::last_recheck)
            .map(|r| AiRecheckInfo {
                outcome: r.outcome.to_owned(),
                at: r.at,
                notes: r.notes,
            }),
        subject: i.subject,
    }
}

/// Affected URLs listed per incident.
const MAX_AFFECTED_URLS: usize = 20;

/// How many rows a list call returns: `limit` if given, at most [`MAX_LIMIT`], at least 1.
pub fn clamp_limit(limit: Option<u32>) -> u32 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// The offset of the next page, when rows remain after `offset + shown`.
fn next_offset(offset: u32, shown: u32, total: u32) -> Option<u32> {
    offset.checked_add(shown).filter(|next| *next < total)
}

/// A check by slug.
pub fn parse_check(slug: &str) -> Result<CheckId, AgentError> {
    CheckId::from_slug(slug.trim()).ok_or_else(|| {
        let valid: Vec<&str> = CheckId::ALL.iter().map(|c| c.slug()).collect();
        AgentError::BadRequest(format!(
            "Unknown check \"{slug}\". The check slugs are: {}.",
            valid.join(", ")
        ))
    })
}

fn clamp_u32(n: i64) -> u32 {
    u32::try_from(n.max(0)).unwrap_or(u32::MAX)
}

/// `critical`, `warning` or `notice` (lowercase, as the web app's filters write them).
pub fn parse_severity(s: &str) -> Result<Severity, AgentError> {
    Severity::from_slug(s.trim()).ok_or_else(|| {
        AgentError::BadRequest(format!(
            "Unknown severity \"{s}\". Use critical, warning or notice."
        ))
    })
}

/// A finished crawl's score, counts and failing checks (most severe first, with example URLs
/// fetched in one query). It reads the crawl only, so it serves any crawl id the caller is
/// already allowed to see, a site's or a quick audit's.
pub async fn crawl_health(pool: &PgPool, crawl: &Crawl) -> Result<CrawlHealth, AgentError> {
    let summary = crawl.summary();
    let mut failing = rank_failing(
        summary
            .iter()
            .flat_map(|s| s.counts.iter())
            .filter_map(|(slug, n)| Some((CheckId::from_slug(slug)?, *n))),
    );
    let more = failing.len().saturating_sub(MAX_FAILING_CHECKS);
    failing.truncate(MAX_FAILING_CHECKS);
    let ids: Vec<CheckId> = failing.iter().map(|&(id, _)| id).collect();
    let mut examples = explorer::example_urls(pool, crawl.id, &ids, EXAMPLES_PER_CHECK).await?;
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
        stop_code: summary
            .as_ref()
            .map_or("unknown", |s| stop_reason_code(&s.stop_reason))
            .to_owned(),
        failing_checks,
        more_failing_checks: u16::try_from(more).unwrap_or(u16::MAX),
    })
}

/// One page of the pages of a crawl that fail `check`.
#[derive(Debug, Clone, PartialEq)]
pub struct IssueRows {
    /// Pages failing the check in the crawl.
    pub total: u32,
    pub limit: u32,
    pub offset: u32,
    pub urls: Vec<UrlRow>,
    /// Pass as `offset` for the next page; none on the last page.
    pub next_offset: Option<u32>,
}

/// The pages of `crawl_id` that fail `check`, `limit` (clamped) from `offset`, in crawl order.
pub async fn issue_page(
    pool: &PgPool,
    crawl_id: Uuid,
    check: CheckId,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<IssueRows, AgentError> {
    let (limit, offset) = (clamp_limit(limit), offset.unwrap_or(0));
    let filter = PageFilter::Check(check);
    let (matching, _) = explorer::match_count(pool, crawl_id, filter, "").await?;
    let rows =
        explorer::rows_at(pool, crawl_id, filter, i64::from(offset), i64::from(limit)).await?;
    let total = clamp_u32(matching);
    let shown = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    let urls = rows
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
    Ok(IssueRows {
        total,
        limit,
        offset,
        urls,
        next_offset: next_offset(offset, shown, total),
    })
}

/// Longest incident title or summary the AI access payload carries.
const MAX_AI_TEXT: usize = 400;

/// The important pages' raw HTML content: shells first, capped.
fn ai_content(report: &AccessReport) -> Option<AiContentInfo> {
    let content = report.content.as_ref()?;
    let mut pages: Vec<AiContentPage> = content
        .pages
        .iter()
        .filter_map(|c| {
            Some(AiContentPage {
                url: report.important.get(usize::from(c.url))?.url.clone(),
                verdict: c.verdict,
                signals: c.signals.clone(),
                words: c.words,
                title: c.title,
                h1: c.h1,
                description: c.description,
                canonical: c.canonical,
                jsonld: matches!(c.jsonld, codoseo_core::page::JsonLdStatus::Valid(_)),
                links_internal: c.links_internal,
            })
        })
        .collect();
    let total = u32::try_from(pages.len()).unwrap_or(u32::MAX);
    let shells = u32::try_from(content.shells().count()).unwrap_or(u32::MAX);
    // Stable: shells keep the report's order, as do the rest.
    pages.sort_by_key(|p| !p.verdict.is_shell());
    let more = pages.len().saturating_sub(MAX_AI_CONTENT_PAGES);
    pages.truncate(MAX_AI_CONTENT_PAGES);
    Some(AiContentInfo {
        grade: codoseo_geo::findings::Grade::AMinus,
        pages_checked: total,
        shells,
        pages,
        more: u32::try_from(more).unwrap_or(u32::MAX),
    })
}

fn ai_llms_txt(r: &codoseo_geo::llms::LlmsTxtReport) -> AiLlmsTxtInfo {
    let mut report = r.clone();
    for c in &mut report.checks {
        c.detail = clip(&c.detail, MAX_AI_TEXT);
    }
    for u in &mut report.link_summary.failing_urls {
        *u = clip(u, MAX_AI_TEXT);
    }
    report.error = report.error.map(|e| clip(&e, MAX_AI_TEXT));
    report.title = report.title.map(|t| clip(&t, codoseo_geo::llms::MAX_TITLE));
    AiLlmsTxtInfo {
        informational: true,
        report,
    }
}

/// One entry per registry bot, with whether robots.txt does the opposite of the owner's intent:
/// the bot is allowed but some important page is blocked, or it is blocked from the home page
/// while the owner wants it out and it is not (only bots that honour robots.txt count).
fn ai_bots(report: &AccessReport, intent: &Intent) -> Vec<AiBotInfo> {
    let total = u32::try_from(report.important.len()).unwrap_or(u32::MAX);
    registry()
        .bots
        .iter()
        .filter_map(|bot| {
            let access = report.bot(&bot.token)?;
            let blocked = u32::try_from(access.blocked.len()).unwrap_or(u32::MAX);
            let stance = intent.effective(bot);
            let honoured = bot.honours_robots == Honours::Yes;
            let conflicts = match stance {
                Stance::Allow => blocked > 0,
                Stance::Block => honoured && access.home_allowed == Some(true),
                Stance::Any => false,
            };
            Some(AiBotInfo {
                token: bot.token.clone(),
                operator: bot.operator.clone(),
                purpose: bot.purpose,
                intent: stance,
                home_allowed: access.home_allowed,
                important_blocked: blocked,
                important_total: total,
                conflicts,
            })
        })
        .collect()
}

/// The synthetic fetch check, one row per probed bot. A bot treated worse than CodoSEObot on any
/// page shows the first such result; otherwise the same page, then inconclusive, then robots.txt,
/// as on the AI access screen.
fn ai_probes(probes: &ProbeReport, intent: &Intent) -> AiProbesInfo {
    let bots = probes
        .bots
        .iter()
        .filter_map(|token| {
            let bot = registry().bot(token)?;
            let results: Vec<_> = probes
                .urls
                .iter()
                .flat_map(|u| &u.results)
                .filter(|r| &r.token == token)
                .collect();
            let verdicts: Vec<&Verdict> = results.iter().map(|r| &r.verdict).collect();
            // Pages its user agent was actually sent to: not robots.txt skips, nor pages whose
            // baseline gave nothing to compare with.
            let requested = results.iter().filter(|r| r.fetch.is_some()).count();
            let shown = verdicts
                .iter()
                .find(|v| v.is_positive())
                .or_else(|| verdicts.iter().find(|v| **v == &Verdict::Same))
                .or_else(|| {
                    verdicts
                        .iter()
                        .find(|v| matches!(v, Verdict::Inconclusive { .. }))
                })
                .or_else(|| verdicts.first())?;
            let count = |f: &dyn Fn(&Verdict) -> bool| {
                u32::try_from(verdicts.iter().filter(|v| f(v)).count()).unwrap_or(u32::MAX)
            };
            Some(AiProbeBotInfo {
                token: bot.token.clone(),
                purpose: bot.purpose,
                intent: intent.effective(bot),
                result: verdict_slug(shown).to_owned(),
                cause: shown.cause_slug().map(str::to_owned),
                vendor: match shown {
                    Verdict::Challenged { vendor } => Some(vendor.slug().to_owned()),
                    _ => None,
                },
                status: match shown {
                    Verdict::Blocked { status } => *status,
                    _ => None,
                },
                urls_affected: count(&Verdict::is_positive),
                urls_probed: u32::try_from(requested).unwrap_or(u32::MAX),
            })
        })
        .collect();
    AiProbesInfo {
        synthetic: true,
        grade: Grade::B,
        urls_probed: u32::try_from(probes.urls.len()).unwrap_or(u32::MAX),
        from: probes.from.clone(),
        stop: probes.stop,
        bots,
        not_probed: probes
            .skipped_bots
            .iter()
            .map(|s| AiNotProbedInfo {
                token: s.token.clone(),
                reason: s.reason,
            })
            .collect(),
    }
}

/// A verdict's `kind`, as the stored report and the local MCP's `probe_ai_fetch` spell it.
fn verdict_slug(v: &Verdict) -> &'static str {
    match v {
        Verdict::Same => "same",
        Verdict::Blocked { .. } => "blocked",
        Verdict::Challenged { .. } => "challenged",
        Verdict::Diverted { .. } => "diverted",
        Verdict::Degraded => "degraded",
        Verdict::Throttled => "throttled",
        Verdict::Inconclusive { .. } => "inconclusive",
        Verdict::SkippedRobots => "skipped_robots",
    }
}

fn ai_engines(report: &AccessReport) -> Vec<AiEngineInfo> {
    engines()
        .iter()
        .map(|engine| {
            let (mut limited, mut excluded) = (0u32, 0u32);
            let mut html = 0u32;
            if let Some(access) = report.engine(engine.id) {
                html = u32::from(access.html_pages);
                for g in &access.groups {
                    let n = u32::try_from(g.urls.len()).unwrap_or(u32::MAX);
                    if g.robots_blocked || g.effect == Effect::Excluded {
                        excluded += n;
                    } else if g.effect == Effect::Limited {
                        limited += n;
                    }
                }
            }
            AiEngineInfo {
                id: engine.id,
                name: engine.name.to_owned(),
                eligible: html.saturating_sub(limited + excluded),
                limited,
                excluded,
                page_controls: engine.page_controls,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_are_clamped() {
        assert_eq!(clamp_limit(None), 50);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(10_000)), 200);
    }

    #[test]
    fn severities_are_lowercase_slugs() {
        assert_eq!(parse_severity("critical").unwrap(), Severity::Critical);
        assert_eq!(parse_severity(" notice ").unwrap(), Severity::Notice);
        assert!(parse_severity("Critical").is_err());
        assert!(parse_severity("severe").is_err());
    }

    #[test]
    fn a_bots_urls_probed_counts_only_pages_its_user_agent_was_sent_to() {
        use codoseo_geo::probe::{InconclusiveReason, ProbeFetch, ProbeResult, ProbedUrl};
        let ok = ProbeFetch {
            status: Some(200),
            html: true,
            ..ProbeFetch::default()
        };
        let page = |url: u16, fetch: Option<ProbeFetch>, verdict: Verdict| ProbedUrl {
            url,
            baseline: ok.clone(),
            results: vec![ProbeResult {
                token: "OAI-SearchBot".to_owned(),
                fetch,
                verdict,
            }],
        };
        let probes = ProbeReport {
            version: 1,
            synthetic: true,
            from: "hello@codoseo.com".to_owned(),
            bots: vec!["OAI-SearchBot".to_owned()],
            skipped_bots: Vec::new(),
            urls: vec![
                page(0, Some(ok.clone()), Verdict::Same),
                // CodoSEObot got nothing here, so the bot was never sent.
                page(
                    1,
                    None,
                    Verdict::Inconclusive {
                        reason: InconclusiveReason::BaselineFailed,
                    },
                ),
                page(2, None, Verdict::SkippedRobots),
            ],
            stop: None,
            requests: 3,
        };
        let info = ai_probes(&probes, &Intent::default());
        assert_eq!(info.urls_probed, 3);
        assert_eq!(info.bots[0].urls_probed, 1);
    }

    /// A report of `n` important HTML pages; every third one (from the second) is a shell.
    fn content_report(n: usize) -> AccessReport {
        let important: Vec<serde_json::Value> = (0..n)
            .map(|i| {
                serde_json::json!({
                    "url": format!("https://example.com/p{i}"),
                    "reason": "most_linked",
                    "status": 200,
                    "html_ok": true,
                })
            })
            .collect();
        let pages: Vec<serde_json::Value> = (0..n)
            .map(|i| {
                let shell = i % 3 == 1;
                serde_json::json!({
                    "url": i,
                    "verdict": if shell { "shell" } else { "present" },
                    "signals": if shell { vec!["mount_empty"] } else { Vec::new() },
                    "words": if shell { 0 } else { 300 },
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "registry_version": 1,
            "robots": { "status": 200, "availability": "ok" },
            "important": important,
            "content": { "pages": pages },
        }))
        .expect("a report")
    }

    #[test]
    fn ai_content_lists_shells_first_and_caps_the_pages() {
        let report = content_report(45);
        let info = ai_content(&report).expect("content");
        assert_eq!(info.pages_checked, 45);
        assert_eq!(info.shells, 15);
        assert_eq!(info.pages.len(), MAX_AI_CONTENT_PAGES);
        assert_eq!(info.more, 15);
        // The 15 shells first, in the report's order, then the first present pages.
        assert!(info.pages[..15].iter().all(|p| p.verdict.is_shell()));
        assert_eq!(info.pages[0].url, "https://example.com/p1");
        assert_eq!(info.pages[14].url, "https://example.com/p43");
        assert_eq!(info.pages[15].url, "https://example.com/p0");
        assert!(info.pages[15..].iter().all(|p| !p.verdict.is_shell()));

        let small = ai_content(&content_report(4)).expect("content");
        assert_eq!((small.pages.len(), small.more), (4, 0));
    }

    #[test]
    fn ai_llms_txt_clips_site_text() {
        use codoseo_geo::llms::{LlmsCheck, LlmsCheckId, MAX_TITLE};
        let long = "x".repeat(5000);
        let mut report = codoseo_geo::llms::assess(
            &codoseo_core::output::LlmsTxtFetch {
                status: 200,
                content_type: Some("text/plain".to_owned()),
                body: "# Site\n".to_owned(),
                ..Default::default()
            },
            false,
        );
        // As a report stored before the caps could hold.
        report.title = Some(long.clone());
        report.error = Some(long.clone());
        report.checks.push(LlmsCheck {
            id: LlmsCheckId::H1,
            ok: false,
            detail: long.clone(),
        });
        report.link_summary.failing_urls = vec![long.clone()];
        let info = ai_llms_txt(&report);
        let r = &info.report;
        assert!(info.informational);
        assert_eq!(r.title.as_ref().unwrap().chars().count(), MAX_TITLE);
        assert_eq!(r.error.as_ref().unwrap().chars().count(), MAX_AI_TEXT);
        assert!(
            r.checks
                .iter()
                .all(|c| c.detail.chars().count() <= MAX_AI_TEXT)
        );
        assert_eq!(r.checks.last().unwrap().detail.chars().count(), MAX_AI_TEXT);
        assert!(r.checks.last().unwrap().detail.ends_with('…'));
        assert_eq!(r.link_summary.failing_urls[0].chars().count(), MAX_AI_TEXT);
    }

    #[test]
    fn the_next_page_starts_where_this_one_ends_until_the_total_is_reached() {
        assert_eq!(next_offset(0, 50, 120), Some(50));
        assert_eq!(next_offset(100, 20, 120), None);
        assert_eq!(next_offset(0, 0, 0), None);
        assert_eq!(next_offset(u32::MAX, 5, u32::MAX), None);
    }
}
