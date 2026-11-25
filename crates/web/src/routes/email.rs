//! `/email/unsubscribe/{token}`: the link in the footer of every notification, and the one
//! behind a mail client's own unsubscribe button. The GET shows a page with a button (mail
//! scanners open every link, so a GET changes nothing); the POST records the suppression. A
//! one-click POST from a mail client (RFC 8058: body `List-Unsubscribe=One-Click`) gets a plain
//! 200 and no page. The token is signed ([`crate::unsubscribe`]), so it is the only credential
//! and the route is exempt from the `Origin` check.

use askama::Template;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use codoseo_store::suppressions::{self, Scope};

use crate::auth::email;
use crate::error::AppError;
use crate::render::html;
use crate::state::AppState;
use crate::unsubscribe::{PATH_PREFIX, Unsubscribe};

pub fn routes() -> Router<AppState> {
    Router::new().route(
        &format!("{PATH_PREFIX}/{{token}}"),
        get(confirm).post(unsubscribe),
    )
}

#[derive(Template)]
#[template(path = "email/unsubscribe.html")]
struct UnsubscribePage {
    view: View,
    token: String,
}

enum View {
    /// The button, for `address`; `all` when the link stops every email.
    Confirm {
        address: String,
        all: bool,
    },
    Done {
        address: String,
        all: bool,
    },
    /// A token that isn't ours (or was cut short by a mail client).
    Invalid,
}

fn page(view: View, token: String) -> Result<Response, AppError> {
    let status = match view {
        View::Invalid => StatusCode::NOT_FOUND,
        _ => StatusCode::OK,
    };
    Ok((status, html(&UnsubscribePage { view, token })?).into_response())
}

async fn confirm(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let view = match state.unsubscribe.parse(&token) {
        Some(u) => View::Confirm {
            address: u.address,
            all: u.scope == Scope::All,
        },
        None => View::Invalid,
    };
    page(view, token)
}

/// Whether a POST body is a mail client's one-click unsubscribe rather than our own page's
/// button.
fn one_click(body: &[u8]) -> bool {
    url::form_urlencoded::parse(body).any(|(k, v)| k == "List-Unsubscribe" && v == "One-Click")
}

async fn unsubscribe(
    State(state): State<AppState>,
    Path(token): Path<String>,
    body: Bytes,
) -> Result<Response, AppError> {
    let Some(u) = state.unsubscribe.parse(&token) else {
        return page(View::Invalid, token);
    };
    record(&state, &u).await?;
    if one_click(&body) {
        return Ok((StatusCode::OK, "Unsubscribed").into_response());
    }
    page(
        View::Done {
            address: u.address,
            all: u.scope == Scope::All,
        },
        token,
    )
}

/// Stores the suppression: `unsubscribe` for a notification's link, `opt_out` for the "don't
/// email me" link in a first-contact email.
async fn record(state: &AppState, u: &Unsubscribe) -> Result<(), AppError> {
    let reason = match u.scope {
        Scope::Notifications => "unsubscribe",
        Scope::All => "opt_out",
    };
    suppressions::add(&state.pool, &email::canonical(&u.address), u.scope, reason).await?;
    tracing::info!(scope = u.scope.as_str(), reason, "an address unsubscribed");
    Ok(())
}
