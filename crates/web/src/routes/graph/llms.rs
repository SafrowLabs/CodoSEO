//! `/s/{site}/graph/llms`: the llms.txt as a tree (title, sections, links with their status) and the top pages it leaves out.

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
    Router::new().route("/s/{site}/graph/llms", get(llms_page))
}

#[derive(Template)]
#[template(path = "graph/llms.html")]
pub struct LlmsTab {
    pub shell: Shell,
    pub base: String,
    pub tabs: Vec<TabLink>,
    pub empty: Option<Empty>,
}

async fn llms_page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let ctx = super::load(&state, &user, &site, Tab::Llms).await?;
    Ok(html(&LlmsTab {
        shell: ctx.shell,
        base: ctx.base,
        tabs: ctx.tabs,
        empty: ctx.empty,
    })?
    .into_response())
}
