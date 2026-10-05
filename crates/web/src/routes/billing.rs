//! `/billing`: the plan, upgrading through Dodo Payments checkout, the customer portal, and
//! choosing which sites stay monitored after a downgrade. Cloud only: on a self-hosted
//! instance every route here is a 404 (and nothing links to them).

use askama::Template;
use axum::body::Bytes;
use axum::extract::{FromRequestParts, Query, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use codoseo_core::plan::{ManualAllowance, Plan, PlanLimits};
use codoseo_store::billing::{self, SetMonitored};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::{AuthRejection, CurrentUser};
use crate::billing::dodo;
use crate::billing::webhook;
use crate::config::Mode;
use crate::error::AppError;
use crate::fmt;
use crate::layout::{Screen, Shell};
use crate::render::html;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/billing", get(page))
        .route("/billing/checkout", post(checkout))
        .route("/billing/portal", post(portal))
        .route("/billing/sites", get(sites_page).post(save_sites))
        .route("/billing/webhook", post(webhook::receive))
}

/// A signed-in user on the cloud. Self-hosted instances answer 404 before anything else (even
/// to a signed-out visitor), so the billing routes look absent there.
pub struct CloudUser(pub CurrentUser);

impl FromRequestParts<AppState> for CloudUser {
    type Rejection = AuthRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<CloudUser, AuthRejection> {
        if state.config.mode != Mode::Cloud {
            return Err(AuthRejection::Error(AppError::NotFound));
        }
        CurrentUser::from_request_parts(parts, state)
            .await
            .map(CloudUser)
    }
}

// ---- the billing page -------------------------------------------------------------------------

pub struct PlanCard {
    pub slug: &'static str,
    pub name: &'static str,
    pub features: Vec<String>,
    /// `upgrade` (checkout), `current`, or `portal` (a paying account changes plan there).
    pub action: &'static str,
}

#[derive(Template)]
#[template(path = "billing/index.html")]
pub struct BillingPage {
    pub shell: Shell,
    pub configured: bool,
    pub plan_name: &'static str,
    pub paid: bool,
    /// When the paid period ends (the next billing date), for a paid plan.
    pub period_end: Option<String>,
    pub has_customer: bool,
    pub cards: Vec<PlanCard>,
    pub success: bool,
    pub error: Option<String>,
    pub stopped_sites: bool,
}

fn plan_name(plan: Plan) -> &'static str {
    match plan {
        Plan::Free => "Free",
        Plan::Pro => "Pro",
        Plan::Agency => "Agency",
        Plan::SelfHosted => "Self-hosted",
    }
}

fn features(plan: Plan) -> Vec<String> {
    let l = PlanLimits::for_plan(plan);
    let mut out = Vec::new();
    if let Some(n) = l.max_sites {
        out.push(format!(
            "{n} monitored site{}",
            if n == 1 { "" } else { "s" }
        ));
    }
    if let Some(n) = l.max_pages {
        out.push(format!("Up to {} pages per crawl", fmt::thousands(n)));
    }
    out.push("A crawl every day".to_owned());
    out.push(match l.manual_crawls {
        ManualAllowance::PerDay(n) => format!("{n} manual crawl a day"),
        ManualAllowance::PerWeek(n) => format!("{n} manual crawl a week"),
        ManualAllowance::Unlimited => "Unlimited manual crawls".to_owned(),
    });
    if let Some(d) = l.history_days {
        out.push(format!("{d} days of history"));
    }
    out.push("Email, Slack, Discord and webhook alerts".to_owned());
    out
}

async fn render_page(
    state: &AppState,
    user: &CurrentUser,
    status: StatusCode,
    success: bool,
    error: Option<String>,
) -> Result<Response, AppError> {
    let shell = Shell::load(state, user, None, Screen::Billing).await?;
    let info = billing::account_billing(&state.pool, user.id()).await?;
    let plan = user.account.plan;
    let paid = matches!(plan, Plan::Pro | Plan::Agency);
    let configured = state.config.billing.is_some();
    let sites = codoseo_store::sites::list_for_account(&state.pool, user.id()).await?;
    let card = |p: Plan, slug, name| PlanCard {
        slug,
        name,
        features: features(p),
        action: if plan == p {
            "current"
        } else if paid {
            "portal"
        } else {
            "upgrade"
        },
    };
    let page = BillingPage {
        shell,
        configured,
        plan_name: plan_name(plan),
        paid,
        period_end: info
            .as_ref()
            .and_then(|i| i.plan_expires_at)
            .filter(|_| paid)
            .map(|at| fmt::date(at - billing::GRACE)),
        has_customer: info.as_ref().is_some_and(|i| i.customer_id.is_some()),
        cards: vec![
            card(Plan::Pro, "pro", "Pro"),
            card(Plan::Agency, "agency", "Agency"),
        ],
        success,
        error,
        stopped_sites: sites.iter().any(|s| !s.monitoring_active),
    };
    let mut res = html(&page)?.into_response();
    *res.status_mut() = status;
    Ok(res)
}

#[derive(Deserialize)]
struct PageQuery {
    status: Option<String>,
}

async fn page(
    State(state): State<AppState>,
    CloudUser(user): CloudUser,
    Query(q): Query<PageQuery>,
) -> Result<Response, AppError> {
    let success = q.status.as_deref() == Some("success");
    render_page(&state, &user, StatusCode::OK, success, None).await
}

const UNAVAILABLE: &str = "Billing isn't configured on this server, so checkout is off.";
const PROVIDER_DOWN: &str = "We couldn't reach our payment provider. Nothing was charged. \
                             Try again in a moment.";

