//! The cloud MCP server's backend: [`CloudBackend`] over the shared [`AgentService`], so a keyed
//! tool call returns the JSON the REST API returns and is charged to the same daily allowance,
//! and [`AnonBackend`] over [`AnonService`] for the no-key tools.
//! Errors become the message the agent reads as a tool error (`AgentError::message`, never raw
//! internal text; quota exhaustion reads the same as over REST).

use std::convert::Infallible;

use codoseo_mcp::cloud::types::{
    AiAccessInfo, AuditIssueUrls, ChangesPage, CrawlQueued, IssueUrlsPage, MonitoringRequested,
    PageInfo, QuickAuditState, SiteHealth, SiteInfo,
};
use codoseo_mcp::cloud::{AnonBackend, CloudBackend};

use super::anon::{AnonCaller, AnonService};
use super::auth::ApiCaller;
use super::error::AgentError;
use super::service::{AgentService, Reply};
use crate::metrics::{self, Surface, Tier};
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
    let result = reply.into_result();
    metrics::api_request(
        Surface::Mcp,
        Tier::Key,
        result.as_ref().map_or_else(AgentError::code, |_| "ok"),
    );
    result.map_err(tool_error)
}

/// A no-key tool call's outcome, counted. Those errors are plain messages without a code.
fn anon<T>(result: Result<T, String>) -> Result<T, String> {
    let label = if result.is_ok() { "ok" } else { "error" };
    metrics::api_request(Surface::Mcp, Tier::Anon, label);
    result
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

    async fn ai_access(&self, who: &ApiCaller, site_id: &str) -> Result<AiAccessInfo, String> {
        outcome(self.service().ai_access(who, site_id).await)
    }

    async fn run_crawl(&self, who: &ApiCaller, site_id: &str) -> Result<CrawlQueued, String> {
        outcome(self.service().run_crawl(who, site_id).await)
    }

    async fn reject(&self, who: &ApiCaller, message: String) -> Result<Infallible, String> {
        let refused = self
            .service()
            .refuse::<Infallible>(who, AgentError::BadRequest(message))
            .await;
        outcome(refused)
    }
}

impl AnonBackend for AgentBackend {
    async fn quick_audit(&self, who: &AnonCaller, url: &str) -> Result<QuickAuditState, String> {
        anon(AnonService::new(&self.state).quick_audit(who, url).await)
    }

    async fn get_audit(&self, who: &AnonCaller, audit_id: &str) -> Result<QuickAuditState, String> {
        anon(AnonService::new(&self.state).get_audit(who, audit_id).await)
    }

    /// `quick_audit` looks at its audit every second or so while it waits; those looks are part
    /// of the one tool call the client's allowance already counted.
    async fn poll_audit(&self, _: &AnonCaller, audit_id: &str) -> Result<QuickAuditState, String> {
        AnonService::new(&self.state).poll_audit(audit_id).await
    }

    async fn audit_issue_urls(
        &self,
        who: &AnonCaller,
        audit_id: &str,
        check: &str,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<AuditIssueUrls, String> {
        anon(
            AnonService::new(&self.state)
                .audit_issue_urls(who, audit_id, check, limit, offset)
                .await,
        )
    }

    async fn start_monitoring(
        &self,
        who: &AnonCaller,
        url: &str,
        email: &str,
    ) -> Result<MonitoringRequested, String> {
        anon(
            AnonService::new(&self.state)
                .start_monitoring(who, url, email)
                .await,
        )
    }
}
