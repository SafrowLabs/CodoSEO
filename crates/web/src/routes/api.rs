//! `/api/v1`: the REST API over the user's monitored sites. Every route is a thin layer over
//! [`AgentService`], which the cloud MCP tools share, so both return the same JSON and count
//! against the same daily quota.
//!
//! Authentication is `Authorization: Bearer <key>` and nothing else (no cookies). Errors are
//! `{"error":{"code","message"}}`; every keyed response carries `X-RateLimit-Limit` and
//! `X-RateLimit-Remaining` (calls left today, UTC), both left out on plans without a limit, and
//! a 429 adds `Retry-After` (seconds to the next 00:00 UTC). The routes are exempt from the
//! `Origin` check, since no browser session reaches them.

use axum::Json;
use axum::Router;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde::Serialize;

use crate::agent::auth::ApiCaller;
use crate::agent::error::AgentError;
use crate::agent::service::{AgentService, Reply};
use crate::state::AppState;

/// Where the API lives; the `Origin` check exempts everything under it.
pub const PREFIX: &str = "/api/v1";

pub fn routes() -> Router<AppState> {
    let api = Router::new()
        .route("/sites", get(list_sites))
        .route("/sites/{site}", get(site_health))
        .route("/sites/{site}/issues/{check}", get(issue_urls))
        .route("/sites/{site}/page", get(page))
        .route("/sites/{site}/changes", get(changes))
        .route("/sites/{site}/crawls", post(run_crawl))
        .route("/usage", get(usage))
        .fallback(unknown)
        .method_not_allowed_fallback(wrong_method);
    Router::new().nest(PREFIX, api)
}

/// An unknown path under the API is a JSON 404 too, not the HTML page.
async fn unknown() -> AgentError {
    AgentError::NotFound("No such API endpoint.".to_owned())
}

/// A known path with the wrong HTTP method is a JSON error too, not an empty 405.
async fn wrong_method() -> AgentError {
    AgentError::MethodNotAllowed("That HTTP method is not allowed for this endpoint.".to_owned())
}

/// A query string, parsed but not yet judged: a malformed one is the API's JSON 400, which
/// still costs the authenticated caller one call, so the verdict waits for [`counted`].
struct ApiQuery<T>(Result<T, AgentError>);

impl<T: serde::de::DeserializeOwned> FromRequestParts<AppState> for ApiQuery<T> {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<ApiQuery<T>, Self::Rejection> {
        Ok(ApiQuery(
            Query::<T>::from_request_parts(parts, state)
                .await
                .map(|Query(q)| q)
                .map_err(|e| AgentError::BadRequest(e.body_text())),
        ))
    }
}

/// Runs `call` with the parsed query, or charges one call and answers the 400 when the query
/// didn't parse.
async fn counted<Q, T: Serialize, F: Future<Output = Reply<T>>>(
    service: &AgentService<'_>,
    caller: &ApiCaller,
    query: ApiQuery<Q>,
    call: impl FnOnce(Q) -> F,
) -> Response {
    match query.0 {
        Ok(q) => respond(StatusCode::OK, call(q).await),
        Err(e) => respond(StatusCode::OK, service.refuse(caller, e).await),
    }
}

/// The reply as a response: the body, or the JSON error, with the rate-limit headers.
fn respond<T: Serialize>(status: StatusCode, reply: Reply<T>) -> Response {
    let mut res = match reply.outcome {
        Ok(value) => (status, Json(value)).into_response(),
        Err(e) => e.into_response(),
    };
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(quota) = reply.quota {
        if let Some(limit) = quota.limit {
            h.insert("x-ratelimit-limit", HeaderValue::from(limit));
        }
        if let Some(remaining) = quota.remaining {
            h.insert("x-ratelimit-remaining", HeaderValue::from(remaining));
        }
    }
    res
}

async fn list_sites(State(state): State<AppState>, caller: ApiCaller) -> Response {
    respond(
        StatusCode::OK,
        AgentService::new(&state).list_sites(&caller).await,
    )
}

async fn site_health(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path(site): Path<String>,
) -> Response {
    respond(
        StatusCode::OK,
        AgentService::new(&state).site_health(&caller, &site).await,
    )
}

#[derive(Deserialize)]
struct PagingQuery {
    limit: Option<u32>,
    offset: Option<u32>,
}

async fn issue_urls(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path((site, check)): Path<(String, String)>,
    q: ApiQuery<PagingQuery>,
) -> Response {
    let service = AgentService::new(&state);
    counted(&service, &caller, q, |q| {
        service.issue_urls(&caller, &site, &check, q.limit, q.offset)
    })
    .await
}

#[derive(Deserialize)]
struct PageQuery {
    url: Option<String>,
}

async fn page(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path(site): Path<String>,
    q: ApiQuery<PageQuery>,
) -> Response {
    let service = AgentService::new(&state);
    let (svc, who, site) = (&service, &caller, &site);
    counted(&service, &caller, q, move |q| async move {
        svc.page(who, site, q.url.as_deref().unwrap_or("")).await
    })
    .await
}

#[derive(Deserialize)]
struct ChangesQuery {
    severity: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

async fn changes(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path(site): Path<String>,
    q: ApiQuery<ChangesQuery>,
) -> Response {
    let service = AgentService::new(&state);
    let (svc, who, site) = (&service, &caller, &site);
    counted(&service, &caller, q, move |q| async move {
        svc.changes(who, site, q.severity.as_deref(), q.limit, q.offset)
            .await
    })
    .await
}

async fn run_crawl(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path(site): Path<String>,
) -> Response {
    respond(
        StatusCode::ACCEPTED,
        AgentService::new(&state).run_crawl(&caller, &site).await,
    )
}

/// Not counted against the allowance.
async fn usage(State(state): State<AppState>, caller: ApiCaller) -> Response {
    respond(
        StatusCode::OK,
        AgentService::new(&state).usage(&caller).await,
    )
}
