//! Two emailed links that act on a person's behalf, and only once they press a button (mail
//! scanners and link previews open every link, so a GET must change nothing):
//!
//! * `/monitoring/start/{token}` (cloud only): the link the no-key MCP tool `start_monitoring`
//!   emails. The POST signs the person in (creating a Free account when the address has none),
//!   adds the site with its weekly crawls and first crawl, and shows an API key once.
//! * `/monitoring/resume/{token}`: the link in the "Keep monitoring?" email. It does not sign the
//!   visitor in; the token is the only credential, it works once, and all it can do is un-pause
//!   the account it was issued for.

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{AppendHeaders, IntoResponse, Response};
use axum::routing::get;
use codoseo_core::plan::PlanLimits;
use codoseo_store::accounts::{SignIn, SignInOutcome};
use codoseo_store::api_keys::{self, CreateKeyOutcome};
use codoseo_store::auth::{
    TokenPurpose, consume_token, live_token_payload, token_is_live, unconsume_token,
};
use codoseo_store::events::{self, EventKind};
use codoseo_store::sites::{self, CreateOutcome, Site};
use serde_json::json;

use super::quick::require_cloud;
use super::sites::{FIRST_CRAWL_PRIORITY, schedule_for};
use crate::agent::keys;
use crate::auth::{CurrentUser, email, magic, session, signup_policy};
use crate::error::AppError;
use crate::render::html;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/monitoring/resume/{token}", get(confirm).post(resume))
        .route("/monitoring/start/{token}", get(start_confirm).post(start))
}

#[derive(Template)]
#[template(path = "monitoring/resume.html")]
struct ResumePage {
    /// `Confirm` (the button), `Resumed` (done) or `Expired`.
    view: ResumeView,
    token: String,
}

enum ResumeView {
    Confirm,
    Resumed,
    Expired,
}

/// The page behind the emailed link: a button when the token is still good, a friendly dead
/// end otherwise. Reads only.
async fn confirm(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let live = token_is_live(
        &state.pool,
        TokenPurpose::ResumeMonitoring,
        &session::hash(&token),
    )
    .await?;
    if live {
        return Ok(html(&ResumePage {
            view: ResumeView::Confirm,
            token,
        })?
        .into_response());
    }
    Ok((
        StatusCode::GONE,
        html(&ResumePage {
            view: ResumeView::Expired,
            token,
        })?,
    )
        .into_response())
}

async fn resume(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    // Using the token and lifting the pause happen together or not at all.
    let mut tx = state.pool.begin().await?;
    let account = consume_token(
        &mut *tx,
        TokenPurpose::ResumeMonitoring,
        &session::hash(&token),
    )
    .await?
    .and_then(|used| used.account_id);
    let Some(account) = account else {
        return Ok((
            StatusCode::GONE,
            html(&ResumePage {
                view: ResumeView::Expired,
                token,
            })?,
        )
            .into_response());
    };
    codoseo_store::accounts::resume_monitoring(&mut *tx, account).await?;
    tx.commit().await?;
    Ok(html(&ResumePage {
        view: ResumeView::Resumed,
        token,
    })?
    .into_response())
}

// ---- /monitoring/start/{token} ---------------------------------------------------------------

/// What the start page shows.
pub enum StartView {
    /// The button. Names the site and address the link was made for.
    Confirm {
        domain: String,
        email: String,
    },
    /// Unknown, expired or already used. A signed-in visitor gets the settings link instead of
    /// the sign-in button.
    Expired {
        signed_in: bool,
    },
    Done(Box<StartDone>),
}

/// What confirming did.
pub struct StartDone {
    pub domain: String,
    pub email: String,
    pub site: SiteNote,
    /// The site's audit screen, when the account has the site.
    pub audit_href: Option<String>,
    /// The key just minted; `None` when the account is at its key limit.
    pub key: Option<MintedKey>,
    pub key_limit: i64,
    pub mcp_url: String,
}

pub enum SiteNote {
    /// New site, first crawl queued.
    Added,
    /// The account already monitored this domain; nothing was queued.
    Existing,
    /// The plan has no room: the site was not added.
    PlanFull { max_sites: u32 },
}

/// The key as the result page shows it, once.
pub struct MintedKey {
    pub key: String,
    /// The Claude Code command with the key filled in.
    pub command: String,
    /// A `mcpServers` entry for MCP clients that take JSON.
    pub json: String,
}

#[derive(Template)]
#[template(path = "monitoring/start.html")]
struct StartPage {
    view: StartView,
    token: String,
}

/// The page behind the emailed link: a button while the token is good, a dead end otherwise.
/// Reads only.
async fn start_confirm(
    State(state): State<AppState>,
    user: Option<CurrentUser>,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    require_cloud(&state)?;
    let payload = live_token_payload(
        &state.pool,
        TokenPurpose::StartMonitoring,
        &session::hash(&token),
    )
    .await?;
    let Some(payload) = payload else {
        return expired(token, user.is_some());
    };
    let text = |k: &str| payload[k].as_str().unwrap_or_default().to_owned();
    let page = StartPage {
        view: StartView::Confirm {
            domain: text("domain"),
            email: text("email"),
        },
        token,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], html(&page)?).into_response())
}

fn expired(token: String, signed_in: bool) -> Result<Response, AppError> {
    Ok((
        StatusCode::GONE,
        [(header::CACHE_CONTROL, "no-store")],
        html(&StartPage {
            view: StartView::Expired { signed_in },
            token,
        })?,
    )
        .into_response())
}

