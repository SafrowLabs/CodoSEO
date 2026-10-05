//! `/sites`: the account's sites and the add-site form (onboarding when there are none).

use askama::Template;
use axum::extract::State;
use axum::http::HeaderName;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use codoseo_core::crawl::AddressPolicy;
use codoseo_core::plan::{Plan, PlanLimits, Schedule};
use codoseo_store::sites::CreateOutcome;
use serde::Deserialize;
use url::Url;

use crate::auth::CurrentUser;
use crate::config::Mode;
use crate::error::AppError;
use crate::fmt;
use crate::layout::{Screen, Shell, initial};
use crate::render::{Hx, html};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/sites", get(index).post(create))
}

/// Priority lane for a site's first crawl (spec section 10).
pub const FIRST_CRAWL_PRIORITY: i16 = 1;

pub struct SiteCard {
    pub domain: String,
    pub initial: String,
    pub href: String,
    pub added: String,
}

#[derive(Template)]
#[template(path = "sites/index.html")]
pub struct SitesPage {
    pub shell: Shell,
    pub cards: Vec<SiteCard>,
    pub form: AddSiteForm,
    pub limit_note: Option<String>,
}

#[derive(Template)]
#[template(path = "sites/form.html")]
pub struct AddSiteForm {
    pub url: String,
    pub error: Option<String>,
    pub can_add: bool,
    pub first: bool,
}

async fn index(State(state): State<AppState>, user: CurrentUser) -> Result<Response, AppError> {
    let shell = Shell::load(&state, &user, None, Screen::Sites).await?;
    let sites = codoseo_store::sites::list_for_account(&state.pool, user.id()).await?;
    let cards = sites
        .iter()
        .map(|s| SiteCard {
            domain: s.domain.clone(),
            initial: initial(&s.domain),
            href: format!("/s/{}/audit", s.id),
            added: fmt::date(s.created_at),
        })
        .collect::<Vec<_>>();
    let limit_note = PlanLimits::for_plan(user.account.plan)
        .max_sites
        .map(|max| format!("{} of {max} sites on your plan", sites.len()));
    let form = AddSiteForm {
        url: String::new(),
        error: None,
        can_add: shell.can_add_site,
        first: cards.is_empty(),
    };
    Ok(html(&SitesPage {
        shell,
        cards,
        form,
        limit_note,
    })?
    .into_response())
}

#[derive(Deserialize)]
pub struct NewSite {
    url: String,
}

/// Turns what the user typed (`example.com`, `https://example.com/blog`) into a start URL.
pub fn parse_start_url(raw: &str) -> Result<Url, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("Enter your site's address, like example.com.".to_owned());
    }
    let with_scheme = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("https://{raw}")
    };
    let mut url =
        Url::parse(&with_scheme).map_err(|_| "That isn't a valid web address.".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Only http and https sites can be crawled.".to_owned());
    }
    let host = url.host_str().unwrap_or_default();
    if host.is_empty() || (!host.contains('.') && host != "localhost") {
        return Err("That isn't a valid web address.".to_owned());
    }
    url.set_fragment(None);
    Ok(url)
}

/// How often a new site on `plan` is crawled on a schedule: daily on paid plans, weekly on Free.
pub fn schedule_for(plan: Plan) -> Option<&'static str> {
    match PlanLimits::for_plan(plan).fastest_schedule {
        Some(Schedule::Daily) if plan != Plan::Free => Some("daily"),
        Some(_) => Some("weekly"),
        None => None,
    }
}

/// What the cloud says to an address it won't crawl.
const PRIVATE_TARGET: &str = "That address is private or internal, so we can't audit it.";

/// In the cloud, refuses addresses the crawler would refuse anyway, so the person hears it at
/// the form instead of from a failed crawl. IP literals go through the crawler's own guard;
/// host names are also checked when they are fetched, after DNS (and after every redirect).
pub fn check_public_target(url: &Url) -> Result<(), String> {
    if let Some(host) = url.host_str() {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let internal_name = host == "localhost"
            || [".localhost", ".local", ".internal", ".localdomain", ".lan"]
                .iter()
                .any(|suffix| host.ends_with(suffix));
        if internal_name {
            return Err(PRIVATE_TARGET.to_owned());
        }
    }
    codoseo_crawler::guard::check_url(url, AddressPolicy::Public)
        .map_err(|_| PRIVATE_TARGET.to_owned())
}

