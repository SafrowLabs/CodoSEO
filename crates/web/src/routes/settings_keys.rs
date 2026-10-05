//! `/settings/api-keys`: the keys agents use for the REST API and the cloud MCP server, today's
//! API usage and how to connect. A key is shown once, on the response that creates it.

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderName, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use codoseo_core::plan::PlanLimits;
use codoseo_store::api_keys::{self, ApiKey, CreateKeyOutcome};
use serde::Deserialize;
use uuid::Uuid;

use crate::agent::keys;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::fmt;
use crate::layout::{Screen, Shell};
use crate::render::{Hx, html};
use crate::state::AppState;

/// The longest key name.
const NAME_MAX: usize = 60;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/settings/api-keys", get(page).post(create))
        .route("/settings/api-keys/{id}/revoke", post(revoke))
}

// ---- views -----------------------------------------------------------------------------------

pub struct KeyView {
    pub id: Uuid,
    pub name: String,
    pub prefix: String,
    pub created: String,
    pub last_used: String,
}

/// A key just created: the only time it is shown.
pub struct ShownKey {
    pub name: String,
    pub key: String,
    /// The Claude Code command with this key filled in.
    pub command: String,
}

#[derive(Template)]
#[template(path = "settings/api_keys_form.html")]
pub struct CreateForm {
    pub name: String,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "settings/api_keys.html")]
pub struct KeysPage {
    pub shell: Shell,
    pub keys: Vec<KeyView>,
    pub form: CreateForm,
    pub new_key: Option<ShownKey>,
    /// `37 of 100 API calls used today`, or the unlimited wording.
    pub usage: String,
    /// The `/mcp` address.
    pub mcp_url: String,
    pub max_keys: i64,
}

fn key_view(k: &ApiKey) -> KeyView {
    KeyView {
        id: k.id,
        name: k.name.clone(),
        prefix: k.prefix.clone(),
        created: fmt::date(k.created_at),
        last_used: k
            .last_used_at
            .map_or_else(|| "never used".to_owned(), fmt::ago),
    }
}

fn mcp_url(state: &AppState) -> Result<String, AppError> {
    Ok(state
        .config
        .base_url
        .join("mcp")
        .map_err(AppError::internal)?
        .to_string())
}

/// `claude mcp add ...` for the key (or the placeholder).
fn command(mcp_url: &str, key: &str) -> String {
    format!(
        "claude mcp add --transport http codoseo {mcp_url} --header \"Authorization: Bearer {key}\""
    )
}

async fn usage_line(state: &AppState, user: &CurrentUser) -> Result<String, AppError> {
    let used = fmt::thousands(api_keys::usage_today(&state.pool, user.id()).await?);
    Ok(
        match PlanLimits::for_plan(user.account.plan).api_calls_per_day {
            Some(limit) => format!("{used} of {} API calls used today", fmt::thousands(limit)),
            None => format!("Unlimited API calls, {used} used today"),
        },
    )
}

async fn render_page(
    state: &AppState,
    user: &CurrentUser,
    form: CreateForm,
    new_key: Option<ShownKey>,
) -> Result<Html<String>, AppError> {
    let listed = api_keys::list_for_account(&state.pool, user.id()).await?;
    let shell = Shell::load(state, user, None, Screen::ApiKeys).await?;
    html(&KeysPage {
        shell,
        keys: listed.iter().map(key_view).collect(),
        form,
        new_key,
        usage: usage_line(state, user).await?,
        mcp_url: mcp_url(state)?,
        max_keys: api_keys::MAX_LIVE_KEYS,
    })
}

fn blank_form() -> CreateForm {
    CreateForm {
        name: String::new(),
        error: None,
    }
}

async fn page(State(state): State<AppState>, user: CurrentUser) -> Result<Response, AppError> {
    Ok(render_page(&state, &user, blank_form(), None)
        .await?
        .into_response())
}

// ---- create and revoke -----------------------------------------------------------------------

#[derive(Deserialize)]
struct NewKeyForm {
    #[serde(default)]
    name: String,
}

enum Refusal {
    Invalid(String),
    Limit(String),
}

async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    hx: Hx,
    Form(form): Form<NewKeyForm>,
) -> Result<Response, AppError> {
    let name = form.name.trim();
    let refusal = match try_create(&state, &user, name).await? {
        Ok(page) => return Ok(page),
        Err(refusal) => refusal,
    };
    let (status, message) = match &refusal {
        Refusal::Invalid(m) => (StatusCode::BAD_REQUEST, m.clone()),
        Refusal::Limit(m) => (StatusCode::FORBIDDEN, m.clone()),
    };
    if !hx.request {
        return Err(match refusal {
            Refusal::Invalid(m) => AppError::BadRequest(m),
            Refusal::Limit(m) => AppError::Limit(m),
        });
    }
    Ok((
        status,
        [
            (HeaderName::from_static("hx-retarget"), "#add-key"),
            (HeaderName::from_static("hx-reswap"), "outerHTML"),
        ],
        html(&CreateForm {
            name: name.to_owned(),
            error: Some(message),
        })?,
    )
        .into_response())
}

/// Makes the key and answers with the page that shows it once.
async fn try_create(
    state: &AppState,
    user: &CurrentUser,
    name: &str,
) -> Result<Result<Response, Refusal>, AppError> {
    if name.is_empty() || name.chars().count() > NAME_MAX || name.chars().any(char::is_control) {
        return Ok(Err(Refusal::Invalid(format!(
            "Give the key a name of 1 to {NAME_MAX} characters, with no line breaks or control characters."
        ))));
    }
    let key = keys::generate();
    let outcome = api_keys::create(
        &state.pool,
        user.id(),
        name,
        &key.hash,
        &key.prefix,
        api_keys::MAX_LIVE_KEYS,
    )
    .await?;
    if outcome == CreateKeyOutcome::LimitReached {
        return Ok(Err(Refusal::Limit(format!(
            "You can have up to {} API keys. Revoke one you don't use to create another.",
            api_keys::MAX_LIVE_KEYS
        ))));
    }
    let command = command(&mcp_url(state)?, &key.plaintext);
    let page = render_page(
        state,
        user,
        blank_form(),
        Some(ShownKey {
            name: name.to_owned(),
            key: key.plaintext,
            command,
        }),
    )
    .await?;
    // The key is on this page: htmx must not keep it in a history snapshot (the template says
    // `hx-history="false"`), the URL goes back to the plain screen, and nothing may cache it.
    Ok(Ok((
        [
            (
                HeaderName::from_static("hx-replace-url"),
                "/settings/api-keys",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        page,
    )
        .into_response()))
}

async fn revoke(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Response, AppError> {
    if !api_keys::revoke(&state.pool, user.id(), id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Redirect::to("/settings/api-keys").into_response())
}
