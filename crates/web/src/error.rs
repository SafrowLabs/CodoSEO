//! One error type for every handler. Spec section 12: the web side shows friendly error pages,
//! errors inside htmx partials show inline, and a Postgres outage is a 503 page.
//!
//! `AppError::into_response` only records *what* went wrong (an [`ErrorInfo`] extension); the
//! [`error_pages`] middleware, which can see whether the request came from htmx, turns that
//! into either a full page or an inline fragment.

use askama::Template;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};

use crate::render::is_htmx;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Forbidden(String),
    /// The request conflicts with current state, e.g. a crawl is already queued.
    #[error("{0}")]
    Conflict(String),
    /// A plan limit was reached (sites, manual crawls).
    #[error("{0}")]
    Limit(String),
    /// Postgres is unreachable.
    #[error("database unavailable")]
    Unavailable,
    #[error("internal error: {0}")]
    Internal(String),
}

impl AppError {
    pub fn internal(e: impl std::fmt::Display) -> AppError {
        AppError::Internal(e.to_string())
    }

    fn parts(&self) -> (StatusCode, &'static str, String) {
        match self {
            AppError::NotFound => (
                StatusCode::NOT_FOUND,
                "Page not found",
                "That page doesn't exist, or it belongs to another account.".to_owned(),
            ),
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, "Check that again", m.clone()),
            AppError::Forbidden(m) => (StatusCode::FORBIDDEN, "Not allowed", m.clone()),
            AppError::Conflict(m) => (StatusCode::CONFLICT, "Already in progress", m.clone()),
            AppError::Limit(m) => (StatusCode::FORBIDDEN, "Plan limit reached", m.clone()),
            AppError::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "We'll be right back",
                "CodoSEO can't reach its database right now. This page retries on its own."
                    .to_owned(),
            ),
            AppError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "An unexpected error happened on our side. Try again in a moment.".to_owned(),
            ),
        }
    }
}

/// Connection-level failures mean Postgres is down or unreachable: a 503, not a 500.
pub fn is_unavailable(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::Io(_)
        | sqlx::Error::Tls(_) => true,
        // SQLSTATE class 08 is "connection exception"; 57P0x is an admin/crash shutdown.
        sqlx::Error::Database(db) => db
            .code()
            .is_some_and(|c| c.starts_with("08") || c.starts_with("57P0")),
        _ => false,
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> AppError {
        if matches!(e, sqlx::Error::RowNotFound) {
            AppError::NotFound
        } else if is_unavailable(&e) {
            AppError::Unavailable
        } else {
            AppError::Internal(e.to_string())
        }
    }
}

impl From<askama::Error> for AppError {
    fn from(e: askama::Error) -> AppError {
        AppError::Internal(format!("template: {e}"))
    }
}

/// What went wrong, attached to an error response for [`error_pages`] to render.
#[derive(Debug, Clone)]
pub struct ErrorInfo {
    pub status: StatusCode,
    pub title: &'static str,
    pub message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if let AppError::Internal(detail) = &self {
            tracing::error!(%detail, "request failed");
        }
        let (status, title, message) = self.parts();
        let mut res = (status, message.clone()).into_response();
        res.extensions_mut().insert(ErrorInfo {
            status,
            title,
            message,
        });
        res
    }
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage<'a> {
    info: &'a ErrorInfo,
    retry: bool,
}

#[derive(Template)]
#[template(path = "partials/error_inline.html")]
struct ErrorInline<'a> {
    info: &'a ErrorInfo,
}

/// Renders [`ErrorInfo`] responses as a full page, or as an inline fragment for htmx.
pub async fn error_pages(req: Request, next: Next) -> Response {
    let htmx = is_htmx(req.headers());
    let res = next.run(req).await;
    let Some(info) = res.extensions().get::<ErrorInfo>().cloned() else {
        return res;
    };
    let body = if htmx {
        ErrorInline { info: &info }.render()
    } else {
        ErrorPage {
            info: &info,
            retry: info.status == StatusCode::SERVICE_UNAVAILABLE,
        }
        .render()
    };
    let mut out = match body {
        Ok(html) => (info.status, Html(html)).into_response(),
        Err(_) => (info.status, info.message.clone()).into_response(),
    };
    if info.status == StatusCode::SERVICE_UNAVAILABLE {
        out.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("10"));
    }
    out.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    out
}

/// The router's fallback: unknown paths get the friendly 404.
pub async fn not_found() -> AppError {
    AppError::NotFound
}
