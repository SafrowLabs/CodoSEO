//! `/bot`: what CodoSEObot is, so site owners can recognise, allow or block it. The crawler's
//! user agent points here.

use askama::Template;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use codoseo_core::crawl::USER_AGENT;

use super::quick::require_cloud;
use crate::config::UmamiConfig;
use crate::error::AppError;
use crate::render::html;
use crate::state::AppState;

#[derive(Template)]
#[template(path = "bot/index.html")]
pub struct BotPage {
    pub user_agent: &'static str,
    pub ip: Option<String>,
    pub base: String,
    pub umami: Option<UmamiConfig>,
}

pub async fn page(State(state): State<AppState>) -> Result<Response, AppError> {
    require_cloud(&state)?;
    Ok(html(&BotPage {
        user_agent: USER_AGENT,
        ip: state.config.bot_ip.clone(),
        base: state.config.origin(),
        umami: state.config.umami.clone(),
    })?
    .into_response())
}
