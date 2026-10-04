//! Login and sessions: magic links, GitHub OAuth, the session cookie, the `Origin` check, and
//! the [`CurrentUser`] extractor every signed-in route takes.

pub mod email;
pub mod github;
pub mod magic;
pub mod mailer;
pub mod origin;
pub mod session;

use axum::Router;
use axum::extract::{FromRequestParts, OptionalFromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use codoseo_store::accounts::{Account, SignupPolicy};
use codoseo_store::sites::Site;
use uuid::Uuid;

use crate::config::Mode;
use crate::error::AppError;
use crate::render::{hx_redirect, is_htmx};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login", get(magic::login_page).post(magic::request_link))
        .route(
            "/auth/magic/{token}",
            get(magic::confirm_page).post(magic::consume),
        )
        .route("/auth/github", get(github::start))
        .route("/auth/github/callback", get(github::callback))
        .route("/logout", post(logout))
}

/// The signed-in account. Taking this as a handler argument makes the route require a login.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub account: Account,
    pub session_id: Uuid,
}

impl CurrentUser {
    pub fn id(&self) -> Uuid {
        self.account.id
    }
}

/// Sends a signed-out visitor to `/login`, coming back to where they were afterwards.
pub enum AuthRejection {
    Login { htmx: bool, next: String },
    Error(AppError),
}

impl IntoResponse for AuthRejection {
    fn into_response(self) -> Response {
        match self {
            AuthRejection::Login { htmx, next } => {
                let to = format!("/login?next={}", urlencode(&next));
                if htmx {
                    (StatusCode::OK, [hx_redirect(&to)]).into_response()
                } else {
                    Redirect::to(&to).into_response()
                }
            }
            AuthRejection::Error(e) => e.into_response(),
        }
    }
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AuthRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<CurrentUser, AuthRejection> {
        let login = || AuthRejection::Login {
            htmx: is_htmx(&parts.headers),
            next: current_path(
                &parts.headers,
                parts.uri.path_and_query().map(|p| p.as_str()),
            ),
        };
        let Some(token) = session::cookie(&parts.headers, session::SESSION_COOKIE) else {
            return Err(login());
        };
        match codoseo_store::auth::find_session(&state.pool, &session::hash(&token)).await {
            Ok(Some((session_id, account))) => Ok(CurrentUser {
                account,
                session_id,
            }),
            Ok(None) => Err(login()),
            Err(e) => Err(AuthRejection::Error(e.into())),
        }
    }
}

/// `Option<CurrentUser>`: `None` when signed out, for pages that work either way.
impl OptionalFromRequestParts<AppState> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<CurrentUser>, AppError> {
        match <CurrentUser as FromRequestParts<AppState>>::from_request_parts(parts, state).await {
            Ok(user) => Ok(Some(user)),
            Err(AuthRejection::Login { .. }) => Ok(None),
            Err(AuthRejection::Error(e)) => Err(e),
        }
    }
}

/// Where to return after login. For an htmx request that is the page the user is on
/// (`HX-Current-URL`), not the fragment URL.
fn current_path(headers: &HeaderMap, uri: Option<&str>) -> String {
    let from_hx = headers
        .get("hx-current-url")
        .and_then(|v| v.to_str().ok())
        .and_then(|u| url::Url::parse(u).ok())
        .map(|u| match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_owned(),
        });
    safe_next(from_hx.as_deref().or(uri)).to_owned()
}

/// Only same-site relative paths are allowed as a post-login destination, so `?next=` can't
/// be used to bounce a user to another site. Browsers drop tabs and newlines from a redirect
/// address (`/\t/evil.com` becomes `//evil.com`), so any whitespace or control character
/// rejects the value outright; it also keeps the `Location` header valid.
pub fn safe_next(next: Option<&str>) -> &str {
    match next {
        Some(n)
            if n.starts_with('/')
                && !n.starts_with("//")
                && !n.contains('\\')
                && !n.chars().any(|c| c.is_control() || c.is_whitespace()) =>
        {
            n
        }
        _ => "/",
    }
}

pub fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// The policy for creating accounts in this mode.
pub fn signup_policy(state: &AppState) -> SignupPolicy {
    SignupPolicy {
        self_hosted: state.config.mode == Mode::SelfHost,
    }
}

/// One of the user's sites, or a 404 (another account's site looks exactly like a missing one).
pub async fn load_site(
    state: &AppState,
    user: &CurrentUser,
    site_id: Uuid,
) -> Result<Site, AppError> {
    codoseo_store::sites::get_for_account(&state.pool, user.id(), site_id)
        .await?
        .ok_or(AppError::NotFound)
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, AppError> {
    if let Some(token) = session::cookie(&headers, session::SESSION_COOKIE) {
        codoseo_store::auth::revoke_session(&state.pool, &session::hash(&token)).await?;
    }
    let clear = session::set_cookie(
        session::SESSION_COOKIE,
        "",
        0,
        state.config.secure_cookies(),
    );
    Ok(([(header::SET_COOKIE, clear)], Redirect::to("/login")).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_must_be_a_local_path() {
        assert_eq!(safe_next(Some("/s/1/audit?x=1")), "/s/1/audit?x=1");
        assert_eq!(safe_next(Some("//evil.com")), "/");
        assert_eq!(safe_next(Some("https://evil.com")), "/");
        assert_eq!(safe_next(Some("/\\evil.com")), "/");
        assert_eq!(safe_next(None), "/");
        // Browsers strip these, turning the path into `//evil.com`.
        for sneaky in [
            "/\t/evil.com",
            "/\n/evil.com",
            "/\r/evil.com",
            "/ /evil.com",
            "/\u{0}x",
        ] {
            assert_eq!(safe_next(Some(sneaky)), "/", "{sneaky:?}");
        }
    }
}
