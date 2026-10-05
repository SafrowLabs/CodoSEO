//! Signed-in screens. Each module owns its routes and merges them in here.

pub mod account;
pub mod admin;
pub mod audit;
pub mod bot;
pub mod changes;
pub mod crawls;
pub mod explorer;
pub mod export;
pub mod landing;
pub mod quick;
pub mod rankorg;
pub mod search;
pub mod seo;
pub mod sites;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;

use crate::auth::CurrentUser;
use crate::config::Mode;
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
        .merge(quick::routes())
        .merge(admin::routes())
        .merge(rankorg::routes())
        .route("/bot", get(bot::page))
        .route("/robots.txt", get(seo::robots))
        .route("/llms.txt", get(seo::llms))
        .route("/sitemap.xml", get(seo::sitemap))
}

/// `/`: the first site's audit, or onboarding when there is no site yet. A signed-out visitor
/// gets the landing page on the cloud and the login page everywhere else.
async fn home(
    State(state): State<AppState>,
    user: Option<CurrentUser>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return if state.config.mode == Mode::Cloud {
            landing::page(&state)
        } else {
            Ok(Redirect::to("/login?next=%2F").into_response())
        };
    };
    let sites = codoseo_store::sites::list_for_account(&state.pool, user.id()).await?;
    Ok(match sites.first() {
        Some(s) => Redirect::to(&format!("/s/{}/audit", s.id)).into_response(),
        None => Redirect::to("/sites").into_response(),
    })
}
