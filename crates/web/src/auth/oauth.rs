//! The parts of an OAuth web sign-in that GitHub and Google share: the `state` cookie that ties
//! the callback to the browser that started it, and finishing the sign-in once the provider has
//! told us who the user is.

use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{AppendHeaders, IntoResponse, Redirect, Response};
use codoseo_store::accounts::{SignIn, SignInOutcome};

use super::magic::signups_closed;
use super::{email, safe_next, session, signup_policy};
use crate::error::AppError;
use crate::state::AppState;

const STATE_COOKIE: &str = "codoseo_oauth";
const STATE_TTL_SECS: i64 = 600;

/// `<BASE_URL><path>`, the redirect URI registered with the provider.
pub(super) fn callback_url(state: &AppState, path: &str) -> String {
    let mut u = state.config.base_url.clone();
    u.set_path(path);
    u.to_string()
}

/// A fresh `state` value and the cookie that remembers it, with where to go afterwards.
pub(super) fn begin(state: &AppState, next: Option<&str>) -> (String, HeaderValue) {
    let nonce = session::random_token();
    let next = safe_next(next);
    // The cookie carries the nonce and where to go afterwards: `<nonce>.<urlencoded next>`.
    let cookie = session::set_cookie(
        STATE_COOKIE,
        &format!("{nonce}.{}", super::urlencode(next)),
        STATE_TTL_SECS,
        state.config.secure_cookies(),
    );
    (nonce, cookie)
}

pub(super) fn start_response(cookie: HeaderValue, to: &str) -> Response {
    ([(header::SET_COOKIE, cookie)], Redirect::to(to)).into_response()
}

/// Checks the `state` the provider sent back against the cookie and returns the `next` path.
pub(super) fn verify(
    headers: &HeaderMap,
    sent_state: Option<&str>,
    provider: &str,
) -> Result<String, AppError> {
    let cookie = session::cookie(headers, STATE_COOKIE).unwrap_or_default();
    let (nonce, next) = cookie.split_once('.').unwrap_or(("", ""));
    let next: String = url::form_urlencoded::parse(format!("n={next}").as_bytes())
        .find(|(k, _)| k == "n")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
    let sent_state = sent_state.unwrap_or_default();
    if nonce.is_empty() || !constant_time_eq(nonce.as_bytes(), sent_state.as_bytes()) {
        return Err(AppError::BadRequest(format!(
            "{provider} sign-in expired. Please try again."
        )));
    }
    Ok(next)
}

/// Signs in (or up) with a verified address from the provider and redirects to `next`.
pub(super) async fn finish(
    state: &AppState,
    address: &str,
    github_id: Option<&str>,
    google_id: Option<&str>,
    next: &str,
) -> Result<Response, AppError> {
    let canonical = email::canonical(address);
    let outcome = codoseo_store::accounts::sign_in(
        &state.pool,
        &SignIn {
            email: address,
            canonical: &canonical,
            github_id,
            google_id,
        },
        signup_policy(state),
    )
    .await?;
    let account = match outcome {
        SignInOutcome::Existing(a) | SignInOutcome::Created(a) => a,
        SignInOutcome::SignupsClosed => return Err(signups_closed()),
    };
    let session_cookie = session::start(state, account.id).await?;
    let clear = session::set_cookie(STATE_COOKIE, "", 0, state.config.secure_cookies());
    // Two Set-Cookie headers: an array of pairs would insert, keeping only the last one.
    Ok((
        AppendHeaders([
            (header::SET_COOKIE, session_cookie),
            (header::SET_COOKIE, clear),
        ]),
        Redirect::to(safe_next(Some(next))),
    )
        .into_response())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