async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    hx: Hx,
    Form(form): Form<NewSite>,
) -> Result<Response, AppError> {
    let limits = PlanLimits::for_plan(user.account.plan);
    let count = codoseo_store::sites::count_for_account(&state.pool, user.id()).await?;
    let first = count == 0;
    let invalid = |msg: String| -> Result<Response, AppError> {
        let f = AddSiteForm {
            url: form.url.trim().to_owned(),
            error: Some(msg),
            can_add: true,
            first,
        };
        if hx.request {
            // The form is boosted (a normal navigation on success), so send the corrected form
            // back into its own place rather than over the page.
            Ok((
                [
                    (HeaderName::from_static("hx-retarget"), "#add-site"),
                    (HeaderName::from_static("hx-reswap"), "outerHTML"),
                ],
                html(&f)?,
            )
                .into_response())
        } else {
            Err(AppError::BadRequest(f.error.unwrap_or_default()))
        }
    };

    let max_sites = limits.max_sites.map(i64::from);
    let limit_reached = || {
        let max = max_sites.unwrap_or_default();
        AppError::Limit(format!(
            "Your plan includes {max} site{}. Remove one or upgrade to add more.",
            if max == 1 { "" } else { "s" }
        ))
    };
    let over_limit = |e: AppError| -> Result<Response, AppError> {
        match e {
            AppError::Limit(msg) if hx.request => invalid(msg),
            e => Err(e),
        }
    };
    // A quick check before validating the address; `create_checked` makes the final call.
    if max_sites.is_some_and(|max| count >= max) {
        return over_limit(limit_reached());
    }
    let start = match parse_start_url(&form.url) {
        Ok(u) => u,
        Err(msg) => return invalid(msg),
    };
    if state.config.mode == Mode::Cloud
        && let Err(msg) = check_public_target(&start)
    {
        return invalid(msg);
    }
    let domain = start.host_str().unwrap_or_default().to_lowercase();
    let schedule = schedule_for(user.account.plan);
    let outcome = codoseo_store::sites::create_checked(
        &state.pool,
        user.id(),
        &domain,
        start.as_str(),
        schedule,
        max_sites,
        FIRST_CRAWL_PRIORITY,
    )
    .await?;
    match outcome {
        CreateOutcome::Created(site) => {
            super::settings_alerts::default_rules_for_site(&state, user.id(), site.id).await;
            Ok(Redirect::to(&format!("/s/{}/audit", site.id)).into_response())
        }
        CreateOutcome::LimitReached => over_limit(limit_reached()),
        CreateOutcome::Duplicate => invalid(format!("{domain} is already one of your sites.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_targets() {
        let ok = |s: &str| check_public_target(&Url::parse(s).unwrap());
        assert!(ok("https://example.com/").is_ok());
        assert!(ok("https://93.184.216.34/").is_ok());
        for bad in [
            "http://localhost/",
            "http://app.localhost/",
            "http://127.0.0.1/",
            "http://10.1.2.3/",
            "http://192.168.0.1/",
            "http://172.16.0.1/",
            "http://169.254.169.254/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://2130706433/",
            "http://nas.local/",
            "http://db.internal/",
        ] {
            assert!(ok(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn start_urls() {
        assert_eq!(
            parse_start_url("example.com").unwrap().as_str(),
            "https://example.com/"
        );
        assert_eq!(
            parse_start_url(" http://Example.com/blog#x ")
                .unwrap()
                .as_str(),
            "http://example.com/blog"
        );
        assert!(parse_start_url("").is_err());
        assert!(parse_start_url("ftp://example.com").is_err());
        assert!(parse_start_url("nodot").is_err());
    }
}
