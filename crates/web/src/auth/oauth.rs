//! The parts of an OAuth web sign-in that GitHub and Google share: the `state` cookie that ties
//! the callback to the browser that started it, and finishing the sign-in once the provider has
//! told us who the user is.

use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{AppendHeaders, IntoResponse, Redirect, Response};
use codoseo_store::accounts::{SignIn, SignInOutcome};
use uuid::Uuid;

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

/// A fresh `state` value and the cookie that remembers it, with where to go afterwards and, for a
/// sign-in started from the no-signup audit, which audit to attach.
pub(super) fn begin(
    state: &AppState,
    next: Option<&str>,
    audit: Option<Uuid>,
) -> (String, HeaderValue) {
    let nonce = session::random_token();
    let next = safe_next(next);
    // `<nonce>.<audit id or empty>.<urlencoded next>`: neither the nonce (URL-safe base64) nor a
    // UUID holds a dot, so the first two dots split it.
    let audit = audit.map(|a| a.to_string()).unwrap_or_default();
    let cookie = session::set_cookie(
        STATE_COOKIE,
        &format!("{nonce}.{audit}.{}", super::urlencode(next)),
        STATE_TTL_SECS,
        state.config.secure_cookies(),
    );
    (nonce, cookie)
}

pub(super) fn start_response(cookie: HeaderValue, to: &str) -> Response {
    ([(header::SET_COOKIE, cookie)], Redirect::to(to)).into_response()
}

/// The person cancelled or the provider refused: back to the login page, and the `state` cookie
/// is spent either way.
pub(super) fn cancelled(state: &AppState) -> Response {
    let clear = session::set_cookie(STATE_COOKIE, "", 0, state.config.secure_cookies());
    ([(header::SET_COOKIE, clear)], Redirect::to("/login")).into_response()
}

/// Where a sign-in goes once the provider has answered.
pub(super) struct After {
    pub next: String,
    /// The no-signup audit to attach to the account.
    pub audit: Option<Uuid>,
}

/// Checks the `state` the provider sent back against the cookie and returns where to go next.
pub(super) fn verify(
    headers: &HeaderMap,
    sent_state: Option<&str>,
    provider: &str,
) -> Result<After, AppError> {
    let cookie = session::cookie(headers, STATE_COOKIE).unwrap_or_default();
    let mut parts = cookie.splitn(3, '.');
    let (nonce, audit, next) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
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
    Ok(After {
        next,
        audit: Uuid::parse_str(audit).ok(),
    })
}

/// Signs in (or up) with a verified address from the provider and redirects to `next`, or, for a
/// sign-in started from the no-signup audit, attaches that audit and goes to its site.
pub(super) async fn finish(
    state: &AppState,
    headers: &HeaderMap,
    address: &str,
    github_id: Option<&str>,
    google_id: Option<&str>,
    after: &After,
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
    let Some(audit) = after.audit else {
        // Two Set-Cookie headers: an array of pairs would insert, keeping only the last one.
        return Ok((
            AppendHeaders([
                (header::SET_COOKIE, session_cookie),
                (header::SET_COOKIE, clear),
            ]),
            Redirect::to(safe_next(Some(&after.next))),
        )
            .into_response());
    };
    // The claim cookie in `headers` decides whether the audit itself moves into the account or a
    // fresh crawl of its domain starts, exactly as for the emailed link.
    let to = crate::routes::quick::attach_after_login(state, &account, audit, headers).await?;
    let spent = crate::routes::quick::clear_claim_cookie(state);
    Ok((
        AppendHeaders([
            (header::SET_COOKIE, session_cookie),
            (header::SET_COOKIE, clear),
            (header::SET_COOKIE, spent),
        ]),
        Redirect::to(&to),
    )
        .into_response())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
