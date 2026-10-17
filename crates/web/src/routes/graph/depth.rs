//! `/s/{site}/graph/depth`: pages per click depth, depth by folder and the deep pages worth a link from a hub.

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use super::{Empty, Tab, TabLink};
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::layout::Shell;
use crate::render::html;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/s/{site}/graph/depth", get(depth_page))
}

#[derive(Template)]
#[template(path = "graph/depth.html")]
pub struct DepthTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
}

async fn depth_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Depth).await?;
    Ok(html(&DepthTab {
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
    })?
    .into_response())
}
