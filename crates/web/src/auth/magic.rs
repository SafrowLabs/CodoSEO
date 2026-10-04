//! Magic links: a 32-byte random token, stored hashed, valid for 15 minutes, usable once.
//!
//! The emailed link opens a small confirm page that POSTs the token back. Mail scanners that
//! prefetch every link in an email only issue a GET, so they can't burn the single-use token
//! before the person clicks it.

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Redirect, Response};
use codoseo_store::accounts::{SignIn, SignInOutcome};
use codoseo_store::auth::TokenPurpose;
use serde::Deserialize;

use super::{CurrentUser, email, safe_next, session, signup_policy};
use crate::assets;
use crate::auth::mailer::Email;
use crate::error::AppError;
use crate::render::{Hx, html};
use crate::state::AppState;

pub const MAGIC_TTL: time::Duration = time::Duration::minutes(15);

/// What the login card shows.
pub struct Card {
    pub email: String,
    pub next: String,
    pub error: Option<String>,
    /// The link was sent: show "check your email" instead of the form.
    pub sent: bool,
    pub github: bool,
    pub dev_hint: bool,
}

#[derive(Template)]
#[template(path = "auth/login.html")]
pub struct LoginPage {
    pub card: Card,
}

#[derive(Template)]
#[template(path = "auth/card_partial.html")]
pub struct CardPartial {
    pub card: Card,
}

#[derive(Deserialize)]
pub struct LoginQuery {
    next: Option<String>,
}

pub async fn login_page(
    State(state): State<AppState>,
    user: Option<CurrentUser>,
    Query(q): Query<LoginQuery>,
) -> Result<Response, AppError> {
    let next = safe_next(q.next.as_deref()).to_owned();
    if user.is_some() {
        return Ok(Redirect::to(&next).into_response());
    }
    Ok(html(&LoginPage {
        card: card(&state, String::new(), next, None, false),
    })?
    .into_response())
}

fn card(state: &AppState, email: String, next: String, error: Option<String>, sent: bool) -> Card {
    Card {
        email,
        next,
        error,
        sent,
        github: state.config.github.is_some(),
        dev_hint: state.config.mode == crate::config::Mode::SelfHost,
    }
}

#[derive(Deserialize)]
pub struct LoginForm {
    email: String,
    next: Option<String>,
}

pub async fn request_link(
    State(state): State<AppState>,
    hx: Hx,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let next = safe_next(form.next.as_deref()).to_owned();
    let respond = |c: Card| -> Result<Response, AppError> {
        Ok(if hx.request {
            html(&CardPartial { card: c })?.into_response()
        } else {
            html(&LoginPage { card: c })?.into_response()
        })
    };

    let Some(address) = email::parse(&form.email) else {
        let err = Some("That doesn't look like an email address.".to_owned());
        return respond(card(&state, form.email.trim().to_owned(), next, err, false));
    };

    let token = session::random_token();
    codoseo_store::auth::create_token(
        &state.pool,
        TokenPurpose::MagicLink,
        &session::hash(&token),
        None,
        Some(serde_json::json!({ "email": address, "next": next })),
        MAGIC_TTL,
    )
    .await?;

    let mut link = state.config.base_url.clone();
    link.set_path(&format!("/auth/magic/{token}"));
    state
        .mailer
        .send(Email {
            to: address.to_owned(),
            subject: "Your CodoSEO sign-in link".to_owned(),
            text: format!(
                "Sign in to CodoSEO:\n\n{link}\n\nThe link works once and expires in 15 minutes. \
                 If you didn't ask for it, you can ignore this email."
            ),
        })
        .await;

    respond(card(&state, address.to_owned(), next, None, true))
}

#[derive(Template)]
#[template(path = "auth/confirm.html")]
pub struct ConfirmPage {
    pub token: String,
}

pub async fn confirm_page(Path(token): Path<String>) -> Result<Response, AppError> {
    Ok(html(&ConfirmPage { token })?.into_response())
}

pub async fn consume(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let used = codoseo_store::auth::consume_token(
        &state.pool,
        TokenPurpose::MagicLink,
        &session::hash(&token),
    )
    .await?
    .ok_or_else(|| {
        AppError::BadRequest(
            "This sign-in link has expired or was already used. Ask for a new one.".to_owned(),
        )
    })?;

    let payload = used.payload.unwrap_or_default();
    let address = payload["email"]
        .as_str()
        .ok_or_else(|| AppError::internal("magic link without an email"))?;
    let next = safe_next(payload["next"].as_str()).to_owned();
    let canonical = email::canonical(address);
    let outcome = codoseo_store::accounts::sign_in(
        &state.pool,
        &SignIn {
            email: address,
            canonical: &canonical,
            github_id: None,
        },
        signup_policy(&state),
    )
    .await?;
    let account = match outcome {
        SignInOutcome::Existing(a) | SignInOutcome::Created(a) => a,
        SignInOutcome::SignupsClosed => return Err(signups_closed()),
    };
    let cookie = session::start(&state, account.id).await?;
    Ok(([(header::SET_COOKIE, cookie)], Redirect::to(&next)).into_response())
}

pub fn signups_closed() -> AppError {
    AppError::Forbidden(
        "Signups are closed on this CodoSEO instance. Ask its owner to let you in.".to_owned(),
    )
}

/// Template helper so auth pages can reference assets without the app shell.
pub fn asset(name: &str) -> &'static str {
    assets::url(name)
}
