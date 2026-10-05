//! What can go wrong in the agent API, with the status, code and message REST answers with
//! (`{"error":{"code":"...","message":"..."}}`). The MCP server shows `message()` as a tool error.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Said for an unknown site id and for another account's site alike, so the two can't be told
/// apart.
pub const SITE_NOT_FOUND: &str = "No such site. List your sites to see their ids.";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    /// No key, a malformed key, an unknown key or a revoked one.
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    /// The site already has a crawl queued or running.
    #[error("{0}")]
    CrawlInProgress(String),
    /// The plan's manual crawl allowance is used up.
    #[error("{0}")]
    PlanLimit(String),
    /// Today's API call allowance is spent. `retry_after_secs` counts to the next 00:00 UTC.
    #[error("You have used all {limit} API calls for today. The count starts again at 00:00 UTC.")]
    QuotaExceeded { limit: u32, retry_after_secs: u64 },
    /// Postgres is unreachable.
    #[error("CodoSEO can't reach its database right now. Try again in a moment.")]
    Unavailable,
    #[error("Something went wrong on our side. Try again in a moment.")]
    Internal(String),
}

impl AgentError {
    pub fn status(&self) -> StatusCode {
        match self {
            AgentError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            AgentError::NotFound(_) => StatusCode::NOT_FOUND,
            AgentError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AgentError::CrawlInProgress(_) => StatusCode::CONFLICT,
            AgentError::PlanLimit(_) => StatusCode::FORBIDDEN,
            AgentError::QuotaExceeded { .. } => StatusCode::TOO_MANY_REQUESTS,
            AgentError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            AgentError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The stable machine name.
    pub fn code(&self) -> &'static str {
        match self {
            AgentError::Unauthorized(_) => "unauthorized",
            AgentError::NotFound(_) => "not_found",
            AgentError::BadRequest(_) => "bad_request",
            AgentError::CrawlInProgress(_) => "crawl_in_progress",
            AgentError::PlanLimit(_) => "plan_limit",
            AgentError::QuotaExceeded { .. } => "quota_exceeded",
            AgentError::Unavailable => "unavailable",
            AgentError::Internal(_) => "internal",
        }
    }

    pub fn message(&self) -> String {
        self.to_string()
    }

    pub fn site_not_found() -> AgentError {
        AgentError::NotFound(SITE_NOT_FOUND.to_owned())
    }
}

impl From<sqlx::Error> for AgentError {
    fn from(e: sqlx::Error) -> AgentError {
        if matches!(e, sqlx::Error::RowNotFound) {
            AgentError::site_not_found()
        } else if crate::error::is_unavailable(&e) {
            AgentError::Unavailable
        } else {
            AgentError::Internal(e.to_string())
        }
    }
}

/// The JSON error response. It never carries `ErrorInfo`, so the HTML error pages leave it alone.
impl IntoResponse for AgentError {
    fn into_response(self) -> Response {
        if let AgentError::Internal(detail) = &self {
            tracing::error!(%detail, "api request failed");
        }
        let body = json!({ "error": { "code": self.code(), "message": self.message() } });
        let mut res = (self.status(), Json(body)).into_response();
        let h = res.headers_mut();
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        match &self {
            AgentError::Unauthorized(_) => {
                h.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            }
            AgentError::QuotaExceeded {
                limit,
                retry_after_secs,
            } => {
                h.insert(header::RETRY_AFTER, HeaderValue::from(*retry_after_secs));
                h.insert("x-ratelimit-limit", HeaderValue::from(*limit));
                h.insert("x-ratelimit-remaining", HeaderValue::from(0u32));
            }
            AgentError::Unavailable => {
                h.insert(header::RETRY_AFTER, HeaderValue::from_static("10"));
            }
            _ => {}
        }
        res
    }
}
