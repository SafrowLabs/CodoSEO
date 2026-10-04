//! Signed-in screens. Each module owns its routes and merges them in here.

pub mod account;
pub mod audit;
pub mod changes;
pub mod crawls;
pub mod explorer;
pub mod export;
pub mod search;
pub mod sites;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;

use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(home))
        .merge(account::routes())
        .merge(sites::routes())
        .merge(crawls::routes())
        .merge(explorer::routes())
        .merge(audit::routes())
        .merge(changes::routes())
        .merge(export::routes())
        .merge(search::routes())
}

/// `/`: the first site's audit, or onboarding when there is no site yet. Signed-out visitors
/// go to login (M6 puts the cloud landing page here).
async fn home(State(state): State<AppState>, user: CurrentUser) -> Result<Response, AppError> {
    let sites = codoseo_store::sites::list_for_account(&state.pool, user.id()).await?;
    Ok(match sites.first() {
        Some(s) => Redirect::to(&format!("/s/{}/audit", s.id)).into_response(),
        None => Redirect::to("/sites").into_response(),
    })
}
