//! The 8 local MCP tools, as a thin layer over [`Backend`]. `audit_site` waits up to
//! `wait_timeout` (50 s in production, shorter in tests) before reporting "still
//! running" with an `audit_id` the caller polls with `get_audit`.

use std::time::Duration;

use rmcp::handler::server::ServerHandler;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use url::Url;

use crate::backend::Backend;
use crate::types::{AuditId, AuditStatus};

/// How often `audit_site` re-checks the backend while waiting for a fast crawl to finish.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One line, at most, promoting the hosted product. Spec section 9: "one promotional
/// line at most."
const PROMO_LINE: &str = "Full monitoring, alerts and a REST API for this site are free to start at https://codoseo.com.";

pub struct CodoseoMcp<B: Backend> {
    backend: B,
    wait_timeout: Duration,
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AuditSiteRequest {
    /// The address to crawl.
    url: String,
    /// Most pages to crawl (default 500).
    max_pages: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AuditIdRequest {
    /// An id returned by `audit_site`.
    audit_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IssueUrlsRequest {
    audit_id: String,
    /// A check's slug, e.g. "title_missing".
    check: String,
    /// Most rows to return (default 50).
    limit: Option<u32>,
    /// Rows to skip, for paging (default 0).
    offset: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetPageRequest {
    audit_id: String,
    url: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct UrlRequest {
    url: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CheckRobotsRequest {
    url: String,
    /// The path to test (default: the URL's own path).
    path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CompareAuditsRequest {
    audit_a: String,
    audit_b: String,
}

fn parse_url(s: &str) -> Result<Url, String> {
    Url::parse(s).map_err(|e| format!("invalid URL \"{s}\": {e}"))
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("could not serialise the result: {e}"))
}

#[tool_router(router = tool_router)]
impl<B: Backend + 'static> CodoseoMcp<B> {
    pub fn new(backend: B) -> CodoseoMcp<B> {
        CodoseoMcp::with_wait_timeout(backend, Duration::from_secs(50))
    }

    /// For tests: a short cutover instead of the production 50 s.
    pub fn with_wait_timeout(backend: B, wait_timeout: Duration) -> CodoseoMcp<B> {
        CodoseoMcp {
            backend,
            wait_timeout,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Crawl a site and run the ~40 SEO checks. Returns the summary \
            (score, failing checks, example URLs) if the crawl finishes while waiting; \
            otherwise returns {\"status\":\"running\",\"audit_id\":...} and the caller \
            should poll get_audit with that id."
    )]
    async fn audit_site(
        &self,
        Parameters(AuditSiteRequest { url, max_pages }): Parameters<AuditSiteRequest>,
    ) -> Result<String, String> {
        let url = parse_url(&url)?;
        let handle = self
            .backend
            .audit(url, max_pages.unwrap_or(500))
            .await
            .map_err(|e| e.to_string())?;
        let deadline = tokio::time::Instant::now() + self.wait_timeout;
        loop {
            let state = self
                .backend
                .get_audit(&handle.id)
                .await
                .map_err(|e| e.to_string())?;
            match state.status {
                AuditStatus::Done => {
                    let summary = state.summary.expect("a Done audit always has a summary");
                    return to_json(&summary);
                }
                AuditStatus::Failed(why) => return Err(why),
                AuditStatus::Running => {
                    if tokio::time::Instant::now() >= deadline {
                        return Ok(json!({
                            "status": "running",
                            "audit_id": handle.id.0,
                        })
                        .to_string());
                    }
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
        }
    }

    #[tool(
        description = "Check on an audit started by audit_site: its progress if still \
            running, or its summary once done."
    )]
    async fn get_audit(
        &self,
        Parameters(AuditIdRequest { audit_id }): Parameters<AuditIdRequest>,
    ) -> Result<String, String> {
        let state = self
            .backend
            .get_audit(&AuditId(audit_id))
            .await
            .map_err(|e| e.to_string())?;
        to_json(&state)
    }

    #[tool(
        description = "List the URLs affected by one failing check from a finished audit, paginated."
    )]
    async fn get_issue_urls(
        &self,
        Parameters(IssueUrlsRequest {
            audit_id,
            check,
            limit,
            offset,
        }): Parameters<IssueUrlsRequest>,
    ) -> Result<String, String> {
        let check_id = codoseo_core::check::CheckId::from_slug(&check)
            .ok_or_else(|| format!("unknown check \"{check}\""))?;
        let rows = self
            .backend
            .issue_urls(
                &AuditId(audit_id),
                check_id,
                limit.unwrap_or(50),
                offset.unwrap_or(0),
            )
            .await
            .map_err(|e| e.to_string())?;
        to_json(&rows)
    }

    #[tool(description = "Get one page's full record from a finished audit.")]
    async fn get_page(
        &self,
        Parameters(GetPageRequest { audit_id, url }): Parameters<GetPageRequest>,
    ) -> Result<String, String> {
        let url = parse_url(&url)?;
        let page = self
            .backend
            .page(&AuditId(audit_id), &url)
            .await
            .map_err(|e| e.to_string())?;
        to_json(&page)
    }

    #[tool(
        description = "Fetch and check one page right now, without a full crawl: fields, redirect chain and page issues."
    )]
    async fn check_page(
        &self,
        Parameters(UrlRequest { url }): Parameters<UrlRequest>,
    ) -> Result<String, String> {
        let url = parse_url(&url)?;
        let page = self
            .backend
            .check_page(url)
            .await
            .map_err(|e| e.to_string())?;
        to_json(&page)
    }

    #[tool(description = "Show a site's robots.txt and whether CodoSEObot may fetch a path.")]
    async fn check_robots(
        &self,
        Parameters(CheckRobotsRequest { url, path }): Parameters<CheckRobotsRequest>,
    ) -> Result<String, String> {
        let url = parse_url(&url)?;
        let report = self
            .backend
            .check_robots(url, path)
            .await
            .map_err(|e| e.to_string())?;
        to_json(&report)
    }

    #[tool(description = "Follow a URL's redirects and show each hop.")]
    async fn check_redirects(
        &self,
        Parameters(UrlRequest { url }): Parameters<UrlRequest>,
    ) -> Result<String, String> {
        let url = parse_url(&url)?;
        let report = self
            .backend
            .check_redirects(url)
            .await
            .map_err(|e| e.to_string())?;
        to_json(&report)
    }

    #[tool(description = "Compare two finished audits of the same site and list what changed.")]
    async fn compare_audits(
        &self,
        Parameters(CompareAuditsRequest { audit_a, audit_b }): Parameters<CompareAuditsRequest>,
    ) -> Result<String, String> {
        let changes = self
            .backend
            .compare(&AuditId(audit_a), &AuditId(audit_b))
            .await
            .map_err(|e| e.to_string())?;
        to_json(&changes)
    }
}

#[tool_handler(router = self.tool_router)]
impl<B: Backend + 'static> ServerHandler for CodoseoMcp<B> {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("codoseo", env!("CARGO_PKG_VERSION")))
            .with_instructions(format!(
                "CodoSEO: crawl sites and check pages locally, with no login and no \
                 cloud calls. Private and internal addresses are allowed. {PROMO_LINE}"
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::AuditCache;
    use crate::local::LocalBackend;
    use codoseo_testkit::{Page, SiteBuilder};
    use rmcp::ServiceExt;
    use rmcp::model::CallToolRequestParams;
    use serde_json::Value;
    use std::time::Duration as StdDuration;

    fn text_of(result: &rmcp::model::CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.clone())
            .unwrap_or_default()
    }

    async fn serve<B: Backend + 'static>(
        server: CodoseoMcp<B>,
    ) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
        let (server_transport, client_transport) = tokio::io::duplex(1 << 16);
        // Hold the server's `RunningService` for the whole connection (via `waiting()`),
        // not just until the handshake completes - dropping it early tears down the
        // session and the client's next write hits a broken pipe.
        tokio::spawn(async move {
            let running = server
                .serve(server_transport)
                .await
                .expect("server handshake");
            let _ = running.waiting().await;
        });
        ().serve(client_transport).await.unwrap()
    }

