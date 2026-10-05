//! Spec section 8: every POST checks `Origin`. A state-changing request whose `Origin` (or, if
//! a browser left that out, `Referer`) isn't this app's own origin is refused with a 403, which
//! stops cross-site form posts even though sessions use `SameSite=Lax` cookies.

use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use url::Url;

use crate::error::AppError;
use crate::state::AppState;

/// Exact paths that take machine-to-machine POSTs authenticated some other way (signatures,
/// API keys): Dodo's billing webhook, which is signed. M8's API adds its own. An exact match,
/// so nothing under or beside these paths is exempt by accident.
const EXEMPT_PATHS: &[&str] = &["/billing/webhook"];

pub async fn check_origin(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let unsafe_method = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let exempt = EXEMPT_PATHS.contains(&req.uri().path());
    if unsafe_method && !exempt && !same_origin(&req, &state.config.origin()) {
        return AppError::Forbidden(
            "This request came from another site, so it was blocked.".to_owned(),
        )
        .into_response();
    }
    next.run(req).await
}

fn same_origin(req: &Request, expected: &str) -> bool {
    let headers = req.headers();
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        return origin == expected;
    }
    headers
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| Url::parse(r).ok())
        .is_some_and(|r| r.origin().ascii_serialization() == expected)
}
