//! `/terms`, `/privacy` and `/refunds`: the hosted service's legal pages (cloud only). They
//! cover codoseo.com, not the self-hosted software, whose license is the AGPL in the repo.

use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use super::quick::require_cloud;
use crate::config::UmamiConfig;
use crate::error::AppError;
use crate::render::html;
use crate::state::AppState;

/// Shown at the top of each page, as words and as the `datetime`. Change both whenever the
/// wording does.
pub const LAST_UPDATED: &str = "13 October 2026";
pub const LAST_UPDATED_ISO: &str = "2026-10-13";

/// Where support, legal notices, privacy requests and crawl complaints go.
pub const CONTACT: &str = "contact@codoseo.com";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/terms", get(terms))
        .route("/privacy", get(privacy))
        .route("/refunds", get(refunds))
}

// Each page has the same fields: the origin for its canonical link, and analytics. The date
// and the contact address come from the constants above, straight from the templates.

#[derive(Template)]
#[template(path = "legal/terms.html")]
pub struct TermsPage {
    pub base: String,
    pub umami: Option<UmamiConfig>,
}

#[derive(Template)]
#[template(path = "legal/privacy.html")]
pub struct PrivacyPage {
    pub base: String,
    pub umami: Option<UmamiConfig>,
}

#[derive(Template)]
#[template(path = "legal/refunds.html")]
pub struct RefundsPage {
    pub base: String,
    pub umami: Option<UmamiConfig>,
}

async fn terms(State(state): State<AppState>) -> Result<Response, AppError> {
    require_cloud(&state)?;
    let (base, umami) = (state.config.origin(), state.config.umami.clone());
    Ok(html(&TermsPage { base, umami })?.into_response())
}

async fn privacy(State(state): State<AppState>) -> Result<Response, AppError> {
    require_cloud(&state)?;
    let (base, umami) = (state.config.origin(), state.config.umami.clone());
    Ok(html(&PrivacyPage { base, umami })?.into_response())
}

async fn refunds(State(state): State<AppState>) -> Result<Response, AppError> {
    require_cloud(&state)?;
    let (base, umami) = (state.config.origin(), state.config.umami.clone());
    Ok(html(&RefundsPage { base, umami })?.into_response())
}