    fn args(value: Value) -> rmcp::model::JsonObject {
        serde_json::from_value(value).unwrap()
    }

    #[tokio::test]
    async fn tools_list_has_all_8_tools_with_descriptions() {
        let backend = LocalBackend::new(AuditCache::new(tempfile::tempdir().unwrap().keep()));
        let client = serve(CodoseoMcp::new(backend)).await;
        let tools = client.peer().list_tools(None).await.unwrap();
        let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
        for expected in [
            "audit_site",
            "get_audit",
            "get_issue_urls",
            "get_page",
            "check_page",
            "check_robots",
            "check_redirects",
            "compare_audits",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
        assert_eq!(tools.tools.len(), 8);
        for t in &tools.tools {
            assert!(
                t.description.as_deref().is_some_and(|d| !d.is_empty()),
                "{} has no description",
                t.name
            );
        }
    }

    #[tokio::test]
    async fn audit_site_on_a_fast_site_returns_the_summary_in_one_call() {
        let site = SiteBuilder::new().html("/", "Home", &[]).start().await;
        let backend = LocalBackend::new(AuditCache::new(tempfile::tempdir().unwrap().keep()));
        let client = serve(CodoseoMcp::new(backend)).await;

        let result = client
            .peer()
            .call_tool(
                CallToolRequestParams::new("audit_site").with_arguments(args(json!({
                    "url": site.url("/").to_string(),
                }))),
            )
            .await
            .unwrap();
        let body: Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(body["pages_crawled"], 1);
        assert!(body["audit_id"].is_string());
    }

    #[tokio::test]
    async fn check_page_round_trips_over_the_wire() {
        let site = SiteBuilder::new().html("/", "Hello", &[]).start().await;
        let backend = LocalBackend::new(AuditCache::new(tempfile::tempdir().unwrap().keep()));
        let client = serve(CodoseoMcp::new(backend)).await;

        let result = client
            .peer()
            .call_tool(
                CallToolRequestParams::new("check_page").with_arguments(args(json!({
                    "url": site.url("/").to_string(),
                }))),
            )
            .await
            .unwrap();
        let body: Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(body["fields"]["title"], "Hello");
    }

    #[tokio::test]
    async fn audit_site_reports_running_past_the_cutover_then_get_audit_finishes() {
        let site = SiteBuilder::new()
            .page("/", Page::slow(StdDuration::from_millis(400)))
            .start()
            .await;
        let backend = LocalBackend::new(AuditCache::new(tempfile::tempdir().unwrap().keep()));
        let client = serve(CodoseoMcp::with_wait_timeout(
            backend,
            StdDuration::from_millis(50),
        ))
        .await;

        let result = client
            .peer()
            .call_tool(
                CallToolRequestParams::new("audit_site").with_arguments(args(json!({
                    "url": site.url("/").to_string(),
                }))),
            )
            .await
            .unwrap();
        let body: Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(body["status"], "running");
        let audit_id = body["audit_id"].as_str().unwrap().to_owned();

        // Poll rather than sleep: the crawl's pace (the slow start page plus the
        // rate-limited robots, tdmrep and sitemap requests) is not what this tests.
        let mut body = Value::Null;
        for _ in 0..40 {
            let result = client
                .peer()
                .call_tool(
                    CallToolRequestParams::new("get_audit").with_arguments(args(json!({
                        "audit_id": audit_id,
                    }))),
                )
                .await
                .unwrap();
            body = serde_json::from_str(&text_of(&result)).unwrap();
            if body["status"] != "running" {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(250)).await;
        }
        assert_eq!(body["status"], "done");
    }

    /// A test-only backend that fabricates a 500-page, every-check-failing summary
    /// without crawling anything, so the wire-level size check is fast.
    struct HugeFailureBackend;

    impl Backend for HugeFailureBackend {
        async fn audit(
            &self,
            _url: Url,
            _max_pages: u32,
        ) -> Result<crate::types::AuditHandle, crate::backend::BackendError> {
            Ok(crate::types::AuditHandle {
                id: AuditId("huge".to_owned()),
                status: AuditStatus::Done,
            })
        }

        async fn get_audit(
            &self,
            id: &AuditId,
        ) -> Result<crate::types::AuditState, crate::backend::BackendError> {
            use crate::types::{AuditSummary, FailingCheck, MAX_FAILING_CHECKS};
            let all = codoseo_core::check::CheckId::ALL;
            let failing_checks = all[..MAX_FAILING_CHECKS]
                .iter()
                .map(|&check| FailingCheck {
                    check,
                    title: "A reasonably descriptive check title".to_owned(),
                    severity: codoseo_core::check::Severity::Warning,
                    count: 500,
                    example_urls: (0..3)
                        .map(|i| {
                            Url::parse(&format!("https://example.com/some/long/path/{i}")).unwrap()
                        })
                        .collect(),
                })
                .collect();
            Ok(crate::types::AuditState {
                id: id.clone(),
                status: AuditStatus::Done,
                progress: None,
                summary: Some(AuditSummary {
                    audit_id: id.clone(),
                    start_url: Url::parse("https://example.com/").unwrap(),
                    health_score: 0,
                    checks_passed: 0,
                    checks_total: all.len() as u16,
                    pages_crawled: 500,
                    stop_reason: "completed".to_owned(),
                    failing_checks,
                    more_failing_checks: (all.len() - MAX_FAILING_CHECKS) as u16,
                }),
            })
        }

        async fn issue_urls(
            &self,
            _id: &AuditId,
            _check: codoseo_core::check::CheckId,
            _limit: u32,
            _offset: u32,
        ) -> Result<Vec<crate::types::UrlRow>, crate::backend::BackendError> {
            unimplemented!("not exercised by this test")
        }

        async fn page(
            &self,
            _id: &AuditId,
            _url: &Url,
        ) -> Result<codoseo_core::page::PageRecord, crate::backend::BackendError> {
            unimplemented!("not exercised by this test")
        }

        async fn check_page(
            &self,
            _url: Url,
        ) -> Result<codoseo_core::page::PageRecord, crate::backend::BackendError> {
            unimplemented!("not exercised by this test")
        }

        async fn check_robots(
            &self,
            _url: Url,
            _path: Option<String>,
        ) -> Result<crate::types::RobotsReport, crate::backend::BackendError> {
            unimplemented!("not exercised by this test")
        }

        async fn check_redirects(
            &self,
            _url: Url,
        ) -> Result<crate::types::RedirectReport, crate::backend::BackendError> {
            unimplemented!("not exercised by this test")
        }

        async fn compare(
            &self,
            _a: &AuditId,
            _b: &AuditId,
        ) -> Result<Vec<codoseo_core::change::Change>, crate::backend::BackendError> {
            unimplemented!("not exercised by this test")
        }
    }

    #[tokio::test]
    async fn audit_site_wire_response_for_a_huge_failure_stays_under_4kb() {
        let client = serve(CodoseoMcp::new(HugeFailureBackend)).await;
        let result = client
            .peer()
            .call_tool(
                CallToolRequestParams::new("audit_site").with_arguments(args(json!({
                    "url": "https://example.com/",
                }))),
            )
            .await
            .unwrap();
        let text = text_of(&result);
        assert!(
            text.len() < 4096,
            "audit_site response is {} bytes, expected under 4096",
            text.len()
        );
    }
}
