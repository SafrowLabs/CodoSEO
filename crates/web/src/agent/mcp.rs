//! The cloud MCP server's backend: [`CloudBackend`] over the shared [`AgentService`], so a keyed
//! tool call returns the JSON the REST API returns and is charged to the same daily allowance,
//! and [`AnonBackend`] over [`AnonService`] for the no-key tools.
//! Errors become the message the agent reads as a tool error (`AgentError::message`, never raw
//! internal text; quota exhaustion reads the same as over REST).

use std::convert::Infallible;

use codoseo_mcp::cloud::types::{
    AuditIssueUrls, ChangesPage, CrawlQueued, IssueUrlsPage, MonitoringRequested, PageInfo,
    QuickAuditState, SiteHealth, SiteInfo,
};
use codoseo_mcp::cloud::{AnonBackend, CloudBackend};

use super::anon::{AnonCaller, AnonService};
use super::auth::ApiCaller;
use super::error::AgentError;
use super::service::{AgentService, Reply};
use crate::state::AppState;

#[derive(Clone)]
pub struct AgentBackend {
    state: AppState,
}

impl AgentBackend {
    pub fn new(state: AppState) -> AgentBackend {
        AgentBackend { state }
    }

    fn service(&self) -> AgentService<'_> {
        AgentService::new(&self.state)
    }
}

/// What the agent is told for a failed call. An internal failure is logged here (REST logs it
/// when it renders the response) and the agent only sees the generic message.
fn tool_error(e: AgentError) -> String {
    if let AgentError::Internal(detail) = &e {
        tracing::error!(%detail, "mcp tool call failed");
    }
    e.message()
}

fn outcome<T>(reply: Reply<T>) -> Result<T, String> {
    reply.into_result().map_err(tool_error)
}

impl CloudBackend for AgentBackend {
    type Keyed = ApiCaller;
    type Anon = AnonCaller;

    async fn list_sites(&self, who: &ApiCaller) -> Result<Vec<SiteInfo>, String> {
        outcome(self.service().list_sites(who).await)
    }

    async fn site_health(&self, who: &ApiCaller, site_id: &str) -> Result<SiteHealth, String> {
        outcome(self.service().site_health(who, site_id).await)
    }

    async fn issue_urls(
        &self,
        who: &ApiCaller,
        site_id: &str,
        check: &str,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<IssueUrlsPage, String> {
        outcome(
            self.service()
                .issue_urls(who, site_id, check, limit, offset)
                .await,
        )
    }

    async fn page(&self, who: &ApiCaller, site_id: &str, url: &str) -> Result<PageInfo, String> {
        outcome(self.service().page(who, site_id, url).await)
    }

    async fn changes(
        &self,
        who: &ApiCaller,
        site_id: &str,
        severity: Option<&str>,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<ChangesPage, String> {
        outcome(
            self.service()
                .changes(who, site_id, severity, limit, offset)
                .await,
        )
    }

    async fn run_crawl(&self, who: &ApiCaller, site_id: &str) -> Result<CrawlQueued, String> {
        outcome(self.service().run_crawl(who, site_id).await)
    }

    async fn reject(&self, who: &ApiCaller, message: String) -> Result<Infallible, String> {
        self.service()
            .refuse::<Infallible>(who, AgentError::BadRequest(message))
            .await
            .into_result()
            .map_err(tool_error)
    }
}

impl AnonBackend for AgentBackend {
    async fn quick_audit(&self, who: &AnonCaller, url: &str) -> Result<QuickAuditState, String> {
        AnonService::new(&self.state).quick_audit(who, url).await
    }

    async fn get_audit(&self, who: &AnonCaller, audit_id: &str) -> Result<QuickAuditState, String> {
        AnonService::new(&self.state).get_audit(who, audit_id).await
    }

    async fn audit_issue_urls(
        &self,
        who: &AnonCaller,
        audit_id: &str,
        check: &str,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<AuditIssueUrls, String> {
        AnonService::new(&self.state)
            .audit_issue_urls(who, audit_id, check, limit, offset)
            .await
    }

    async fn start_monitoring(
        &self,
        who: &AnonCaller,
        url: &str,
        email: &str,
    ) -> Result<MonitoringRequested, String> {
        AnonService::new(&self.state)
            .start_monitoring(who, url, email)
            .await
    }
}
