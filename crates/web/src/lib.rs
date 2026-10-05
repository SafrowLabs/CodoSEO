//! The CodoSEO web app: axum routes, askama templates and htmx partials, in one crate the
//! `codoseo` binary serves as its `web` role.

pub mod abuse;
pub mod agent;
pub mod assets;
pub mod auth;
pub mod billing;
pub mod config;
pub mod crawl_policy;
pub mod error;
pub mod fmt;
pub mod health;
pub mod layout;
pub mod rankorg;
pub mod render;
pub mod routes;
pub mod serp;
pub mod state;
pub mod turnstile;

use std::net::SocketAddr;

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderValue, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use tokio::net::TcpListener;

pub use config::{Config, Mode};
pub use state::AppState;

/// The whole app as a router, ready to serve or to drive in tests with `oneshot`.
pub fn app(state: AppState) -> Router {
    Router::new()
        .merge(auth::router())
        .merge(routes::router())
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .route("/assets/{file}", get(assets::serve))
        .fallback(error::not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::origin::check_origin,
        ))
        .layer(middleware::from_fn(error::error_pages))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

/// Serves the app until `shutdown` resolves.
pub async fn serve(
    state: AppState,
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    // Connect info gives the abuse limits the socket's address when no proxy header applies.
    axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

/// Headers every response gets: no framing, no MIME sniffing, a strict referrer policy, and
/// no caching of signed-in HTML.
async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    let is_html = h
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    if is_html && !h.contains_key(header::CACHE_CONTROL) {
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-cache"),
        );
    }
    res
}
