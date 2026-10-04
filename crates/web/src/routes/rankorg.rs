//! `/go/rankorg`: counts a click and sends the visitor to RankOrg with their domain and top
//! pages. Links on the audit preview and the explorer point here instead of straight at
//! RankOrg, so every click lands in the funnel. Cloud only.

use axum::Router;
use axum::extract::{OriginalUri, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use codoseo_store::events::{self, EventKind};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::quick::require_cloud;
use crate::auth::{CurrentUser, urlencode};
use crate::error::AppError;
use crate::rankorg;
use crate::state::AppState;

/// How many pages go to RankOrg.
const TOP_PAGES: i64 = 10;

pub fn routes() -> Router<AppState> {
    Router::new().route("/go/rankorg", get(go))
}

#[derive(Deserialize)]
pub struct GoQuery {
    src: Option<String>,
    audit: Option<String>,
    site: Option<String>,
}

fn parse_id(raw: Option<&str>) -> Result<Uuid, AppError> {
    raw.and_then(|r| Uuid::parse_str(r).ok())
        .ok_or(AppError::NotFound)
}

async fn go(
    State(state): State<AppState>,
    user: Option<CurrentUser>,
    OriginalUri(uri): OriginalUri,
    Query(q): Query<GoQuery>,
) -> Result<Response, AppError> {
    require_cloud(&state)?;
    let pool = &state.pool;
    let (domain, pages, medium, account_id, site_id, payload) = match q.src.as_deref() {
        // The public audit preview: anyone with the report's link.
        Some("audit") => {
            let id = parse_id(q.audit.as_deref())?;
            let audit = codoseo_store::quick::get(pool, id)
                .await?
                .ok_or(AppError::NotFound)?;
            let pages = codoseo_store::reports::top_pages_by_inlinks(pool, id, TOP_PAGES).await?;
            (
                audit.domain,
                pages,
                "audit_preview",
                None,
                Some(audit.site_id),
                json!({ "src": "audit", "crawl_id": id }),
            )
        }
        // The explorer: the owner of the site only.
        Some("explorer") => {
            let id = parse_id(q.site.as_deref())?;
            let Some(user) = user else {
                let back = uri.path_and_query().map_or("/", |p| p.as_str());
                return Ok(
                    Redirect::to(&format!("/login?next={}", urlencode(back))).into_response()
                );
            };
            let site = codoseo_store::sites::get_for_account(pool, user.id(), id)
                .await?
                .ok_or(AppError::NotFound)?;
            let pages = match codoseo_store::crawls::latest_done(pool, site.id).await? {
                Some(crawl) => {
                    codoseo_store::reports::top_pages_by_inlinks(pool, crawl.id, TOP_PAGES).await?
                }
                None => Vec::new(),
            };
            (
                site.domain,
                pages,
                "explorer",
                Some(user.id()),
                Some(site.id),
                json!({ "src": "explorer" }),
            )
        }
        _ => return Err(AppError::NotFound),
    };

    events::record(
        pool,
        EventKind::RankorgClick,
        account_id,
        site_id,
        Some(payload),
    )
    .await?;
    let to = rankorg::link(&state.config.rankorg_url, &domain, &pages, medium);
    Ok(Redirect::to(to.as_str()).into_response())
}
