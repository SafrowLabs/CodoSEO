//! `/settings/alerts`: where alerts go (the account's channels) and which changes go there at
//! once (the per-site rules grid). Everything not marked instant waits for the Monday digest.

use std::collections::HashSet;
use std::time::Duration;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderName, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use codoseo_core::change::ChangeKind;
use codoseo_core::plan::PlanLimits;
use codoseo_notify::{AlertMessage, ChannelKind, ChannelTarget, DeliveryError, deliver};
use codoseo_store::alert_rules::{self, ALL_KINDS};
use codoseo_store::channels::{self, ChannelSummary, DeleteOutcome};
use codoseo_store::events::{self, EventKind};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::CurrentUser;
use crate::auth::email;
use crate::config::Mode;
use crate::error::AppError;
use crate::layout::{Screen, Shell};
use crate::render::{Hx, ToastKind, html, toast};
use crate::state::AppState;

/// How long "Send test" waits for the channel.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);
/// "Send test" messages per account per hour.
const TEST_LIMIT: i64 = 5;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/settings/alerts", get(page))
        .route("/settings/alerts/channels", post(add_channel))
        .route(
            "/settings/alerts/channels/{id}/delete",
            post(delete_channel),
        )
        .route("/settings/alerts/channels/{id}/mute", post(mute_channel))
        .route(
            "/settings/alerts/channels/{id}/enable",
            post(enable_channel),
        )
        .route("/settings/alerts/channels/{id}/test", post(test_channel))
        .route("/settings/alerts/rules", post(set_rule))
}

const UPGRADE: &str = "Slack, Discord and webhook alerts are on Pro and Agency. \
                       Upgrade to Pro to use them.";

fn kind_label(kind: ChannelKind) -> &'static str {
    match kind {
        ChannelKind::Email => "Email",
        ChannelKind::Slack => "Slack",
        ChannelKind::Discord => "Discord",
        ChannelKind::Webhook => "Webhook",
    }
}

/// Turns on the default instant rules on a new site for every enabled channel of the account,
/// the account's own email address first. The alert planner covers any site this misses, so a
/// failure is logged and not shown.
pub async fn default_rules_for_site(state: &AppState, account_id: Uuid, site_id: Uuid) {
    let result = async {
        channels::ensure_default_email(&state.pool, &state.channel_key, account_id)
            .await
            .map_err(|e| e.to_string())?;
        alert_rules::create_defaults_for_site(&state.pool, account_id, site_id)
            .await
            .map_err(|e| e.to_string())
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(%site_id, %error, "could not create the default alert rules");
    }
}

// ---- views -----------------------------------------------------------------------------------

pub struct ChannelView {
    pub id: Uuid,
    pub kind: &'static str,
    pub name: String,
    pub target: String,
    /// `active`, `muted` or `off`
    pub status: &'static str,
    pub status_label: &'static str,
    pub error: Option<String>,
    pub muted: bool,
    pub enabled: bool,
    pub is_default: bool,
}

pub struct RuleCell {
    pub channel: Uuid,
    pub label: String,
    pub checked: bool,
}

pub struct RuleRow {
    pub slug: &'static str,
    pub label: &'static str,
    pub note: &'static str,
    pub cells: Vec<RuleCell>,
}

pub struct SiteRules {
    pub id: Uuid,
    pub domain: String,
    pub rows: Vec<RuleRow>,
}

pub struct ColumnView {
    pub label: String,
    pub off: bool,
}

pub struct NewSecret {
    pub channel: String,
    pub secret: String,
}

#[derive(Template)]
#[template(path = "settings/alerts_form.html")]
pub struct AddChannelForm {
    pub kinds: Vec<(&'static str, &'static str)>,
    pub kind: String,
    pub target: String,
    pub name: String,
    pub error: Option<String>,
    /// Show "Upgrade to Pro": Slack, Discord and webhooks aren't on this plan.
    pub upgrade: bool,
    /// Link the hint to `/billing` (cloud only; self-hosted has no billing pages).
    pub upgrade_link: bool,
}

#[derive(Template)]
#[template(path = "settings/alerts.html")]
pub struct AlertsPage {
    pub shell: Shell,
    pub channels: Vec<ChannelView>,
    pub form: AddChannelForm,
    pub columns: Vec<ColumnView>,
    pub sites: Vec<SiteRules>,
    pub secret: Option<NewSecret>,
}

#[derive(Template)]
#[template(path = "settings/alerts_test.html")]
struct TestResult {
    error: Option<String>,
}

