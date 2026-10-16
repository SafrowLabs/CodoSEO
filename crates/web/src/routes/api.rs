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
use axum::body::Bytes;
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde::Serialize;

use crate::agent::auth::ApiCaller;
use crate::agent::error::AgentError;
use crate::agent::service::{AgentService, Reply};
use crate::ai_incident;
use crate::metrics::{self, Surface, Tier};
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
        .route("/sites/{site}/ai-access", get(ai_access))
        .route("/sites/{site}/policy", get(get_policy).put(put_policy))
        .route("/sites/{site}/policy/preview", post(preview_policy))
        .route("/sites/{site}/ai-access/incidents", get(incident_export))
        .route(
            "/sites/{site}/ai-access/incidents/{id}/fix",
            get(incident_fix),
        )
        .route(
            "/sites/{site}/ai-access/incidents/{id}/recheck",
            post(recheck_incident),
        )
        .route(
            "/sites/{site}/ai-access/incidents/{id}/snooze",
            post(snooze_incident).delete(unsnooze_incident),
        )
        .route(
            "/sites/{site}/ai-access/incidents/{id}/accept",
            post(accept_incident).delete(unaccept_incident),
        )
        .route("/sites/{site}/crawls", post(run_crawl))
        .route("/sites/{site}/deploy", post(deploy))
        .route(
            "/sites/{site}/deploy-hook",
            get(deploy_hook_status)
                .post(create_deploy_hook)
                .delete(delete_deploy_hook),
        )
        .route("/hooks/deploy/{secret}", post(deploy_by_secret))
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
        Err(e) => respond(StatusCode::OK, service.refuse::<T>(caller, e).await),
    }
}

/// The reply as a JSON response: the body, or the JSON error, with the rate-limit headers.
fn respond<T: Serialize>(status: StatusCode, reply: Reply<T>) -> Response {
    respond_with(status, reply, |status, value| {
        (status, Json(value)).into_response()
    })
}

/// [`respond`] with another body: `render` makes the success response.
fn respond_with<T>(
    status: StatusCode,
    reply: Reply<T>,
    render: impl FnOnce(StatusCode, T) -> Response,
) -> Response {
    metrics::api_request(
        Surface::Rest,
        Tier::Key,
        reply
            .outcome
            .as_ref()
            .map_or_else(AgentError::code, |_| "ok"),
    );
    let mut res = match reply.outcome {
        Ok(value) => render(status, value),
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

async fn ai_access(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path(site): Path<String>,
) -> Response {
    respond(
        StatusCode::OK,
        AgentService::new(&state).ai_access(&caller, &site).await,
    )
}

/// `Accept: application/toml` asks for the policy as a `codoseo.toml` file.
fn wants_toml(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/toml"))
}

async fn get_policy(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path(site): Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    let reply = AgentService::new(&state).policy(&caller, &site).await;
    if wants_toml(&headers) {
        respond_with(StatusCode::OK, reply, |status, info| {
            (
                status,
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/toml; charset=utf-8"),
                )],
                info.policy.to_toml(),
            )
                .into_response()
        })
    } else {
        respond(StatusCode::OK, reply)
    }
}

/// A policy from a request body: a `codoseo.toml` (`Content-Type: application/toml`, unknown
/// keys ignored) or the JSON `get_policy` returns under `policy`.
fn policy_body(
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> Result<codoseo_policy::SitePolicy, String> {
    let toml = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("toml"));
    match std::str::from_utf8(body) {
        Err(_) => Err("The body is not UTF-8 text.".to_owned()),
        Ok(text) if toml => codoseo_policy::parse_file(text)
            .map(|f| f.policy)
            .map_err(|e| codoseo_policy::error_lines(&e)),
        Ok(text) => serde_json::from_str::<codoseo_policy::SitePolicy>(text)
            .map_err(|e| format!("The body is not a policy: {e}")),
    }
}

/// What saving a policy (a body as [`put_policy`] takes) would do, without saving it.
async fn preview_policy(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path(site): Path<String>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let policy = policy_body(&headers, &body);
    respond(
        StatusCode::OK,
        AgentService::new(&state)
            .preview_policy(&caller, &site, policy)
            .await,
    )
}

/// Replaces the site's policy: a `codoseo.toml` (`Content-Type: application/toml`, unknown keys
/// ignored) or the JSON `get_policy` returns under `policy`. Answers like `get_policy`, with
/// `saved`: what the save did to the latest crawl's scores.
async fn put_policy(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path(site): Path<String>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let policy = policy_body(&headers, &body);
    respond(
        StatusCode::OK,
        AgentService::new(&state)
            .set_policy(&caller, &site, policy)
            .await,
    )
}

/// An incident id from the path. One that isn't a number matches no incident: the same 404.
fn incident_id(raw: &str) -> i64 {
    raw.trim().parse().unwrap_or(-1)
}

async fn incident_fix(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path((site, id)): Path<(String, String)>,
) -> Response {
    respond(
        StatusCode::OK,
        AgentService::new(&state)
            .incident_fix(&caller, &site, incident_id(&id))
            .await,
    )
}

async fn recheck_incident(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path((site, id)): Path<(String, String)>,
) -> Response {
    respond(
        StatusCode::ACCEPTED,
        AgentService::new(&state)
            .recheck_incident(&caller, &site, incident_id(&id))
            .await,
    )
}

