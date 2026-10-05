//! The cloud MCP server's backend: [`CloudBackend`] over the shared [`AgentService`], so a keyed
//! tool call returns the JSON the REST API returns and is charged to the same daily allowance.
//! Errors become the message the agent reads as a tool error (`AgentError::message`, never raw
//! internal text; quota exhaustion reads the same as over REST).

use codoseo_mcp::cloud::CloudBackend;
use codoseo_mcp::cloud::types::{
    ChangesPage, CrawlQueued, IssueUrlsPage, PageInfo, SiteHealth, SiteInfo,
};

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
    /// The no-key tier has nothing to know about its callers yet.
    type Anon = ();

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

    async fn reject(&self, who: &ApiCaller, message: String) -> String {
        match self
            .service()
            .refuse(who, AgentError::BadRequest(message))
            .await
            .into_result()
        {
            Err(e) => tool_error(e),
            // `refuse` always answers with its error.
            Ok(()) => String::new(),
        }
    }
}
