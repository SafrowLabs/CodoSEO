//! The no-key tier of the cloud MCP server: what [`AnonBackend`](codoseo_mcp::cloud::AnonBackend)
//! does over the same store the website's no-signup audit uses. Nothing here is charged to an
//! API key; abuse is held back by the limits of `store::quick`: one fresh audit per domain per
//! 24 hours, a daily budget over all agents, the per-IP limits (shared with the website, for
//! clients that connect directly) and the start-monitoring email caps.
//!
//! Errors are `String`s, the message an agent reads as a tool error. Database trouble is logged
//! and read as the generic message.

use std::net::IpAddr;

use axum::http::{HeaderMap, header};
use codoseo_checks::def;
use codoseo_mcp::cloud::types::{
    AuditIssueUrls, MonitoringRequested, QuickAuditState, QuickAuditSummary,
};
use codoseo_store::crawls::CrawlStatus;
use codoseo_store::events::{self, EventKind};
use codoseo_store::quick::{
    self, LimitWindow, Limits, MonitoringCaps, MonitoringSlot, Requester, Source, StartOutcome,
    StartRequest,
};
use serde_json::json;
use uuid::Uuid;

use super::error::AgentError;
use super::service::{crawl_health, issue_page, parse_check};
use crate::abuse;
use crate::auth::mailer::Email;
use crate::auth::{email, session};
use crate::routes::quick::no_report_notice;
use crate::routes::sites::{check_public_target, parse_start_url};
use crate::state::AppState;

/// How long a start-monitoring link works. Longer than a sign-in link: the person has to see
/// the email, which an agent's user may do later.
pub const START_TTL: time::Duration = time::Duration::hours(24);

const GENERIC: &str = "Something went wrong on our side. Try again in a moment.";
const NO_SUCH_AUDIT: &str =
    "No such audit. Use the audit_id that quick_audit returned (it is the id of a free audit).";

/// What is known about a caller without a key, from the HTTP request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnonCaller {
    /// The client's address, when it can be told (see [`abuse::client_ip`]).
    pub ip: Option<IpAddr>,
    /// A hosted connector (claude.ai, ChatGPT) calling from servers shared by many people, by
    /// its `User-Agent`. It skips the per-IP limits, which would lock everyone out at once.
    pub shared_client: bool,
}

impl AnonCaller {
    pub fn from_request(state: &AppState, headers: &HeaderMap, peer: Option<IpAddr>) -> AnonCaller {
        let user_agent = headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok());
        AnonCaller {
            ip: abuse::client_ip(headers, peer, &state.config),
            shared_client: state.config.mcp.is_shared_client(user_agent),
        }
    }

    /// The hashes the per-IP limits count by (today's and yesterday's salt). None for a shared
    /// client and for a request whose address can't be told: nothing per IP applies then.
    fn ip_hashes(&self, state: &AppState) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
        if self.shared_client {
            return None;
        }
        let ip = self.ip?;
        let today = time::OffsetDateTime::now_utc().date();
        let hash = |day: time::Date| abuse::ip_hash(&state.config.secret_key, ip, day);
        Some((hash(today), today.previous_day().map(hash)))
    }
}

pub struct AnonService<'a> {
    state: &'a AppState,
}

fn internal(e: impl std::fmt::Display) -> String {
    tracing::error!(error = %e, "no-key mcp tool failed");
    GENERIC.to_owned()
}

fn db(e: sqlx::Error) -> String {
    if crate::error::is_unavailable(&e) {
        AgentError::Unavailable.message()
    } else {
        internal(e)
    }
}

/// A site address as the agent typed it, or why it can't be used.
fn public_target(raw: &str) -> Result<url::Url, String> {
    parse_start_url(raw).and_then(|u| check_public_target(&u).map(|()| u))
}