fn channel_view(c: &ChannelSummary) -> ChannelView {
    let (status, status_label) = if !c.enabled {
        ("off", "Turned off")
    } else if c.muted {
        ("muted", "Muted")
    } else {
        ("active", "Active")
    };
    ChannelView {
        id: c.id,
        kind: kind_label(c.kind),
        name: c
            .name
            .clone()
            .unwrap_or_else(|| kind_label(c.kind).to_owned()),
        target: c.target.clone(),
        status,
        status_label,
        error: if c.enabled {
            None
        } else {
            c.last_error.clone()
        },
        muted: c.muted,
        enabled: c.enabled,
        is_default: c.is_default,
    }
}

fn add_form(state: &AppState, email_only: bool) -> AddChannelForm {
    let mut kinds = vec![("email", "Email")];
    if !email_only {
        kinds.extend([
            ("slack", "Slack"),
            ("discord", "Discord"),
            ("webhook", "Webhook"),
        ]);
    }
    AddChannelForm {
        kinds,
        kind: "email".to_owned(),
        target: String::new(),
        name: String::new(),
        error: None,
        upgrade: email_only,
        upgrade_link: email_only && state.config.mode == Mode::Cloud,
    }
}

fn note(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::BecameNoindex => "key pages only",
        ChangeKind::ErrorSpike => "a burst of new 4xx and 5xx pages",
        ChangeKind::SitemapShrank => "lost 10% or more",
        ChangeKind::AiBotBlocked => "AI search bots blocked, or robots.txt failing",
        ChangeKind::AiAnswersRestricted => "page markup keeps pages out of AI answers",
        ChangeKind::AiIssueResolved => "an AI access issue is fixed",
        ChangeKind::AiBlockNotApplied => "a bot you block can still crawl",
        ChangeKind::AiPreferencesChanged => "Content-Signal, Content-Usage or TDM changed",
        _ => "",
    }
}

