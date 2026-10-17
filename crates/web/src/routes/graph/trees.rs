//! `/s/{site}/graph/tree`, `/folders` and `/map`: the crawl tree by clicks, the folder tree by URL and the force-directed bubble map, drawn in the browser by `graph.js` from `data.json`.

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
    Router::new()
        .route("/s/{site}/graph/tree", get(tree_page))
        .route("/s/{site}/graph/folders", get(folders_page))
        .route("/s/{site}/graph/map", get(map_page))
}

#[derive(Template)]
#[template(path = "graph/tree.html")]
pub struct TreeTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
}

async fn tree_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Tree).await?;
    Ok(html(&TreeTab {
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
    })?
    .into_response())
}

#[derive(Template)]
#[template(path = "graph/folders.html")]
pub struct FoldersTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
}

async fn folders_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Folders).await?;
    Ok(html(&FoldersTab {
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
    })?
    .into_response())
}

#[derive(Template)]
#[template(path = "graph/map.html")]
pub struct MapTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
}

async fn map_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Map).await?;
    Ok(html(&MapTab {
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
    })?
    .into_response())
}