impl<'a> AnonService<'a> {
    pub fn new(state: &'a AppState) -> AnonService<'a> {
        AnonService { state }
    }

    /// Counts one call of a direct client against its per-minute allowance (shared connectors
    /// and callers whose address can't be told are not limited here).
    fn throttle(&self, who: &AnonCaller) -> Result<(), String> {
        let Some((hash, _)) = who.ip_hashes(self.state) else {
            return Ok(());
        };
        self.state
            .anon_calls
            .check(&hash, std::time::Instant::now())
            .map_err(|wait| {
                format!(
                    "Too many calls from this client: the limit is {} a minute. Wait {} seconds \
                     and try again.",
                    super::limiter::ANON_CALLS_PER_WINDOW,
                    wait.as_secs() + 1
                )
            })
    }

    /// `{BASE_URL}{path}`.
    fn link(&self, path: &str) -> String {
        let mut link = self.state.config.base_url.clone();
        link.set_path(path);
        link.to_string()
    }

    // ---- quick_audit, get_audit, get_issue_urls ----

    pub async fn quick_audit(
        &self,
        who: &AnonCaller,
        raw_url: &str,
    ) -> Result<QuickAuditState, String> {
        self.throttle(who)?;
        let state = self.state;
        let url = public_target(raw_url)?;
        let domain = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let hashes = who.ip_hashes(state);
        // Nobody holds this claim token: an agent's audit can't be attached by cookie, only
        // read by its id.
        let claim_token = session::random_token();
        let outcome = quick::start(
            &state.pool,
            &StartRequest {
                domain: &domain,
                start_url: url.as_str(),
                claim_hash: &session::hash(&claim_token),
                ip_hash: hashes.as_ref().map(|(today, _)| today.as_slice()),
                previous_ip_hash: hashes.as_ref().and_then(|(_, prev)| prev.as_deref()),
                limits: Limits::DEFAULT,
                source: Source::Agent,
                agent_daily_budget: Some(state.config.mcp.daily_audits),
            },
        )
        .await
        .map_err(db)?;
        let outcome_is_fresh = matches!(outcome, StartOutcome::Started { .. });
        let (crawl_id, how) = match outcome {
            StartOutcome::Started { crawl_id } => (crawl_id, "started"),
            StartOutcome::Cached { crawl_id } => (crawl_id, "cached"),
            StartOutcome::Joined { crawl_id } => (crawl_id, "joined"),
            StartOutcome::AgentBudgetReached { retry_after_secs } => {
                return Err(format!(
                    "CodoSEO's free audits for AI assistants are used up for now. Try again in \
                     {}, or ask the user to run the audit at {} themselves.",
                    abuse::wait_text(retry_after_secs),
                    self.link("/"),
                ));
            }
            StartOutcome::Limited {
                window,
                retry_after_secs,
            } => {
                let (limit, per) = match window {
                    LimitWindow::Hour => (Limits::DEFAULT.per_hour, "an hour"),
                    LimitWindow::Day => (Limits::DEFAULT.per_day, "a day"),
                };
                return Err(format!(
                    "The limit is {limit} new audits {per} for each client address, and this \
                     one has used them. Try again in {}. Audits of sites audited in the last \
                     24 hours are not counted.",
                    abuse::wait_text(retry_after_secs)
                ));
            }
        };
        // Only a fresh audit is a funnel step: repeats of cached and joined ones are free to
        // ask for, and must not each leave a row in a table nothing prunes. The audit exists
        // either way, so a failure to note it is logged and the agent still gets its id.
        if outcome_is_fresh
            && let Err(error) = events::record(
                &state.pool,
                EventKind::AuditStarted,
                None,
                None,
                Some(json!({
                    "crawl_id": crawl_id, "domain": domain, "outcome": how, "source": "agent",
                })),
            )
            .await
        {
            tracing::error!(%error, %crawl_id, "could not record the audit_started event");
        }
        self.state_of(crawl_id).await
    }

    pub async fn get_audit(
        &self,
        who: &AnonCaller,
        audit_id: &str,
    ) -> Result<QuickAuditState, String> {
        self.throttle(who)?;
        let id = Uuid::parse_str(audit_id.trim()).map_err(|_| NO_SUCH_AUDIT.to_owned())?;
        self.state_of(id).await
    }

    /// `get_audit` without the per-client throttle, for `quick_audit`'s wait loop.
    pub async fn poll_audit(&self, audit_id: &str) -> Result<QuickAuditState, String> {
        let id = Uuid::parse_str(audit_id.trim()).map_err(|_| NO_SUCH_AUDIT.to_owned())?;
        self.state_of(id).await
    }

    /// Where the audit stands. A queued or running one is a single small read; the score, the
    /// failing checks and their example URLs are only worked out once it has ended.
    async fn state_of(&self, id: Uuid) -> Result<QuickAuditState, String> {
        let (status, pages_done) = quick::status(&self.state.pool, id)
            .await
            .map_err(db)?
            .ok_or_else(|| NO_SUCH_AUDIT.to_owned())?;
        if matches!(status, CrawlStatus::Queued | CrawlStatus::Running) {
            let line = match status {
                CrawlStatus::Queued => "Waiting for a crawler. ",
                _ => "The crawl is running. ",
            };
            return Ok(QuickAuditState::Running {
                audit_id: id,
                pages_done,
                message: format!("{line}Call get_audit with this audit_id again in a few seconds."),
            });
        }
        let audit = self.audit(&id.to_string()).await?;
        let crawl = &audit.crawl;
        if let Some(notice) = no_report_notice(crawl) {
            return Ok(QuickAuditState::Failed {
                audit_id: id,
                reason: format!("{}. {}", notice.title, notice.message),
            });
        }
        let health = crawl_health(&self.state.pool, crawl)
            .await
            .map_err(|e| match e {
                AgentError::Unavailable => e.message(),
                other => internal(other),
            })?;
        Ok(QuickAuditState::Done(Box::new(
            QuickAuditSummary {
                audit_id: id,
                domain: audit.domain.clone(),
                start_url: audit.start_url.clone(),
                health_score: health.health_score,
                checks_passed: health.checks_passed,
                checks_total: health.checks_total,
                pages_crawled: health.pages_crawled,
                stop_reason: health.stop_reason,
                stop_code: health.stop_code,
                failing_checks: health.failing_checks.into_iter().map(Into::into).collect(),
                more_failing_checks: health.more_failing_checks,
                report_url: self.link(&format!("/audit/{id}")),
                note: format!(
                    "To monitor {} every week and get an email when something breaks, call \
                     start_monitoring with the site's URL and the owner's email address.",
                    audit.domain
                ),
            }
            .fit(),
        )))
    }

    pub async fn audit_issue_urls(
        &self,
        who: &AnonCaller,
        audit_id: &str,
        check: &str,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<AuditIssueUrls, String> {
        self.throttle(who)?;
        let audit = self.audit(audit_id).await?;
        let check = parse_check(check).map_err(|e| e.message())?;
        if audit.crawl.status != CrawlStatus::Done || no_report_notice(&audit.crawl).is_some() {
            return Err(match audit.crawl.status {
                CrawlStatus::Queued | CrawlStatus::Running => {
                    "That audit has not finished yet. Call get_audit until it is done.".to_owned()
                }
                _ => "That audit ended without a report, so there are no pages to list.".to_owned(),
            });
        }
        let rows = issue_page(&self.state.pool, audit.crawl.id, check, limit, offset)
            .await
            .map_err(|e| match e {
                AgentError::Unavailable => e.message(),
                other => internal(other),
            })?;
        Ok(AuditIssueUrls {
            audit_id: audit.crawl.id,
            check,
            title: def(check).title.to_owned(),
            total: rows.total,
            limit: rows.limit,
            offset: rows.offset,
            urls: rows.urls,
            next_offset: rows.next_offset,
        })
    }

    /// The quick audit with this id. Every id that isn't one (unknown, malformed, another
    /// kind of crawl) reads the same.
    async fn audit(&self, audit_id: &str) -> Result<quick::Audit, String> {
        let id = Uuid::parse_str(audit_id.trim()).map_err(|_| NO_SUCH_AUDIT.to_owned())?;
        quick::get(&self.state.pool, id)
            .await
            .map_err(db)?
            .ok_or_else(|| NO_SUCH_AUDIT.to_owned())
    }

    // ---- start_monitoring ----

    pub async fn start_monitoring(
        &self,
        who: &AnonCaller,
        raw_url: &str,
        raw_email: &str,
    ) -> Result<MonitoringRequested, String> {
        self.throttle(who)?;
        let state = self.state;
        let url = public_target(raw_url)?;
        let address = email::parse(raw_email)
            .ok_or_else(|| "That doesn't look like an email address.".to_owned())?
            .to_owned();
        if abuse::is_disposable(&address) {
            return Err(
                "Please use a permanent email address. Throwaway inboxes can't keep \
                        a site's alerts."
                    .to_owned(),
            );
        }
        let domain = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let canonical = email::canonical(&address);
        let token = session::random_token();
        let hashes = who.ip_hashes(state);
        let slot = quick::create_monitoring_token(
            &state.pool,
            &canonical,
            hashes.as_ref().map(|(today, prev)| Requester {
                ip_hash: today,
                previous_ip_hash: prev.as_deref(),
            }),
            &session::hash(&token),
            json!({
                "email": address,
                "canonical": canonical,
                "start_url": url.as_str(),
                "domain": domain,
                "source": "agent",
            }),
            START_TTL,
            MonitoringCaps::with_daily(state.config.mcp.daily_emails),
        )
        .await
        .map_err(db)?;
        match slot {
            MonitoringSlot::Created => {}
            MonitoringSlot::AddressDayCapReached => {
                return Err(
                    "We already sent that address its emails for today. Ask the user \
                            to check their inbox and spam folder, or try again tomorrow."
                        .to_owned(),
                );
            }
            MonitoringSlot::HourlyCapReached => {
                return Err(
                    "CodoSEO has sent as many monitoring emails as it allows for AI \
                            assistants this hour. Try again in an hour."
                        .to_owned(),
                );
            }
            MonitoringSlot::AddressCapReached => {
                return Err(
                    "We already sent several emails to that address in the last hour. \
                            Ask the user to check their inbox and spam folder, or try again in \
                            an hour."
                        .to_owned(),
                );
            }
            MonitoringSlot::IpCapReached => {
                return Err(
                    "This client has asked for several monitoring emails in the last \
                            hour, which is the limit. Try again in an hour."
                        .to_owned(),
                );
            }
            MonitoringSlot::DailyCapReached => {
                return Err(
                    "CodoSEO has sent all the monitoring emails it allows for AI \
                            assistants today. Try again tomorrow, or ask the user to add the \
                            site at the website."
                        .to_owned(),
                );
            }
        }

        let link = self.link(&format!("/monitoring/start/{token}"));
        let text = format!(
            "An AI assistant you are working with asked CodoSEO to monitor {domain}.\n\n\
             To confirm, open this link and press the button:\n\n{link}\n\n\
             CodoSEO then creates your free account, crawls {domain} now and every week, and \
             emails you when something important breaks. It also shows you an API key once, \
             which lets your assistant read your site's results.\n\n\
             The link works once and expires in 24 hours. If you didn't ask for this, ignore \
             this email and nothing happens."
        );
        if let Err(e) = state
            .mailer
            .send(Email {
                to: address.clone(),
                subject: format!("Confirm monitoring for {domain}"),
                text,
                html: None,
            })
            .await
        {
            // The answer is the same either way; a broken mail server is for the operator.
            tracing::error!(error = %e, "could not send the start-monitoring link");
        }
        events::record(
            &state.pool,
            EventKind::EmailGiven,
            None,
            None,
            Some(json!({ "source": "agent", "domain": domain })),
        )
        .await
        .map_err(db)?;
        Ok(MonitoringRequested {
            status: "confirmation_sent".to_owned(),
            domain: domain.clone(),
            message: format!(
                "We emailed a confirmation link to {address}. Tell the user to open it and \
                 press the button within 24 hours. Then CodoSEO crawls {domain} every week, \
                 emails them when something important breaks, and shows them an API key to \
                 connect you to their own data. Nothing starts until they confirm."
            ),
        })
    }
}
