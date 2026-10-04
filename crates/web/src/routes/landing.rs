//! The cloud landing page at `/`: one URL box that starts a no-signup audit.

use askama::Template;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::error::AppError;
use crate::render::html;
use crate::state::AppState;

#[derive(Template)]
#[template(path = "landing/index.html")]
pub struct Landing {
    /// What the visitor typed, kept when the address is refused.
    pub url: String,
    pub error: Option<String>,
    pub checks: usize,
    pub base: String,
}

fn view(state: &AppState, url: &str, error: Option<String>) -> Landing {
    Landing {
        url: url.to_owned(),
        error,
        checks: codoseo_checks::CHECKS.len(),
        base: state.config.origin(),
    }
}

/// The landing page for a signed-out visitor.
pub fn page(state: &AppState) -> Result<Response, AppError> {
    Ok(html(&view(state, "", None))?.into_response())
}

/// The landing page again with a message under the box, for an address we won't audit.
pub fn refuse(state: &AppState, url: &str, message: String) -> Result<Response, AppError> {
    Ok((
        StatusCode::BAD_REQUEST,
        html(&view(state, url.trim(), Some(message)))?,
    )
        .into_response())
}
