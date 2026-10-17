//! `/s/{site}/graph/sitemap`: the sitemap index and its child sitemaps, each split by what the crawl found for its URLs, against the pages the crawl reached by links.

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
    Router::new().route("/s/{site}/graph/sitemap", get(sitemap_page))
}

#[derive(Template)]
#[template(path = "graph/sitemap.html")]
pub struct SitemapTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
}

async fn sitemap_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Sitemap).await?;
    Ok(html(&SitemapTab {
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
    })?
    .into_response())
}
