//! The cloud MCP server: one handler type, [`CloudMcp`], that serves `/mcp` over streamable HTTP
//! to two kinds of caller and gives each its own tool set.
//!
//! * A caller holding an API key gets the keyed tools ([`crate::cloud::keyed`]): the user's own
//!   monitored sites, charged to the key's daily allowance.
//! * A caller without a key (cloud only) gets the no-key tools, which a later task fills in
//!   (see [`CloudMcp::anon_router`]).
//!
//! The web crate decides who is calling, once per HTTP request, and puts a [`Caller`] in the
//! request's extensions; rmcp copies the request's `http::request::Parts` into every tool
//! context, so the handler reads the caller from there. A tool of the other tier is simply
//! "tool not found", and `initialize`, `tools/list` and pings never touch the backend (free).

use std::convert::Infallible;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures_util::FutureExt;
use http::request::Parts;
use rmcp::ErrorData as McpError;
use rmcp::RoleServer;
use rmcp::handler::server::ServerHandler;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
    Implementation, ListToolsResult, PaginatedRequestParams, ResultType, ServerCapabilities,
    ServerConfig,
};
use rmcp::service::RequestContext;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::types::{ChangesPage, CrawlQueued, IssueUrlsPage, PageInfo, SiteHealth, SiteInfo};

/// Who is calling, as the HTTP layer resolved it for one request. The web crate inserts one into
/// the request extensions before the MCP service sees the request.
#[derive(Debug, Clone)]
pub enum Caller<K, A = ()> {
    /// A live API key: what the keyed tools act as.
    Keyed(K),
    /// No key (cloud only; self-hosted refuses such a request before it gets here).
    Anon(A),
}

/// What the keyed tools do, behind the MCP surface. The web crate implements it over the shared
/// agent service, so a tool returns the same JSON as the REST API and counts against the same
/// daily allowance.
///
/// Every method is one tool call: it charges the call (the first thing it does), then looks at
/// its arguments, which arrive as the strings the agent sent. `Err` is the message shown to the
/// agent as a tool error; it is never raw internal text.
pub trait CloudBackend: Send + Sync + 'static {
    /// The authenticated holder of an API key.
    type Keyed: Clone + Send + Sync + 'static;
    /// What is known about a caller without a key (no-key tools only).
    type Anon: Clone + Send + Sync + 'static;

    fn list_sites(
        &self,
        who: &Self::Keyed,
    ) -> impl Future<Output = Result<Vec<SiteInfo>, String>> + Send;

    fn site_health(
        &self,
        who: &Self::Keyed,
        site_id: &str,
    ) -> impl Future<Output = Result<SiteHealth, String>> + Send;

    fn issue_urls(
        &self,
        who: &Self::Keyed,
        site_id: &str,
        check: &str,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> impl Future<Output = Result<IssueUrlsPage, String>> + Send;

    fn page(
        &self,
        who: &Self::Keyed,
        site_id: &str,
        url: &str,
    ) -> impl Future<Output = Result<PageInfo, String>> + Send;

    fn changes(
        &self,
        who: &Self::Keyed,
        site_id: &str,
        severity: Option<&str>,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> impl Future<Output = Result<ChangesPage, String>> + Send;

    fn run_crawl(
        &self,
        who: &Self::Keyed,
        site_id: &str,
    ) -> impl Future<Output = Result<CrawlQueued, String>> + Send;

    /// Counts one call for a tool call whose arguments were unusable (not even the right JSON
    /// shape), like any other call, and gives back what to tell the agent: `message`, or the
    /// quota message when the allowance is spent. It always answers with an error, which the
    /// type says: there is no `Ok` value to be empty.
    fn reject(
        &self,
        who: &Self::Keyed,
        message: String,
    ) -> impl Future<Output = Result<Infallible, String>> + Send;
}

/// The longest one tool call may take before the agent is told so. Above the 45 s the no-key
/// `quick_audit` waits for a fresh audit.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(75);

/// The cloud MCP handler. One per HTTP request is cheap: the tool routers are built once and
/// cloned.
#[derive(Clone)]
pub struct CloudMcp<B: CloudBackend> {
    pub(super) backend: B,
    keyed_tools: ToolRouter<Self>,
    anon_tools: ToolRouter<Self>,
    call_timeout: Duration,
}

impl<B: CloudBackend> CloudMcp<B> {
    pub fn new(backend: B) -> CloudMcp<B> {
        CloudMcp {
            backend,
            keyed_tools: Self::keyed_router(),
            anon_tools: Self::anon_router(),
            call_timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    /// For tests: a shorter limit than [`DEFAULT_CALL_TIMEOUT`] on one tool call.
    pub fn with_call_timeout(mut self, timeout: Duration) -> CloudMcp<B> {
        self.call_timeout = timeout;
        self
    }

    /// The no-key tools: `quick_audit`, `get_audit`, `get_issue_urls` and `start_monitoring`.
    /// Empty until the no-key tier lands; build it here like [`Self::keyed_router`] (a
    /// `#[tool_router(router = anon_router)]` impl in its own file) and drop this stub.
    fn anon_router() -> ToolRouter<Self> {
        ToolRouter::new()
    }

    /// The caller of this request, as the web layer resolved it.
    fn caller<'c>(
        &self,
        context: &'c RequestContext<RoleServer>,
    ) -> Result<&'c Caller<B::Keyed, B::Anon>, McpError> {
        context
            .extensions
            .get::<Parts>()
            .and_then(|parts| parts.extensions.get::<Caller<B::Keyed, B::Anon>>())
            .ok_or_else(|| McpError::internal_error("the request has no resolved caller", None))
    }

    /// The key holder behind a keyed tool call.
    pub(super) fn keyed<'p>(&self, parts: &'p Parts) -> Result<&'p B::Keyed, String> {
        match parts.extensions.get::<Caller<B::Keyed, B::Anon>>() {
            Some(Caller::Keyed(who)) => Ok(who),
            _ => Err("This tool needs an API key.".to_owned()),
        }
    }

    /// The arguments of a keyed tool call as `T`. Arguments that don't fit still cost the call
    /// (as a bad request does over REST) and answer with a tool error.
    pub(super) async fn args<T: DeserializeOwned>(
        &self,
        who: &B::Keyed,
        arguments: rmcp::model::JsonObject,
    ) -> Result<T, String> {
        match serde_json::from_value(serde_json::Value::Object(arguments)) {
            Ok(args) => Ok(args),
            Err(e) => {
                let Err(message) = self
                    .backend
                    .reject(who, format!("The arguments are not valid: {e}."))
                    .await;
                Err(message)
            }
        }
    }
}