#[derive(Deserialize)]
struct CheckoutForm {
    plan: String,
}

async fn checkout(
    State(state): State<AppState>,
    CloudUser(user): CloudUser,
    Form(form): Form<CheckoutForm>,
) -> Result<Response, AppError> {
    let Some(cfg) = state.config.billing.as_ref() else {
        return render_page(
            &state,
            &user,
            StatusCode::SERVICE_UNAVAILABLE,
            false,
            Some(UNAVAILABLE.to_owned()),
        )
        .await;
    };
    let plan = match form.plan.as_str() {
        "pro" => Plan::Pro,
        "agency" => Plan::Agency,
        _ => return Err(AppError::BadRequest("Choose Pro or Agency.".to_owned())),
    };
    if matches!(user.account.plan, Plan::Pro | Plan::Agency) {
        return Err(AppError::Conflict(
            "You already have a paid plan. Change or cancel it with Manage billing.".to_owned(),
        ));
    }
    let product = cfg.product_for(plan).ok_or(AppError::NotFound)?;
    let return_url = state
        .config
        .base_url
        .join("billing?status=success")
        .map_err(AppError::internal)?;
    match dodo::create_checkout(
        &state.http,
        cfg,
        product,
        &user.account.email,
        user.id(),
        return_url.as_str(),
    )
    .await
    {
        Ok(url) => Ok(Redirect::to(&url).into_response()),
        Err(error) => {
            tracing::warn!(%error, account = %user.id(), "Dodo checkout failed");
            render_page(
                &state,
                &user,
                StatusCode::BAD_GATEWAY,
                false,
                Some(PROVIDER_DOWN.to_owned()),
            )
            .await
        }
    }
}

async fn portal(
    State(state): State<AppState>,
    CloudUser(user): CloudUser,
) -> Result<Response, AppError> {
    let Some(cfg) = state.config.billing.as_ref() else {
        return render_page(
            &state,
            &user,
            StatusCode::SERVICE_UNAVAILABLE,
            false,
            Some(UNAVAILABLE.to_owned()),
        )
        .await;
    };
    let customer = billing::account_billing(&state.pool, user.id())
        .await?
        .and_then(|i| i.customer_id)
        .ok_or_else(|| {
            AppError::BadRequest(
                "There's no billing account to manage yet. Upgrade first.".to_owned(),
            )
        })?;
    let return_url = state
        .config
        .base_url
        .join("billing")
        .map_err(AppError::internal)?;
    match dodo::create_portal_session(&state.http, cfg, &customer, return_url.as_str()).await {
        Ok(link) => Ok(Redirect::to(&link).into_response()),
        Err(error) => {
            tracing::warn!(%error, account = %user.id(), "Dodo portal failed");
            render_page(
                &state,
                &user,
                StatusCode::BAD_GATEWAY,
                false,
                Some(PROVIDER_DOWN.to_owned()),
            )
            .await
        }
    }
}

// ---- which sites stay monitored ---------------------------------------------------------------

pub struct SiteChoice {
    pub id: Uuid,
    pub domain: String,
    pub added: String,
    pub active: bool,
}

#[derive(Template)]
#[template(path = "billing/sites.html")]
pub struct SitesChoicePage {
    pub shell: Shell,
    pub plan_name: &'static str,
    /// `None`: no limit.
    pub max: Option<u32>,
    pub sites: Vec<SiteChoice>,
    pub active_count: usize,
}

async fn sites_page(
    State(state): State<AppState>,
    CloudUser(user): CloudUser,
) -> Result<Response, AppError> {
    let shell = Shell::load(&state, &user, None, Screen::Billing).await?;
    let sites = codoseo_store::sites::list_for_account(&state.pool, user.id()).await?;
    let active_count = sites.iter().filter(|s| s.monitoring_active).count();
    Ok(html(&SitesChoicePage {
        shell,
        plan_name: plan_name(user.account.plan),
        max: PlanLimits::for_plan(user.account.plan).max_sites,
        sites: sites
            .iter()
            .map(|s| SiteChoice {
                id: s.id,
                domain: s.domain.clone(),
                added: fmt::date(s.created_at),
                active: s.monitoring_active,
            })
            .collect(),
        active_count,
    })?
    .into_response())
}

async fn save_sites(
    State(state): State<AppState>,
    CloudUser(user): CloudUser,
    body: Bytes,
) -> Result<Response, AppError> {
    // The form repeats `keep` once per ticked site, which `Form<struct>` can't read.
    let mut keep = Vec::new();
    for (name, value) in url::form_urlencoded::parse(&body) {
        if name == "keep" {
            keep.push(
                Uuid::parse_str(&value)
                    .map_err(|_| AppError::BadRequest("That isn't a site.".to_owned()))?,
            );
        }
    }
    match billing::set_monitored(&state.pool, user.id(), &keep).await? {
        SetMonitored::Saved => Ok(Redirect::to("/sites").into_response()),
        SetMonitored::UnknownSite => Err(AppError::NotFound),
        SetMonitored::TooMany { max } => Err(AppError::Limit(format!(
            "Your plan monitors {max} site{}. Untick some, or upgrade to keep more.",
            if max == 1 { "" } else { "s" }
        ))),
    }
}

/// True when `sites` holds a site the plan stopped monitoring (shown as a banner on `/sites`).
pub fn has_stopped_sites(sites: &[codoseo_store::sites::Site]) -> bool {
    sites.iter().any(|s| !s.monitoring_active)
}
