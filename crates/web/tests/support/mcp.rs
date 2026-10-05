//! A no-key MCP client for tests: one JSON-RPC POST per call to `/mcp`, as a connector sends it,
//! with the `User-Agent` and client address (`CF-Connecting-IP`) of the caller under test.

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use codoseo_mcp::cloud::CloudMcp;
use codoseo_web::agent::mcp::AgentBackend;
use codoseo_web::state::AppState;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{TestApp, TestResponse};

pub const HOST: &str = "codoseo.com";
pub const PROTOCOL: &str = "2025-06-18";
/// What a hosted connector's `User-Agent` contains, and what Claude Code's doesn't.
pub const SHARED_UA: &str = "Claude-User/1.0 (+https://anthropic.com)";
pub const DIRECT_UA: &str = "claude-code/2.0 (cli)";

pub fn jsonrpc(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
}

/// A no-key MCP client: one JSON-RPC POST per call, as a connector sends it.
pub struct Client<'a> {
    state: AppState,
    router: Router,
    user_agent: Option<&'a str>,
    ip: Option<&'a str>,
}

impl<'a> Client<'a> {
    /// A client whose `quick_audit` waits `wait` for a fresh audit (zero: answers at once).
    pub fn new(app: &'a TestApp, wait: Duration, user_agent: Option<&'a str>) -> Client<'a> {
        let handler = CloudMcp::new(AgentBackend::new(app.state.clone()))
            .with_quick_audit_wait(wait, Duration::from_millis(50));
        Client {
            state: app.state.clone(),
            router: codoseo_web::routes::mcp::router_for(&app.state, handler)
                .with_state(app.state.clone()),
            user_agent,
            ip: None,
        }
    }

    /// Looks at a waiting audit every `poll` instead of every 50 ms.
    pub fn with_poll(mut self, wait: Duration, poll: Duration) -> Client<'a> {
        let handler = CloudMcp::new(AgentBackend::new(self.state.clone()))
            .with_quick_audit_wait(wait, poll);
        self.router = codoseo_web::routes::mcp::router_for(&self.state, handler)
            .with_state(self.state.clone());
        self
    }

    pub fn with_ip(mut self, ip: &'a str) -> Client<'a> {
        self.ip = Some(ip);
        self
    }

    pub async fn post(&self, body: Value) -> TestResponse {
        let mut b = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("mcp-protocol-version", PROTOCOL);
        if let Some(ua) = self.user_agent {
            b = b.header(header::USER_AGENT, ua);
        }
        if let Some(ip) = self.ip {
            b = b.header("cf-connecting-ip", ip);
        }
        let res = tower::ServiceExt::oneshot(
            self.router.clone(),
            b.body(Body::from(body.to_string())).unwrap(),
        )
        .await
        .expect("infallible");
        let (status, headers) = (res.status(), res.headers().clone());
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("body");
        TestResponse {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }

    pub async fn result(&self, method: &str, params: Value) -> Value {
        let res = self.post(jsonrpc(method, params)).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
        let body: Value = serde_json::from_str(&res.body).expect("json-rpc body");
        assert!(body.get("error").is_none(), "protocol error: {}", res.body);
        body["result"].clone()
    }

    pub async fn tools(&self) -> Vec<Value> {
        self.result("tools/list", json!({})).await["tools"]
            .as_array()
            .unwrap()
            .clone()
    }

    /// A `tools/call` result: `(is_error, text)`.
    pub async fn call(&self, name: &str, args: Value) -> (bool, String) {
        let result = self
            .result("tools/call", json!({"name": name, "arguments": args}))
            .await;
        let text = result["content"][0]["text"].as_str().unwrap().to_owned();
        (result["isError"].as_bool().unwrap_or(false), text)
    }

    pub async fn call_ok(&self, name: &str, args: Value) -> Value {
        let (is_error, text) = self.call(name, args).await;
        assert!(!is_error, "{name} failed: {text}");
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name} json ({e}): {text}"))
    }

    pub async fn call_err(&self, name: &str, args: Value) -> String {
        let (is_error, text) = self.call(name, args).await;
        assert!(is_error, "{name} should have failed: {text}");
        text
    }

    /// Starts an audit and returns its id (the wait is short, so it is "running").
    pub async fn audit_id(&self, url: &str) -> Uuid {
        let state = self.call_ok("quick_audit", json!({"url": url})).await;
        Uuid::parse_str(state["audit_id"].as_str().unwrap()).unwrap()
    }
}
