//! `/account`: who is signed in, their plan, sign out, and (self-hosted owner) whether new
//! people may sign up.

use askama::Template;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;

use crate::auth::CurrentUser;
use crate::config::Mode;
use crate::error::AppError;
use crate::layout::{Screen, Shell};
use crate::render::html;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/account", get(page))
        .route("/account/signups", post(set_signups))
}

#[derive(Template)]
#[template(path = "account/index.html")]
pub struct AccountPage {
    pub shell: Shell,
    pub email: String,
    pub plan: String,
    pub self_hosted: bool,
    pub is_owner: bool,
    pub signups_open: bool,
}

async fn page(State(state): State<AppState>, user: CurrentUser) -> Result<Response, AppError> {
    let shell = Shell::load(&state, &user, None, Screen::Account).await?;
    let signups_open = codoseo_store::accounts::signups_open(&state.pool).await?;
    Ok(html(&AccountPage {
        plan: shell.user.plan_label.clone(),
        shell,
        email: user.account.email.clone(),
        self_hosted: state.config.mode == Mode::SelfHost,
        is_owner: user.account.is_owner,
        signups_open,
    })?
    .into_response())
}

#[derive(Deserialize)]
struct SignupsForm {
    open: String,
}

async fn set_signups(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<SignupsForm>,
) -> Result<Response, AppError> {
    if state.config.mode != Mode::SelfHost || !user.account.is_owner {
        return Err(AppError::Forbidden(
            "Only the instance owner can change signups.".to_owned(),
        ));
    }
    codoseo_store::accounts::set_signups_open(&state.pool, form.open == "true").await?;
    Ok(Redirect::to("/account").into_response())
}
