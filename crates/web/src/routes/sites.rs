//! `/sites`: the account's sites and the add-site form (onboarding when there are none).

use askama::Template;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use codoseo_core::plan::{Plan, PlanLimits, Schedule};
use codoseo_store::crawl_queue::{CrawlQueue, CrawlTrigger};
use serde::Deserialize;
use url::Url;

use crate::auth::CurrentUser;
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
            Ok(html(&f)?.into_response())
        } else {
            Err(AppError::BadRequest(f.error.unwrap_or_default()))
        }
    };

    if let Some(max) = limits.max_sites
        && count >= i64::from(max)
    {
        return Err(AppError::Limit(format!(
            "Your plan includes {max} site{}. Remove one or upgrade to add more.",
            if max == 1 { "" } else { "s" }
        )));
    }
    let start = match parse_start_url(&form.url) {
        Ok(u) => u,
        Err(msg) => return invalid(msg),
    };
    let domain = start.host_str().unwrap_or_default().to_lowercase();
    let existing = codoseo_store::sites::list_for_account(&state.pool, user.id()).await?;
    if existing.iter().any(|s| s.domain == domain) {
        return invalid(format!("{domain} is already one of your sites."));
    }

    let schedule = match limits.fastest_schedule {
        Some(Schedule::Daily) if user.account.plan != Plan::Free => Some("daily"),
        Some(_) => Some("weekly"),
        None => None,
    };
    let site =
        codoseo_store::sites::create(&state.pool, user.id(), &domain, start.as_str(), schedule)
            .await?;
    CrawlQueue::new(state.pool.clone())
        .enqueue(
            site.id,
            &site.domain,
            CrawlTrigger::First,
            FIRST_CRAWL_PRIORITY,
            None,
            None,
        )
        .await?;
    Ok(Redirect::to(&format!("/s/{}/audit", site.id)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

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
