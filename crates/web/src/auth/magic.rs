//! Magic links: a 32-byte random token, stored hashed, valid for 15 minutes, usable once.
//!
//! The emailed link opens a small confirm page that POSTs the token back. Mail scanners that
//! prefetch every link in an email only issue a GET, so they can't burn the single-use token
//! before the person clicks it.

use std::net::IpAddr;

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{AppendHeaders, IntoResponse, Redirect, Response};
use codoseo_store::accounts::{SignIn, SignInOutcome};
use codoseo_store::auth::TokenPurpose;
use codoseo_store::quick::{self, Requester, UnlockCaps, UnlockSlot};
use codoseo_store::suppressions::{self, Scope};
use serde::Deserialize;
use uuid::Uuid;

use super::{CurrentUser, email, safe_next, session, signup_policy};
use crate::abuse::{self, ClientIp};
use crate::assets;
use crate::error::AppError;
use crate::render::{Hx, html};
use crate::routes::legal::CONTACT;
use crate::state::AppState;
use crate::turnstile::{self, Action, Verdict};
use crate::unsubscribe::Unsubscribe;
use codoseo_notify::letters;

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
    /// On the cloud: signing in accepts the Terms and Privacy Policy, so the card links them.
    pub legal: bool,
    /// Turnstile's site key, when the form is gated.
    pub turnstile_site_key: Option<String>,
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
        legal: state.config.mode == crate::config::Mode::Cloud,
        turnstile_site_key: state.config.turnstile.as_ref().map(|t| t.site_key.clone()),
    }
}

#[derive(Deserialize)]
pub struct LoginForm {
    email: String,
    next: Option<String>,
    /// Added to the form by Cloudflare's Turnstile script.
    #[serde(rename = "cf-turnstile-response", default)]
    turnstile: Option<String>,
}

/// What the form says when a sign-in email was not sent.
pub fn throttle_text(slot: UnlockSlot) -> &'static str {
    match slot {
        UnlockSlot::AddressCapReached | UnlockSlot::AddressDayCapReached => {
            "We've already sent several emails to that address recently. Check your inbox and \
             spam folder, or try again later."
        }
        UnlockSlot::IpCapReached => {
            "Several sign-in links were asked for from your connection in the last hour, which \
             is the limit. Try again in an hour."
        }
        UnlockSlot::AuditCapReached | UnlockSlot::Created => {
            "We already sent several links for this audit. Check your inbox and spam folder, or \
             try again in an hour."
        }
    }
}

pub fn blocked_text() -> String {
    format!(
        "That address asked us not to email it. If that's a mistake, write to {CONTACT} and \
         we'll lift it."
    )
}

/// The status and words for a Turnstile verdict that stops the form; `None` when it passed.
pub fn turnstile_text(verdict: Verdict) -> Option<(StatusCode, &'static str)> {
    match verdict {
        Verdict::Passed => None,
        Verdict::Failed => Some((
            StatusCode::FORBIDDEN,
            "We couldn't confirm you're a human. Reload the page and try again.",
        )),
        Verdict::Unavailable => Some((
            StatusCode::SERVICE_UNAVAILABLE,
            "Verification is unavailable right now. Please try again in a minute.",
        )),
    }
}

pub async fn request_link(
    State(state): State<AppState>,
    hx: Hx,
    ClientIp(ip): ClientIp,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let next = safe_next(form.next.as_deref()).to_owned();
    let respond = |status: StatusCode, c: Card| -> Result<Response, AppError> {
        Ok(if hx.request {
            (status, html(&CardPartial { card: c })?).into_response()
        } else {
            (status, html(&LoginPage { card: c })?).into_response()
        })
    };
    let typed = form.email.trim().to_owned();

    let Some(address) = email::parse(&form.email) else {
        let err = Some("That doesn't look like an email address.".to_owned());
        return respond(StatusCode::OK, card(&state, typed, next, err, false));
    };
    let verdict = turnstile::verify(&state, Action::Login, form.turnstile.as_deref(), ip).await;
    if let Some((status, message)) = turnstile_text(verdict) {
        let err = Some(message.to_owned());
        return respond(status, card(&state, typed, next, err, false));
    }

    match issue_link(&state, address, &next, None, ip).await? {
        LinkOutcome::Sent => respond(
            StatusCode::OK,
            card(&state, address.to_owned(), next, None, true),
        ),
        LinkOutcome::Throttled(slot) => respond(
            StatusCode::TOO_MANY_REQUESTS,
            card(
                &state,
                typed,
                next,
                Some(throttle_text(slot).to_owned()),
                false,
            ),
        ),
        LinkOutcome::Blocked => respond(
            StatusCode::OK,
            card(&state, typed, next, Some(blocked_text()), false),
        ),
    }
}