async fn render_page(
    state: &AppState,
    user: &CurrentUser,
    form: AddChannelForm,
    secret: Option<NewSecret>,
) -> Result<Html<String>, AppError> {
    let pool = &state.pool;
    let sites = codoseo_store::sites::list_for_account(pool, user.id()).await?;
    // The account's own address is always a channel; sites that predate alert rules get their
    // default rules the first time this page is opened.
    let default = channels::ensure_default_email(pool, &state.channel_key, user.id())
        .await
        .map_err(AppError::internal)?;
    for site in &sites {
        alert_rules::create_defaults(pool, site.id, default).await?;
    }
    let listed = channels::list_for_account(pool, &state.channel_key, user.id())
        .await
        .map_err(AppError::internal)?;
    let on: HashSet<(Uuid, ChangeKind, Uuid)> = alert_rules::grid(pool, user.id())
        .await?
        .into_iter()
        .filter(|c| c.instant)
        .map(|c| (c.site_id, c.kind, c.channel_id))
        .collect();

    let column_label = |c: &ChannelSummary| match &c.name {
        Some(name) => format!("{name} · {}", c.target),
        None => format!("{} · {}", kind_label(c.kind), c.target),
    };
    let sites = sites
        .iter()
        .map(|site| SiteRules {
            id: site.id,
            domain: site.domain.clone(),
            rows: ALL_KINDS
                .iter()
                .map(|&kind| RuleRow {
                    slug: kind.slug(),
                    label: kind.label(),
                    note: note(kind),
                    cells: listed
                        .iter()
                        .map(|c| RuleCell {
                            channel: c.id,
                            label: format!("{} to {}", kind.label(), column_label(c)),
                            checked: on.contains(&(site.id, kind, c.id)),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();
    let shell = Shell::load(state, user, None, Screen::Alerts).await?;
    html(&AlertsPage {
        shell,
        channels: listed.iter().map(channel_view).collect(),
        form,
        columns: listed
            .iter()
            .map(|c| ColumnView {
                label: column_label(c),
                off: !c.enabled,
            })
            .collect(),
        sites,
        secret,
    })
}

fn email_only(user: &CurrentUser) -> bool {
    PlanLimits::for_plan(user.account.plan).email_alerts_only
}

async fn page(State(state): State<AppState>, user: CurrentUser) -> Result<Response, AppError> {
    let form = add_form(&state, email_only(&user));
    Ok(render_page(&state, &user, form, None)
        .await?
        .into_response())
}

// ---- channels --------------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewChannel {
    kind: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    name: String,
}

/// A fresh signing secret for a webhook channel.
fn new_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("whsec_{hex}")
}

/// Why a channel could not be added: shown inline for htmx, as an error page otherwise.
enum Refusal {
    Invalid(String),
    Limit(String),
}

async fn add_channel(
    State(state): State<AppState>,
    user: CurrentUser,
    hx: Hx,
    Form(form): Form<NewChannel>,
) -> Result<Response, AppError> {
    let email_only = email_only(&user);
    match create(&state, &user, &form, email_only).await? {
        Ok(created) => {
            if let Some(secret) = created.secret {
                // Shown once, on the page this answers with.
                let page = render_page(
                    &state,
                    &user,
                    add_form(&state, email_only),
                    Some(NewSecret {
                        channel: created.name,
                        secret,
                    }),
                )
                .await?;
                return Ok((
                    [
                        (
                            HeaderName::from_static("hx-replace-url"),
                            "/settings/alerts",
                        ),
                        // The secret is on this page; nothing may cache it.
                        (header::CACHE_CONTROL, "no-store"),
                    ],
                    page,
                )
                    .into_response());
            }
            Ok(Redirect::to("/settings/alerts").into_response())
        }
        Err(refusal) => {
            let (status, message) = match &refusal {
                Refusal::Invalid(m) => (StatusCode::BAD_REQUEST, m.clone()),
                Refusal::Limit(m) => (StatusCode::FORBIDDEN, m.clone()),
            };
            if !hx.request {
                return Err(match refusal {
                    Refusal::Invalid(m) => AppError::BadRequest(m),
                    Refusal::Limit(m) => AppError::Limit(m),
                });
            }
            let mut again = add_form(&state, email_only);
            again.kind = form.kind.clone();
            again.target = form.target.trim().to_owned();
            again.name = form.name.trim().to_owned();
            again.error = Some(message);
            Ok((
                status,
                [
                    (HeaderName::from_static("hx-retarget"), "#add-channel"),
                    (HeaderName::from_static("hx-reswap"), "outerHTML"),
                ],
                html(&again)?,
            )
                .into_response())
        }
    }
}

struct Created {
    name: String,
    /// The webhook signing secret, to show once.
    secret: Option<String>,
}

async fn create(
    state: &AppState,
    user: &CurrentUser,
    form: &NewChannel,
    email_only: bool,
) -> Result<Result<Created, Refusal>, AppError> {
    let Some(kind) = ChannelKind::parse(form.kind.trim()) else {
        return Ok(Err(Refusal::Invalid(
            "Pick where alerts should go.".to_owned(),
        )));
    };
    if email_only && kind != ChannelKind::Email {
        return Ok(Err(Refusal::Limit(UPGRADE.to_owned())));
    }
    let held = channels::count_for_account(&state.pool, user.id())
        .await
        .map_err(AppError::internal)?;
    if held >= channels::MAX_CHANNELS_PER_ACCOUNT {
        return Ok(Err(Refusal::Limit(format!(
            "You can have up to {} alert channels. Delete one you don't use to add another.",
            channels::MAX_CHANNELS_PER_ACCOUNT
        ))));
    }
    let raw = form.target.trim();
    let mut secret = None;
    let target = match kind {
        ChannelKind::Email => match email::parse(raw) {
            Some(address) => ChannelTarget::Email {
                to: address.to_owned(),
            },
            None => {
                return Ok(Err(Refusal::Invalid(
                    "Enter a valid email address.".to_owned(),
                )));
            }
        },
        _ => {
            let url = match state.notify_http.validate_target(kind, raw).await {
                Ok(url) => url,
                Err(e) => return Ok(Err(Refusal::Invalid(sentence(&e.to_string())))),
            };
            match kind {
                ChannelKind::Slack => ChannelTarget::Slack { url },
                ChannelKind::Discord => ChannelTarget::Discord { url },
                _ => {
                    let s = new_secret();
                    secret = Some(s.clone());
                    ChannelTarget::Webhook { url, secret: s }
                }
            }
        }
    };
    let name = form.name.trim();
    let name = (!name.is_empty()).then(|| name.chars().take(60).collect::<String>());
    let id = channels::create(
        &state.pool,
        &state.channel_key,
        user.id(),
        &target,
        name.as_deref(),
        false,
    )
    .await
    .map_err(AppError::internal)?;
    alert_rules::create_defaults_for_account(&state.pool, user.id(), id).await?;
    Ok(Ok(Created {
        name: name.unwrap_or_else(|| kind_label(kind).to_owned()),
        secret,
    }))
}

/// "that doesn't look like a web address" -> "That doesn't look like a web address."
fn sentence(s: &str) -> String {
    let mut chars = s.chars();
    let mut out: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => return String::new(),
    };
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

/// The account's channel, or a 404.
async fn owned(
    state: &AppState,
    user: &CurrentUser,
    id: Uuid,
) -> Result<channels::ChannelState, AppError> {
    match channels::state(&state.pool, id)
        .await
        .map_err(AppError::internal)?
    {
        Some(c) if c.account_id == user.id() => Ok(c),
        _ => Err(AppError::NotFound),
    }
}

async fn delete_channel(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Response, AppError> {
    match channels::delete(&state.pool, user.id(), id).await? {
        DeleteOutcome::Deleted => Ok(Redirect::to("/settings/alerts").into_response()),
        DeleteOutcome::NotFound => Err(AppError::NotFound),
        DeleteOutcome::DefaultChannel => Err(AppError::Conflict(
            "Your own email address is always a channel. Mute it instead of deleting it."
                .to_owned(),
        )),
    }
}

#[derive(Deserialize)]
struct MuteForm {
    muted: String,
}

async fn mute_channel(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(form): Form<MuteForm>,
) -> Result<Response, AppError> {
    if !channels::set_muted(&state.pool, user.id(), id, form.muted == "true").await? {
        return Err(AppError::NotFound);
    }
    Ok(Redirect::to("/settings/alerts").into_response())
}

async fn enable_channel(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Response, AppError> {
    if !channels::reenable(&state.pool, user.id(), id).await? {
        return Err(AppError::NotFound);
    }
    Ok(Redirect::to("/settings/alerts").into_response())
}

/// Sends a test message to the channel, waiting up to 10 seconds, and answers with the small
/// fragment that replaces the button's result slot.
async fn test_channel(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Response, AppError> {
    owned(&state, &user, id).await?;
    if let Err(minutes) = events::take_allowance(
        &state.pool,
        EventKind::ChannelTest,
        user.id(),
        TEST_LIMIT,
        60,
    )
    .await?
    {
        let message = format!(
            "Too many test messages — try again in {minutes} minute{}.",
            if minutes == 1 { "" } else { "s" }
        );
        let page = html(&TestResult {
            error: Some(message),
        })?;
        return Ok((StatusCode::TOO_MANY_REQUESTS, page).into_response());
    }
    let result =
        |error: Option<String>| html(&TestResult { error }).map(IntoResponse::into_response);
    let target = match channels::get_target(&state.pool, &state.channel_key, id).await {
        Ok(Some(target)) => target,
        Ok(None) => return Err(AppError::NotFound),
        Err(e) => return result(Some(format!("The saved address can't be read: {e}"))),
    };
    let sites = codoseo_store::sites::list_for_account(&state.pool, user.id()).await?;
    let (domain, dashboard) = match sites.first() {
        Some(site) => (
            site.domain.clone(),
            state
                .config
                .base_url
                .join(&format!("s/{}/changes", site.id)),
        ),
        None => ("your site".to_owned(), Ok(state.config.base_url.clone())),
    };
    let dashboard = dashboard.map_err(AppError::internal)?;
    let message = AlertMessage::test(domain, dashboard);
    let outcome = tokio::time::timeout(
        TEST_TIMEOUT,
        deliver(&state.notify_http, &state.mailer, &target, &message),
    )
    .await;
    match outcome {
        Ok(Ok(())) => result(None),
        // Mail errors name our own SMTP host and its replies; they belong in the log, not on the
        // page. A webhook's status or refusal is about the user's own endpoint, so it shows.
        Ok(Err(DeliveryError::Mail(e))) => {
            tracing::warn!(channel_id = %id, error = %e, "test email failed");
            result(Some("Couldn't send the test message.".to_owned()))
        }
        Ok(Err(e)) => result(Some(sentence(&e.to_string()))),
        Err(_) => result(Some("It didn't answer within 10 seconds.".to_owned())),
    }
}

// ---- rules -----------------------------------------------------------------------------------

#[derive(Deserialize)]
struct RuleForm {
    site: Uuid,
    kind: String,
    channel: Uuid,
    /// A checked box sends a value; an unchecked one sends nothing.
    #[serde(default)]
    instant: Option<String>,
}

async fn set_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    hx: Hx,
    Form(form): Form<RuleForm>,
) -> Result<Response, AppError> {
    let kind: ChangeKind = ALL_KINDS
        .iter()
        .copied()
        .find(|k| k.slug() == form.kind)
        .ok_or_else(|| AppError::BadRequest("That kind of change doesn't exist.".to_owned()))?;
    // Both must be the account's own.
    if codoseo_store::sites::get_for_account(&state.pool, user.id(), form.site)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound);
    }
    owned(&state, &user, form.channel).await?;
    let instant = form
        .instant
        .as_deref()
        .is_some_and(|v| v != "false" && v != "0");
    if !alert_rules::set(&state.pool, form.site, kind, form.channel, instant).await? {
        return Err(AppError::NotFound);
    }
    if hx.request {
        let message = if instant {
            "Instant alerts on"
        } else {
            "Moved to the weekly digest"
        };
        return Ok((StatusCode::NO_CONTENT, [toast(ToastKind::Ok, message)]).into_response());
    }
    Ok(Redirect::to("/settings/alerts").into_response())
}