/// Confirms: uses the token (once), signs the person in, adds the site, mints a key and shows it.
/// If anything fails after the token is used, the token is made usable again, so the link in
/// their inbox still works; what had already been done (the account, the site) is reused by the
/// retry.
async fn start(
    State(state): State<AppState>,
    user: Option<CurrentUser>,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    require_cloud(&state)?;
    let hash = session::hash(&token);
    let used = consume_token(&state.pool, TokenPurpose::StartMonitoring, &hash).await?;
    let Some(payload) = used.and_then(|u| u.payload) else {
        return expired(token, user.is_some());
    };
    match confirm_start(&state, token, &payload).await {
        Ok(response) => Ok(response),
        Err(error) => {
            tracing::error!(%error, "start-monitoring confirmation failed; making its link usable again");
            if let Err(e) = unconsume_token(&state.pool, TokenPurpose::StartMonitoring, &hash).await
            {
                tracing::error!(error = %e, "could not make the start-monitoring link usable again");
            }
            Err(error)
        }
    }
}

async fn confirm_start(
    state: &AppState,
    token: String,
    payload: &serde_json::Value,
) -> Result<Response, AppError> {
    let text = |k: &str| -> Result<String, AppError> {
        payload[k]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| AppError::internal(format!("start-monitoring token without {k}")))
    };
    let (address, start_url) = (text("email")?, text("start_url")?);
    // Defensive: a link is only ever issued for a host without a trailing dot.
    let domain = text("domain")?.trim_end_matches('.').to_owned();

    let canonical = email::canonical(&address);
    let outcome = codoseo_store::accounts::sign_in(
        &state.pool,
        &SignIn {
            email: &address,
            canonical: &canonical,
            github_id: None,
        },
        signup_policy(state),
    )
    .await?;
    let account = match outcome {
        SignInOutcome::Existing(a) | SignInOutcome::Created(a) => a,
        SignInOutcome::SignupsClosed => return Err(magic::signups_closed()),
    };
    // Opening a link from one of our emails counts as activity for the inactivity check.
    codoseo_store::accounts::record_email_click(&state.pool, account.id).await?;

    let max_sites = PlanLimits::for_plan(account.plan).max_sites;
    let owned = |sites: &[Site]| sites.iter().find(|s| s.domain == domain).cloned();
    let mut site = owned(&sites::list_for_account(&state.pool, account.id).await?);
    let note = match site {
        Some(_) => SiteNote::Existing,
        None => {
            let created = sites::create_checked(
                &state.pool,
                account.id,
                &domain,
                &start_url,
                schedule_for(account.plan),
                max_sites.map(i64::from),
                FIRST_CRAWL_PRIORITY,
                // Marks the first crawl, so the funnel counts it as an agent's.
                Some("agent"),
            )
            .await?;
            match created {
                CreateOutcome::Created(new) => {
                    super::settings_alerts::default_rules_for_site(state, account.id, new.id).await;
                    site = Some(new);
                    SiteNote::Added
                }
                // The same link used twice at once: the other request added it first.
                CreateOutcome::Duplicate => {
                    site = owned(&sites::list_for_account(&state.pool, account.id).await?);
                    SiteNote::Existing
                }
                CreateOutcome::LimitReached => SiteNote::PlanFull {
                    max_sites: max_sites.unwrap_or_default(),
                },
            }
        }
    };
    events::record(
        &state.pool,
        EventKind::LinkClicked,
        Some(account.id),
        site.as_ref().map(|s| s.id),
        Some(json!({
            "source": "agent",
            "domain": domain,
            "outcome": match note {
                SiteNote::Added => "added",
                SiteNote::Existing => "existing",
                SiteNote::PlanFull { .. } => "limit",
            },
        })),
    )
    .await?;

    let mcp_url = keys::mcp_url(&state.config)?;
    // The session first and the key last: the key is the one thing that can't be shown again,
    // so nothing that can fail comes after it but rendering the page.
    let cookie = session::start(state, account.id).await?;
    let key = keys::generate();
    let created = api_keys::create(
        &state.pool,
        account.id,
        "Agent (start_monitoring)",
        &key.hash,
        &key.prefix,
        api_keys::MAX_LIVE_KEYS,
    )
    .await?;
    let new_key_id = match &created {
        CreateKeyOutcome::Created(k) => Some(k.id),
        CreateKeyOutcome::LimitReached => None,
    };
    let shown = new_key_id.map(|_| MintedKey {
        command: keys::claude_command(&mcp_url, &key.plaintext),
        json: keys::mcp_servers_json(&mcp_url, &key.plaintext),
        key: key.plaintext,
    });

    let page = StartPage {
        view: StartView::Done(Box::new(StartDone {
            audit_href: site.as_ref().map(|s| format!("/s/{}/audit", s.id)),
            domain,
            email: address,
            site: note,
            key: shown,
            key_limit: api_keys::MAX_LIVE_KEYS,
            mcp_url,
        })),
        token,
    };
    let body = match html(&page) {
        Ok(body) => body,
        Err(error) => {
            // A key nobody saw is worthless: take it back, so the retry mints the one shown.
            if let Some(id) = new_key_id
                && let Err(e) = api_keys::revoke(&state.pool, account.id, id).await
            {
                tracing::error!(error = %e, "could not revoke a key that was never shown");
            }
            return Err(error);
        }
    };
    // The key is on this page, once: nothing may cache it, and the session cookie goes with it.
    Ok((
        AppendHeaders([
            (header::SET_COOKIE, cookie),
            (
                header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            ),
        ]),
        body,
    )
        .into_response())
}