/// A tool's JSON result: the REST API's body for the same call.
pub(super) fn to_json<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value)
        .map_err(|_| "Something went wrong on our side. Try again in a moment.".to_owned())
}

impl<B: CloudBackend> ServerHandler for CloudMcp<B> {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("codoseo", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "CodoSEO monitors websites and checks them for SEO problems. With an API key \
                 (header \"Authorization: Bearer <key>\", created under Settings > API keys) \
                 these tools read your monitored sites: list_sites, then get_site_health, \
                 get_issue_urls, get_page, get_changes and run_crawl. Each tool call counts \
                 against your daily API allowance. When connected without a key, the tools are \
                 instead a free quick audit of any public site (quick_audit, get_audit, \
                 get_issue_urls) and start_monitoring. The same data is available as a REST API \
                 under /api/v1 with the same key.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let router = match self.caller(&context)? {
            Caller::Keyed(_) => &self.keyed_tools,
            Caller::Anon(_) => &self.anon_tools,
        };
        Ok(ListToolsResult {
            result_type: Some(ResultType::COMPLETE),
            tools: router.list_all(),
            meta: None,
            next_cursor: None,
            // The list depends on who asks, so no shared cache may keep it.
            ttl_ms: context
                .protocol_version()
                .is_some_and(|v| v >= rmcp::model::ProtocolVersion::V_2026_07_28)
                .then_some(0),
            cache_scope: context
                .protocol_version()
                .is_some_and(|v| v >= rmcp::model::ProtocolVersion::V_2026_07_28)
                .then_some(CacheScope::Private),
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let router = match self.caller(&context)? {
            Caller::Keyed(_) => &self.keyed_tools,
            Caller::Anon(_) => &self.anon_tools,
        };
        let call = router.call(ToolCallContext::new(self, request, context));
        // rmcp runs the handler in a task of its own: a panic would send no response and leave
        // the HTTP request waiting, and so would a stalled backend.
        match tokio::time::timeout(self.call_timeout, AssertUnwindSafe(call).catch_unwind()).await {
            Ok(Ok(result)) => result,
            Ok(Err(panic)) => {
                let detail = panic
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                tracing::error!(%detail, "mcp tool call panicked");
                Err(McpError::internal_error(
                    "Something went wrong on our side. Try again in a moment.",
                    None,
                ))
            }
            Err(_) => Ok(CallToolResult::error(vec![ContentBlock::text(
                "That call took too long and was stopped. Try again in a moment.",
            )])
            .into()),
        }
    }
}
