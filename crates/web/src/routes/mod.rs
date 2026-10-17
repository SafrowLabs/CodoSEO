//! Signed-in screens. Each module owns its routes and merges them in here.

pub mod account;
pub mod admin;
pub mod ai_access;
pub mod ai_access_bots;
pub mod ai_access_robots;
pub mod ai_bots;
pub mod ai_overview;
pub mod api;
pub mod audit;
pub mod audit_ai;
pub mod audit_fix;
pub mod audit_trend;
pub mod billing;
pub mod bot;
pub mod changes;
pub mod crawls;
pub mod demo;
pub mod explorer;
pub mod export;
pub mod landing;
pub mod legal;
pub mod mcp;
pub mod monitoring;
pub mod policy;
pub mod policy_seo;
pub mod quick;
pub mod rankorg;
pub mod search;
pub mod seo;
pub mod settings_alerts;
pub mod settings_keys;
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
        .route("/app", get(app_home))
        .merge(account::routes())
        .merge(sites::routes())
        .merge(crawls::routes())
        .merge(explorer::routes())
        .merge(audit::routes())
        .merge(changes::routes())
        .merge(ai_access::routes())
        .merge(policy::routes())
        .merge(ai_bots::routes())
        .merge(export::routes())
        .merge(search::routes())
        .merge(quick::routes())
        .merge(monitoring::routes())
        .merge(settings_alerts::routes())
        .merge(settings_keys::routes())
        .merge(api::routes())
        .merge(billing::routes())
        .merge(admin::routes())
        .merge(rankorg::routes())
        .merge(legal::routes())
        .merge(demo::routes())
        .route("/bot", get(bot::page))
        .route("/stats/pages-crawled", get(landing::pages_crawled))
        .route("/robots.txt", get(seo::robots))
        .route("/llms.txt", get(seo::llms))
        .route("/sitemap.xml", get(seo::sitemap))
}

/// `/`: the landing page on the cloud, signed in or not (it links a signed-in visitor to the
/// app). Elsewhere a signed-in user goes to the app and a signed-out one to the login page.
async fn home(
    State(state): State<AppState>,
    user: Option<CurrentUser>,
) -> Result<Response, AppError> {
    if state.config.mode == Mode::Cloud {
        return landing::page(&state, user.is_some()).await;
    }
    match user {
        Some(user) => app_home(State(state), user).await,
        None => Ok(Redirect::to("/login?next=%2Fapp").into_response()),
    }
}

/// `/app`: the first site's audit, or onboarding when there is no site yet.
async fn app_home(State(state): State<AppState>, user: CurrentUser) -> Result<Response, AppError> {
    let sites = codoseo_store::sites::list_for_account(&state.pool, user.id()).await?;
    Ok(match sites.first() {
        Some(s) => Redirect::to(&format!("/s/{}/audit", s.id)).into_response(),
        None => Redirect::to("/sites").into_response(),
    })
}
