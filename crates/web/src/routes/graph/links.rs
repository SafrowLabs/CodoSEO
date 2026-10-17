//! `/s/{site}/graph/links`: broken pages with the pages that link to them, and redirect chains hop by hop.

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
    Router::new().route("/s/{site}/graph/links", get(links_page))
}

#[derive(Template)]
#[template(path = "graph/links.html")]
pub struct LinksTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
}

async fn links_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Links).await?;
    Ok(html(&LinksTab {
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
    })?
    .into_response())
}
