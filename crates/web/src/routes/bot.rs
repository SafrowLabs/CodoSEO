//! `/bot`: what CodoSEObot is, so site owners can recognise, allow or block it. The crawler's
//! user agent points here.

use askama::Template;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use codoseo_core::crawl::{PROBE_FROM, USER_AGENT};
use codoseo_core::plan::{Plan, PlanLimits};
use codoseo_geo::probe::candidates;

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
    /// The `From` header on every synthetic AI fetch check.
    pub probe_from: &'static str,
    /// The most pages a cloud plan probes after a crawl.
    pub probe_pages: u8,
    /// The tokens whose documented user agents the fetch checks send.
    pub probe_bots: Vec<String>,
    pub base: String,
    pub umami: Option<UmamiConfig>,
}

pub async fn page(State(state): State<AppState>) -> Result<Response, AppError> {
    require_cloud(&state)?;
    Ok(html(&BotPage {
        user_agent: USER_AGENT,
        ip: state.config.bot_ip.clone(),
        probe_from: PROBE_FROM,
        probe_pages: cloud_probe_pages(),
        probe_bots: candidates().0.into_iter().map(|b| b.token).collect(),
        base: state.config.origin(),
        umami: state.config.umami.clone(),
    })?
    .into_response())
}

/// The most pages any cloud plan fetches with the AI bots' user agents after a crawl.
fn cloud_probe_pages() -> u8 {
    [Plan::Free, Plan::Lite, Plan::Pro]
        .into_iter()
        .map(|p| PlanLimits::for_plan(p).probe_urls)
        .max()
        .unwrap_or(0)
}
