//! The four no-key tools (cloud only): a free quick audit of any public site, its results, and
//! `start_monitoring`, which turns an audit into monitored site once the user confirms by
//! email. They are what a claude.ai or ChatGPT connector sees, since those connect without an
//! API key. Each is a thin layer over [`AnonBackend`].
//!
//! `quick_audit` does the waiting here, like the local `audit_site`: it starts the audit, looks
//! at it every [`super::handler::DEFAULT_POLL_INTERVAL`] and gives up after
//! [`super::handler::DEFAULT_QUICK_AUDIT_WAIT`] with `{"status":"running","audit_id":...}`, which
//! the agent follows up with `get_audit`. Nothing here is charged to a key; the web crate's
//! backend enforces the abuse limits.

use std::future::Future;

use http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use super::handler::{CloudBackend, CloudMcp, to_json};
use super::types::{AuditIssueUrls, MonitoringRequested, QuickAuditState};

/// What the no-key tools do, behind the MCP surface. `Err` is the message shown to the agent as
/// a tool error; it is never raw internal text. Arguments arrive as the strings the agent sent.
pub trait AnonBackend: CloudBackend {
    /// Starts a quick audit of `url` (or joins the running one, or reuses the finished one from
    /// the last 24 hours) and says where it stands right now.
    fn quick_audit(
        &self,
        who: &Self::Anon,
        url: &str,
    ) -> impl Future<Output = Result<QuickAuditState, String>> + Send;

    /// Where a quick audit stands. Anything that isn't a quick audit is "no such audit".
    fn get_audit(
        &self,
        who: &Self::Anon,
        audit_id: &str,
    ) -> impl Future<Output = Result<QuickAuditState, String>> + Send;

    /// The pages of a finished quick audit that fail one check.
    fn audit_issue_urls(
        &self,
        who: &Self::Anon,
        audit_id: &str,
        check: &str,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> impl Future<Output = Result<AuditIssueUrls, String>> + Send;

    /// Emails `email` a link that, once opened and confirmed, starts monitoring `url`.
    fn start_monitoring(
        &self,
        who: &Self::Anon,
        url: &str,
        email: &str,
    ) -> impl Future<Output = Result<MonitoringRequested, String>> + Send;
}

#[derive(Debug, Deserialize, JsonSchema)]
struct QuickAuditArgs {
    /// The site to audit: a domain like "example.com" or a full address. Public sites only.
    url: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AuditArgs {
    /// The `audit_id` from `quick_audit`.
    audit_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AuditIssueUrlsArgs {
    /// The `audit_id` from `quick_audit`.
    audit_id: String,
    /// A check's slug, e.g. "title_missing". The audit summary lists the failing ones.
    check: String,
    /// Most rows to return (default 50, at most 200).
    limit: Option<u32>,
    /// Rows to skip, for paging (default 0). Use `next_offset` from the previous page.
    offset: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StartMonitoringArgs {
    /// The site to monitor: a domain like "example.com" or a full address. Public sites only.
    url: String,
    /// The email address of the person who owns the site. The confirmation link goes here.
    email: String,
}

#[tool_router(router = anon_router, vis = "pub(super)")]
impl<B: AnonBackend> CloudMcp<B> {
    #[tool(
        description = "Audit a public website for SEO problems, free and with no account: \
            crawls up to 100 pages and runs about 40 checks. Returns the health score (0 to \
            100), checks passed, pages crawled, and up to 15 failing checks (most severe \
            first) each with a count and 3 example URLs, plus report_url, the same report as a \
            web page for the user. A crawl takes from a few seconds to a minute or two: if it \
            is not done after about 45 seconds this returns {\"status\":\"running\",\"audit_id\":\
            ...} and you call get_audit with that id every few seconds. A site audited in the \
            last 24 hours returns that report again instead of a new crawl. Use get_issue_urls \
            for every page behind a failing check.",
        annotations(
            title = "Audit a website",
            read_only_hint = false,
            idempotent_hint = true,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn quick_audit(
        &self,
        Parameters(QuickAuditArgs { url }): Parameters<QuickAuditArgs>,
        Extension(parts): Extension<Parts>,
    ) -> Result<String, String> {
        let who = self.anon(&parts)?;
        let mut state = self.backend.quick_audit(who, &url).await?;
        let deadline = tokio::time::Instant::now() + self.quick_audit_wait;
        while let QuickAuditState::Running { audit_id, .. } = &state {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(self.poll_interval).await;
            // A read that fails mid-wait must not lose the id the agent needs: say "running"
            // and let it ask again with get_audit.
            match self.backend.get_audit(who, &audit_id.to_string()).await {
                Ok(next) => state = next,
                Err(error) => {
                    tracing::warn!(%error, %audit_id, "quick_audit poll failed, answering running");
                    break;
                }
            }
        }
        to_json(&state)
    }

    #[tool(
        description = "Check on an audit started by quick_audit: its progress while it is \
            still running, or its summary once done (or why it could not be audited). Call \
            it every few seconds after quick_audit answered \"running\".",
        annotations(
            title = "Audit status",
            read_only_hint = true,
            idempotent_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn get_audit(
        &self,
        Parameters(AuditArgs { audit_id }): Parameters<AuditArgs>,
        Extension(parts): Extension<Parts>,
    ) -> Result<String, String> {
        let who = self.anon(&parts)?;
        to_json(&self.backend.get_audit(who, &audit_id).await?)
    }

    #[tool(
        description = "The pages that fail one check in a finished quick audit, paginated: \
            URL, status, title and indexability of each, the total, and next_offset for the \
            next page. Use a check slug such as \"title_missing\" from the audit summary; an \
            unknown slug's error lists all of them. Only audits made with quick_audit.",
        name = "get_issue_urls",
        annotations(
            title = "Pages failing a check",
            read_only_hint = true,
            idempotent_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn get_audit_issue_urls(
        &self,
        Parameters(AuditIssueUrlsArgs {
            audit_id,
            check,
            limit,
            offset,
        }): Parameters<AuditIssueUrlsArgs>,
        Extension(parts): Extension<Parts>,
    ) -> Result<String, String> {
        let who = self.anon(&parts)?;
        to_json(
            &self
                .backend
                .audit_issue_urls(who, &audit_id, &check, limit, offset)
                .await?,
        )
    }

    #[tool(
        description = "Start free weekly monitoring of a site for its owner. Sends the owner's \
            email address a confirmation link; when they open it and press the button, \
            CodoSEO creates their free account, crawls the site (up to 500 pages) now and \
            every week, emails them when something important breaks, and shows them an API key \
            once to connect you with their own data. Nothing starts until they confirm. Ask \
            the user for their email address first and never invent one; use the address of \
            the person who owns the site.",
        annotations(
            title = "Start monitoring a site",
            read_only_hint = false,
            idempotent_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn start_monitoring(
        &self,
        Parameters(StartMonitoringArgs { url, email }): Parameters<StartMonitoringArgs>,
        Extension(parts): Extension<Parts>,
    ) -> Result<String, String> {
        let who = self.anon(&parts)?;
        to_json(&self.backend.start_monitoring(who, &url, &email).await?)
    }
}
