//! `/s/{site}/search?q=`: the ⌘K palette's "Pages" section. `app.js` fetches it as you type
//! and inserts the HTML as-is, so the response is only `.pal-item` links into the explorer
//! (or nothing at all, and the palette shows its own empty state).

use askama::Template;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::{CurrentUser, load_site};
use crate::error::AppError;
use crate::render::html;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/s/{site}/search", get(search))
}

/// The most pages the palette lists.
const LIMIT: i64 = 8;

#[derive(Deserialize)]
struct SearchQuery {
    q: Option<String>,
}

/// One palette item.
struct Hit {
    /// The explorer with this page selected.
    href: String,
    /// Path plus query.
    path: String,
    /// The title, or a dash when the page has none.
    title: String,
}

#[derive(Template)]
#[template(path = "search/results.html")]
struct Results {
    hits: Vec<Hit>,
}

async fn search(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
    Query(query): Query<SearchQuery>,
) -> Result<Response, AppError> {
    let site_id = Uuid::parse_str(&site).map_err(|_| AppError::NotFound)?;
    let site = load_site(&state, &user, site_id).await?;
    let q = query.q.as_deref().unwrap_or_default().trim();
    if q.is_empty() {
        return Ok(nothing());
    }
    let Some(crawl) = codoseo_store::crawls::latest_done(&state.pool, site.id).await? else {
        return Ok(nothing());
    };
    let found = codoseo_store::search::pages(&state.pool, crawl.id, q, LIMIT).await?;
    if found.is_empty() {
        return Ok(nothing());
    }
    let hits = found
        .into_iter()
        .map(|h| Hit {
            href: format!("/s/{}/explorer?sel={:016x}", site.id, h.url_hash),
            path: if h.path.is_empty() {
                "/".to_owned()
            } else {
                h.path
            },
            title: h
                .title
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "—".to_owned()),
        })
        .collect();
    Ok(html(&Results { hits })?.into_response())
}

/// An empty 200: no pages to offer.
fn nothing() -> Response {
    Html(String::new()).into_response()
}
