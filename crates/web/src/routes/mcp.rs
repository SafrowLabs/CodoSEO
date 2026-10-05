//! `/mcp`: the cloud MCP server (streamable HTTP, stateless, JSON responses) in front of
//! [`CloudMcp`]. Authentication is the API's: `Authorization: Bearer <key>` and nothing else
//! (a session cookie never counts). One endpoint serves both tiers: a request with a key gets the
//! keyed tools, a request without one gets the no-key tools on the cloud and a 401 when
//! self-hosted. A malformed, unknown or revoked key is a 401 JSON error, never "no key".
//!
//! A small middleware resolves the caller once per HTTP request and puts it in the request's
//! extensions; rmcp copies the request parts into every tool context, where the handler reads
//! it. `initialize`, `tools/list` and pings are free; each keyed `tools/call` costs one call
//! (see [`AgentBackend`]). The route is exempt from the `Origin` check (no cookie reaches it),
//! and rmcp itself refuses a `Host` that isn't this app's (DNS rebinding).

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use codoseo_mcp::cloud::{Caller, CloudMcp};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use crate::agent::auth::{self, ApiCaller};
use crate::agent::mcp::AgentBackend;
use crate::config::{Config, Mode};
use crate::state::AppState;

/// Where the MCP server lives; the `Origin` check exempts exactly this path.
pub const PATH: &str = "/mcp";

/// The caller as the middleware resolves it for [`AgentBackend`].
type McpCaller = Caller<ApiCaller, ()>;

pub fn routes(state: &AppState) -> Router<AppState> {
    let handler = CloudMcp::new(AgentBackend::new(state.clone()));
    let config = StreamableHttpServerConfig::default()
        // No sessions: every request stands alone, so any instance can answer it.
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(allowed_hosts(&state.config));
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    Router::new()
        .route_service(PATH, service)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            resolve_caller,
        ))
}

/// The `Host` values rmcp accepts: this app's own (with its port when `BASE_URL` has one) and
/// the loopback names, which a local client or a test server uses.
fn allowed_hosts(config: &Config) -> Vec<String> {
    let mut hosts: Vec<String> = ["localhost", "127.0.0.1", "::1"].map(str::to_owned).into();
    if let Some(host) = config.base_url.host_str() {
        hosts.push(match config.base_url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        });
    }
    hosts
}

/// Works out who is calling before rmcp sees the request. A bad key, or no key on a self-hosted
/// server, ends here as the API's JSON 401.
async fn resolve_caller(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let headers = req.headers();
    let caller: McpCaller = match auth::bearer_key(headers) {
        Ok(None) if state.config.mode == Mode::Cloud => Caller::Anon(()),
        // A key, a bad key, or no key where one is required: `authenticate` says which.
        _ => match auth::authenticate(&state, headers).await {
            Ok(keyed) => Caller::Keyed(keyed),
            Err(e) => return e.into_response(),
        },
    };
    req.extensions_mut().insert(caller);
    // The request's parts travel into the tool context; keep the key out of any debug print.
    if let Some(value) = req.headers_mut().get_mut(header::AUTHORIZATION) {
        value.set_sensitive(true);
    }
    let mut res = next.run(req).await;
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}
