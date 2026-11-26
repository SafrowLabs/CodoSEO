//! "Sign in with GitHub": the OAuth web flow (see [`super::oauth`] for the `state` cookie). The
//! account email is the user's primary verified GitHub email, so a GitHub login and a magic link
//! for that address are one account.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, header};
use axum::response::Response;
use serde::Deserialize;

use super::oauth;
use crate::error::AppError;
use crate::state::AppState;

const CALLBACK_PATH: &str = "/auth/github/callback";

#[derive(Deserialize)]
pub struct StartQuery {
    next: Option<String>,
}

pub async fn start(
    State(state): State<AppState>,
    Query(q): Query<StartQuery>,
) -> Result<Response, AppError> {
    let gh = state.config.github.as_ref().ok_or(AppError::NotFound)?;
    let (nonce, cookie) = oauth::begin(&state, q.next.as_deref(), None);
    let mut url = gh.authorize_url.clone();
    url.query_pairs_mut()
        .append_pair("client_id", &gh.client_id)
        .append_pair("redirect_uri", &oauth::callback_url(&state, CALLBACK_PATH))
        .append_pair("scope", "read:user user:email")
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
struct GithubUser {
    id: u64,
}

#[derive(Deserialize)]
struct GithubEmail {
    email: String,
    primary: bool,
    verified: bool,
}

pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    let gh = state.config.github.as_ref().ok_or(AppError::NotFound)?;
    if q.error.is_some() {
        return Ok(oauth::cancelled(&state));
    }
    let after = oauth::verify(&headers, q.state.as_deref(), "GitHub")?;
    let code = q
        .code
        .ok_or_else(|| AppError::BadRequest("GitHub didn't send a code.".to_owned()))?;

    let token: TokenResponse = state
        .http
        .post(gh.token_url.clone())
        .header(header::ACCEPT, "application/json")
        .form(&[
            ("client_id", gh.client_id.as_str()),
            ("client_secret", gh.client_secret.as_str()),
            ("code", code.as_str()),
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
                .unwrap_or_else(|| "GitHub refused the sign-in.".to_owned()),
        )
    })?;

    let api = |path: &str| {
        state
            .http
            .get(gh.api_url.join(path).expect("static path"))
            .bearer_auth(&access)
            .header(header::ACCEPT, "application/vnd.github+json")
    };
    let user: GithubUser = api("user")
        .send()
        .await
        .map_err(AppError::internal)?
        .error_for_status()
        .map_err(AppError::internal)?
        .json()
        .await
        .map_err(AppError::internal)?;
    let emails: Vec<GithubEmail> = api("user/emails")
        .send()
        .await
        .map_err(AppError::internal)?
        .error_for_status()
        .map_err(AppError::internal)?
        .json()
        .await
        .map_err(AppError::internal)?;
    let address = emails
        .iter()
        .find(|e| e.primary && e.verified)
        .or_else(|| emails.iter().find(|e| e.verified))
        .map(|e| e.email.clone())
        .ok_or_else(|| {
            AppError::BadRequest(
                "Your GitHub account has no verified email address. Use an email link instead."
                    .to_owned(),
            )
        })?;

    let github_id = user.id.to_string();
    oauth::finish(&state, &headers, &address, Some(&github_id), None, &after).await
}