/// A no-signup audit a sign-in link should attach to the new account when it is used.
pub struct AuditLink<'a> {
    pub crawl_id: Uuid,
    pub domain: &'a str,
    /// What the audit found so far, for the email.
    pub audit: letters::Audit,
}

/// What [`issue_link`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOutcome {
    Sent,
    /// The address, the audit or the visitor's connection has had its share of sign-in emails
    /// (nothing was sent).
    Throttled(UnlockSlot),
    /// The address asked never to be emailed (nothing was sent).
    Blocked,
}

/// Stores a magic-link token for `address` and emails the link. With an `audit`, the link also
/// carries the audit's crawl id, and using it attaches the audited site to the account. Every
/// link is capped per recipient and per client address, and an audit's links per audit too
/// (see [`quick::create_unlock_token`]).
pub async fn issue_link(
    state: &AppState,
    address: &str,
    next: &str,
    audit: Option<AuditLink<'_>>,
    ip: Option<IpAddr>,
) -> Result<LinkOutcome, AppError> {
    let canonical = email::canonical(address);
    if suppressions::blocks_all(&state.pool, &canonical).await? {
        return Ok(LinkOutcome::Blocked);
    }
    let token = session::random_token();
    let token_hash = session::hash(&token);
    let hashes = ip.map(|ip| abuse::ip_hashes(&state.config.secret_key, ip));
    let requester = hashes.as_ref().map(|(today, prev)| Requester {
        ip_hash: today,
        previous_ip_hash: prev.as_deref(),
    });
    let mut payload = serde_json::json!({ "email": address, "next": next, "canonical": canonical });
    let slot = match &audit {
        None => {
            quick::create_login_token(
                &state.pool,
                &canonical,
                requester,
                &token_hash,
                payload,
                MAGIC_TTL,
                UnlockCaps::DEFAULT,
            )
            .await?
        }
        Some(a) => {
            payload["audit"] = serde_json::json!(a.crawl_id);
            quick::create_unlock_token(
                &state.pool,
                a.crawl_id,
                &canonical,
                requester,
                &token_hash,
                payload,
                MAGIC_TTL,
                UnlockCaps::DEFAULT,
            )
            .await?
        }
    };
    if slot != UnlockSlot::Created {
        return Ok(LinkOutcome::Throttled(slot));
    }

    let mut link = state.config.base_url.clone();
    link.set_path(&format!("/auth/magic/{token}"));
    let draft = match audit {
        None => letters::sign_in(&state.email_images(), link.as_str(), address),
        Some(a) => {
            // Whoever typed the address may not be its owner: the owner can stop every email.
            let opt_out = state.unsubscribe.link(
                &state.config.base_url,
                &Unsubscribe {
                    address: address.to_owned(),
                    scope: Scope::All,
                    account_id: None,
                },
            );
            letters::report_link(
                &state.email_images(),
                a.domain,
                link.as_str(),
                opt_out.as_str(),
                &a.audit,
            )
        }
    };
    let email = draft
        .email(address)
        .map_err(|e| AppError::internal(format!("the sign-in email did not render: {e}")))?;
    if let Err(e) = state.mailer.send(email).await {
        // The page says "if the address is registered we sent a link" either way; a broken mail
        // server is for the operator to see in the logs, not for the visitor to probe.
        tracing::error!(error = %e, "could not send the sign-in link");
    }
    Ok(LinkOutcome::Sent)
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
    headers: HeaderMap,
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
    // Opening a link from one of our emails counts as activity for the inactivity check.
    codoseo_store::accounts::record_email_click(&state.pool, account.id).await?;

    // A link from the no-signup audit also attaches the audited site to the account.
    let audit = payload["audit"]
        .as_str()
        .and_then(|a| Uuid::parse_str(a).ok());
    let Some(audit) = audit else {
        return Ok(([(header::SET_COOKIE, cookie)], Redirect::to(&next)).into_response());
    };
    let to = crate::routes::quick::attach_after_login(&state, &account, audit, &headers).await?;
    let spent = crate::routes::quick::clear_claim_cookie(&state);
    Ok((
        AppendHeaders([(header::SET_COOKIE, cookie), (header::SET_COOKIE, spent)]),
        Redirect::to(&to),
    )
        .into_response())
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
