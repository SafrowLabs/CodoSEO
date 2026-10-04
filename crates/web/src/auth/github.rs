//! "Sign in with GitHub": the OAuth web flow. A random `state` value in a short-lived cookie
//! ties the callback to the browser that started it. The account email is the user's primary
//! verified GitHub email, so a GitHub login and a magic link for that address are one account.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, header};
use axum::response::{AppendHeaders, IntoResponse, Redirect, Response};
use codoseo_store::accounts::{SignIn, SignInOutcome};
use serde::Deserialize;

use super::magic::signups_closed;
use super::{email, safe_next, session, signup_policy};
use crate::error::AppError;
use crate::state::AppState;

const STATE_COOKIE: &str = "codoseo_oauth";
const STATE_TTL_SECS: i64 = 600;

#[derive(Deserialize)]
pub struct StartQuery {
    next: Option<String>,
}

fn callback_url(state: &AppState) -> String {
    let mut u = state.config.base_url.clone();
    u.set_path("/auth/github/callback");
    u.to_string()
}

pub async fn start(
    State(state): State<AppState>,
    Query(q): Query<StartQuery>,
) -> Result<Response, AppError> {
    let gh = state.config.github.as_ref().ok_or(AppError::NotFound)?;
    let nonce = session::random_token();
    let next = safe_next(q.next.as_deref());
    let mut url = gh.authorize_url.clone();
    url.query_pairs_mut()
        .append_pair("client_id", &gh.client_id)
        .append_pair("redirect_uri", &callback_url(&state))
        .append_pair("scope", "read:user user:email")
        .append_pair("state", &nonce);
    // The cookie carries the nonce and where to go afterwards: `<nonce>.<urlencoded next>`.
    let cookie = session::set_cookie(
        STATE_COOKIE,
        &format!("{nonce}.{}", super::urlencode(next)),
        STATE_TTL_SECS,
        state.config.secure_cookies(),
    );
    Ok(([(header::SET_COOKIE, cookie)], Redirect::to(url.as_str())).into_response())
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
        return Ok(Redirect::to("/login").into_response());
    }
    let cookie = session::cookie(&headers, STATE_COOKIE).unwrap_or_default();
    let (nonce, next) = cookie.split_once('.').unwrap_or(("", ""));
    let next: String = url::form_urlencoded::parse(format!("n={next}").as_bytes())
        .find(|(k, _)| k == "n")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
    let sent_state = q.state.unwrap_or_default();
    if nonce.is_empty() || !constant_time_eq(nonce.as_bytes(), sent_state.as_bytes()) {
        return Err(AppError::BadRequest(
            "GitHub sign-in expired. Please try again.".to_owned(),
        ));
    }
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
            ("redirect_uri", callback_url(&state).as_str()),
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
    let canonical = email::canonical(&address);
    let outcome = codoseo_store::accounts::sign_in(
        &state.pool,
        &SignIn {
            email: &address,
            canonical: &canonical,
            github_id: Some(&github_id),
        },
        signup_policy(&state),
    )
    .await?;
    let account = match outcome {
        SignInOutcome::Existing(a) | SignInOutcome::Created(a) => a,
        SignInOutcome::SignupsClosed => return Err(signups_closed()),
    };
    let session_cookie = session::start(&state, account.id).await?;
    let clear = session::set_cookie(STATE_COOKIE, "", 0, state.config.secure_cookies());
    // Two Set-Cookie headers: an array of pairs would insert, keeping only the last one.
    Ok((
        AppendHeaders([
            (header::SET_COOKIE, session_cookie),
            (header::SET_COOKIE, clear),
        ]),
        Redirect::to(safe_next(Some(&next))),
    )
        .into_response())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
