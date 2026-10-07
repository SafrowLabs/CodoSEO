//! The cloud landing page at `/`: the marketing page, with one URL box that starts a no-signup
//! audit. Its numbers (checks per category, the plan limits, the size of the quick audit) come
//! from the code, so the page can't promise something the app doesn't enforce.

use askama::Template;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use codoseo_checks::{CHECKS, Category};
use codoseo_core::plan::{ManualAllowance, Plan, PlanLimits, Schedule};

use crate::config::UmamiConfig;
use crate::error::AppError;
use crate::fmt;
use crate::render::html;
use crate::state::AppState;

#[derive(Template)]
#[template(path = "landing/index.html")]
pub struct Landing {
    /// What the visitor typed, kept when the address is refused.
    pub url: String,
    pub error: Option<String>,
    pub checks: usize,
    pub base: String,
    /// The origin without its scheme (`codoseo.com`), where the page names the hosted service.
    pub host: String,
    /// Set when Turnstile is configured: the widget and its script are shown.
    pub turnstile_site_key: Option<String>,
    pub umami: Option<UmamiConfig>,
    pub categories: Vec<CategoryView>,
    /// Pages in the no-signup audit.
    pub quick_pages: u32,
    pub free: Vec<String>,
    pub self_hosted: Vec<String>,
}

pub struct CategoryView {
    pub name: &'static str,
    pub count: usize,
    pub highlights: &'static [Highlight],
}

/// A few of a category's checks in plain words, coloured by their worst severity.
pub struct Highlight {
    pub label: &'static str,
    /// `r` critical, `a` warning, `n0` notice: the dot colour in the stylesheet.
    pub severity: &'static str,
}

const fn h(label: &'static str, severity: &'static str) -> Highlight {
    Highlight { label, severity }
}

const CATEGORIES: [(Category, &str, &[Highlight]); 9] = [
    (
        Category::Response,
        "Response",
        &[
            h("4xx and 5xx errors", "r"),
            h("Fetch failures", "r"),
            h("Redirect loops", "r"),
            h("Redirect chains", "a"),
        ],
    ),
    (
        Category::Indexability,
        "Indexability",
        &[
            h("robots.txt blocking the site", "r"),
            h("Pages blocked by robots.txt", "a"),
            h("Canonical to non-200", "a"),
            h("Noindex and missing canonicals", "n0"),
        ],
    ),
    (
        Category::OnPage,
        "On-page",
        &[
            h("Missing or duplicate titles", "a"),
            h("Missing or duplicate descriptions", "a"),
            h("Missing H1s", "a"),
            h("Length violations", "n0"),
        ],
    ),
    (
        Category::Content,
        "Content",
        &[
            h("Duplicate content", "a"),
            h("Thin pages under 200 words", "n0"),
            h("Missing image alt text", "n0"),
        ],
    ),
    (
        Category::Links,
        "Links",
        &[
            h("Broken links", "a"),
            h("Orphan pages", "a"),
            h("Links to redirects", "n0"),
            h("Deep pages", "n0"),
        ],
    ),
    (
        Category::Technical,
        "Technical",
        &[
            h("HTTP instead of HTTPS", "a"),
            h("Mixed content", "a"),
            h("Slow responses", "n0"),
        ],
    ),
    (
        Category::Sitemap,
        "Sitemap",
        &[
            h("Sitemap pages not 200", "a"),
            h("Sitemap pages that are noindex", "a"),
            h("Pages missing from the sitemap", "n0"),
        ],
    ),
    (
        Category::Social,
        "Social",
        &[h("Missing Open Graph title or image", "n0")],
    ),
    (
        Category::Schema,
        "Schema",
        &[
            h("Invalid JSON-LD", "a"),
            h("hreflang missing self-reference", "n0"),
        ],
    ),
];

fn categories() -> Vec<CategoryView> {
    CATEGORIES
        .iter()
        .map(|&(cat, name, highlights)| CategoryView {
            name,
            count: CHECKS.iter().filter(|c| c.category == cat).count(),
            highlights,
        })
        .collect()
}

fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// What a plan includes, in the words the pricing cards use.
pub fn plan_features(plan: Plan) -> Vec<String> {
    let l = PlanLimits::for_plan(plan);
    let mut out = vec![match l.max_sites {
        Some(n) => plural(n, "monitored site", "monitored sites"),
        None => "Unlimited sites".to_owned(),
    }];
    if let Some(n) = l.max_pages {
        out.push(format!("Up to {} pages per crawl", fmt::thousands(n)));
    }
    match l.fastest_schedule {
        Some(Schedule::Weekly) => out.push("A scheduled crawl every week".to_owned()),
        Some(Schedule::Daily) => out.push("A scheduled crawl every day".to_owned()),
        None => {}
    }
    out.push(match l.manual_crawls {
        ManualAllowance::PerWeek(n) => {
            format!("{} a week", plural(n, "manual crawl", "manual crawls"))
        }
        ManualAllowance::PerDay(n) => {
            format!("{} a day", plural(n, "manual crawl", "manual crawls"))
        }
        ManualAllowance::Unlimited => "Unlimited manual crawls".to_owned(),
    });
    if let Some(d) = l.history_days {
        out.push(format!("{d} days of history"));
    }
    out.push(if l.email_alerts_only {
        "Email alerts".to_owned()
    } else {
        "Email, Slack, Discord and webhook alerts".to_owned()
    });
    out.push(match l.api_calls_per_day {
        Some(n) => format!("{} API calls a day", fmt::thousands(n)),
        None => "No API call limit".to_owned(),
    });
    out
}

fn view(state: &AppState, url: &str, error: Option<String>) -> Landing {
    let base = state.config.origin();
    Landing {
        url: url.to_owned(),
        error,
        checks: CHECKS.len(),
        host: base
            .split_once("://")
            .map_or(base.as_str(), |(_, host)| host)
            .to_owned(),
        base,
        turnstile_site_key: state.config.turnstile.as_ref().map(|t| t.site_key.clone()),
        umami: state.config.umami.clone(),
        categories: categories(),
        quick_pages: PlanLimits::quick_audit().max_pages.unwrap_or(100),
        free: plan_features(Plan::Free),
        self_hosted: plan_features(Plan::SelfHosted),
    }
}

/// The landing page for a signed-out visitor.
pub fn page(state: &AppState) -> Result<Response, AppError> {
    Ok(html(&view(state, "", None))?.into_response())
}

/// The landing page again with a message under the box, for a submit we turned away.
pub fn refuse(
    state: &AppState,
    url: &str,
    status: StatusCode,
    message: String,
) -> Result<Response, AppError> {
    Ok((status, html(&view(state, url.trim(), Some(message)))?).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_check_is_in_a_listed_category() {
        let listed: usize = categories().iter().map(|c| c.count).sum();
        assert_eq!(listed, CHECKS.len());
    }

    #[test]
    fn the_free_card_matches_the_free_plan() {
        assert_eq!(
            plan_features(Plan::Free),
            [
                "1 monitored site",
                "Up to 500 pages per crawl",
                "A scheduled crawl every week",
                "1 manual crawl a week",
                "30 days of history",
                "Email alerts",
                "100 API calls a day",
            ]
        );
    }

    #[test]
    fn the_self_hosted_card_has_no_plan_caps() {
        let f = plan_features(Plan::SelfHosted);
        assert_eq!(f[0], "Unlimited sites");
        assert!(f.contains(&"Up to 100,000 pages per crawl".to_owned()));
        assert!(f.contains(&"No API call limit".to_owned()));
        assert!(f.contains(&"Email, Slack, Discord and webhook alerts".to_owned()));
    }
}