/// A JSON request body, parsed but not yet judged, like [`ApiQuery`]: one that doesn't parse
/// still costs a call. An empty body is `{}`.
struct ApiBody<T>(Result<T, AgentError>);

impl<T: serde::de::DeserializeOwned> FromRequest<AppState> for ApiBody<T> {
    type Rejection = std::convert::Infallible;

    async fn from_request(req: Request, state: &AppState) -> Result<ApiBody<T>, Self::Rejection> {
        let bytes = Bytes::from_request(req, state).await.unwrap_or_default();
        let bytes = if bytes.iter().all(u8::is_ascii_whitespace) {
            Bytes::from_static(b"{}")
        } else {
            bytes
        };
        Ok(ApiBody(serde_json::from_slice(&bytes).map_err(|_| {
            AgentError::BadRequest("The body must be JSON like {\"days\": 7}.".to_owned())
        })))
    }
}

#[derive(Deserialize)]
struct SnoozeBody {
    /// Anything but a whole number is refused with the same message as a wrong number.
    days: Option<serde_json::Value>,
}

async fn snooze_incident(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path((site, id)): Path<(String, String)>,
    body: ApiBody<SnoozeBody>,
) -> Response {
    let service = AgentService::new(&state);
    let (svc, who, site) = (&service, &caller, &site);
    counted(&service, &caller, ApiQuery(body.0), move |b| async move {
        let days = b.days.and_then(|d| d.as_i64());
        svc.snooze_incident(who, site, incident_id(&id), days).await
    })
    .await
}

async fn unsnooze_incident(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path((site, id)): Path<(String, String)>,
) -> Response {
    respond(
        StatusCode::OK,
        AgentService::new(&state)
            .unsnooze_incident(&caller, &site, incident_id(&id))
            .await,
    )
}

async fn accept_incident(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path((site, id)): Path<(String, String)>,
) -> Response {
    respond(
        StatusCode::OK,
        AgentService::new(&state)
            .accept_incident(&caller, &site, incident_id(&id))
            .await,
    )
}

async fn unaccept_incident(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path((site, id)): Path<(String, String)>,
) -> Response {
    respond(
        StatusCode::OK,
        AgentService::new(&state)
            .unaccept_incident(&caller, &site, incident_id(&id))
            .await,
    )
}

#[derive(Deserialize)]
struct ExportQuery {
    format: Option<String>,
}

/// The incident export, as JSON (default) or CSV; the same rows as the app's download.
async fn incident_export(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path(site): Path<String>,
    q: ApiQuery<ExportQuery>,
) -> Response {
    let service = AgentService::new(&state);
    let format = match q.0 {
        Ok(q) => match q.format.as_deref().map(str::trim) {
            None | Some("") | Some("json") => Ok(false),
            Some("csv") => Ok(true),
            Some(_) => Err(AgentError::BadRequest(
                "format must be csv or json.".to_owned(),
            )),
        },
        Err(e) => Err(e),
    };
    match format {
        Err(e) => respond(
            StatusCode::OK,
            service
                .refuse::<Vec<ai_incident::IncidentRecord>>(&caller, e)
                .await,
        ),
        Ok(csv) => respond_with(
            StatusCode::OK,
            service.incident_export(&caller, &site).await,
            move |status, records| {
                if !csv {
                    return (status, Json(records)).into_response();
                }
                match ai_incident::to_csv(&records, super::export::text) {
                    Ok(body) => (
                        status,
                        [(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static("text/csv; charset=utf-8"),
                        )],
                        body,
                    )
                        .into_response(),
                    Err(e) => AgentError::Internal(e).into_response(),
                }
            },
        ),
    }
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

/// A deploy finished: crawl now (or answer with the crawl that already covers it). Any request
/// body (a platform's deploy payload) is ignored.
async fn deploy(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path(site): Path<String>,
) -> Response {
    respond(
        StatusCode::ACCEPTED,
        AgentService::new(&state).deploy(&caller, &site).await,
    )
}

/// The same for a platform that can't send a header: the site's secret URL authenticates.
async fn deploy_by_secret(State(state): State<AppState>, Path(secret): Path<String>) -> Response {
    respond(
        StatusCode::ACCEPTED,
        AgentService::new(&state).deploy_by_secret(&secret).await,
    )
}

async fn create_deploy_hook(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path(site): Path<String>,
) -> Response {
    respond(
        StatusCode::CREATED,
        AgentService::new(&state)
            .create_deploy_hook(&caller, &site)
            .await,
    )
}

async fn deploy_hook_status(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path(site): Path<String>,
) -> Response {
    respond(
        StatusCode::OK,
        AgentService::new(&state)
            .deploy_hook_status(&caller, &site)
            .await,
    )
}

async fn delete_deploy_hook(
    State(state): State<AppState>,
    caller: ApiCaller,
    Path(site): Path<String>,
) -> Response {
    respond(
        StatusCode::OK,
        AgentService::new(&state)
            .delete_deploy_hook(&caller, &site)
            .await,
    )
}

/// Not counted against the allowance.
async fn usage(State(state): State<AppState>, caller: ApiCaller) -> Response {
    respond(
        StatusCode::OK,
        AgentService::new(&state).usage(&caller).await,
    )
}
