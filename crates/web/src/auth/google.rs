//! "Continue with Google": Google's OAuth 2.0 / OpenID Connect web flow (see [`super::oauth`] for
//! the `state` cookie). We ask only for `openid email`, read the user's `sub` and verified email
//! from the userinfo endpoint, and sign in with that address, so a Google login and a magic link
//! for the same address are one account.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, header};
use axum::response::Response;
use serde::Deserialize;

use super::oauth;
use crate::error::AppError;
use crate::state::AppState;

const CALLBACK_PATH: &str = "/auth/google/callback";

#[derive(Deserialize)]
pub struct StartQuery {
    next: Option<String>,
}

pub async fn start(
    State(state): State<AppState>,
    Query(q): Query<StartQuery>,
) -> Result<Response, AppError> {
    let g = state.config.google.as_ref().ok_or(AppError::NotFound)?;
    let (nonce, cookie) = oauth::begin(&state, q.next.as_deref());
    let mut url = g.authorize_url.clone();
    url.query_pairs_mut()
        .append_pair("client_id", &g.client_id)
        .append_pair("redirect_uri", &oauth::callback_url(&state, CALLBACK_PATH))
        .append_pair("response_type", "code")
        .append_pair("scope", "openid email")
        // People often have a work and a personal Google account: let them pick.
        .append_pair("prompt", "select_account")
        .append_pair("state", &nonce);
    Ok(oauth::start_response(cookie, url.as_str()))
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct UserInfo {
    sub: String,
    email: Option<String>,
    #[serde(default)]
    email_verified: bool,
}

pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    let g = state.config.google.as_ref().ok_or(AppError::NotFound)?;
    if q.error.is_some() {
        return Ok(oauth::cancelled(&state));
    }
    let next = oauth::verify(&headers, q.state.as_deref(), "Google")?;
    let code = q
        .code
        .ok_or_else(|| AppError::BadRequest("Google didn't send a code.".to_owned()))?;

    let token: TokenResponse = state
        .http
        .post(g.token_url.clone())
        .header(header::ACCEPT, "application/json")
        .form(&[
            ("client_id", g.client_id.as_str()),
            ("client_secret", g.client_secret.as_str()),
            ("code", code.as_str()),
            ("grant_type", "authorization_code"),
            (
                "redirect_uri",
                oauth::callback_url(&state, CALLBACK_PATH).as_str(),
            ),
        ])
        .send()
        .await
        .map_err(AppError::internal)?
        .json()
        .await
        .map_err(AppError::internal)?;
    let access = token.access_token.ok_or_else(|| {
        AppError::BadRequest(
            token
                .error_description
                .unwrap_or_else(|| "Google refused the sign-in.".to_owned()),
        )
    })?;

    let user: UserInfo = state
        .http
        .get(g.userinfo_url.clone())
        .bearer_auth(&access)
        .send()
        .await
        .map_err(AppError::internal)?
        .error_for_status()
        .map_err(AppError::internal)?
        .json()
        .await
        .map_err(AppError::internal)?;
    let address = user.email.filter(|_| user.email_verified).ok_or_else(|| {
        AppError::BadRequest(
            "Your Google account has no verified email address. Use an email link instead."
                .to_owned(),
        )
    })?;

    oauth::finish(&state, &address, None, Some(&user.sub), &next).await
}
