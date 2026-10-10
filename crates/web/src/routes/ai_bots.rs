//! `/ai-bots` and `/ai-bots.json`: the public AI bot registry, on the cloud and self-hosted
//! alike. The page is the registry as a searchable table; the JSON is the data file itself
//! (CC0-1.0), served with open CORS so anyone can build on it.

use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use codoseo_geo::{Honours, Purpose, registry, registry_json};

use super::ai_access::purpose_label;
use crate::config::{Mode, UmamiConfig};
use crate::error::AppError;
use crate::render::html;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ai-bots", get(page))
        .route("/ai-bots.json", get(json))
}

pub struct BotEntry {
    pub token: String,
    pub product: String,
    pub purpose: &'static str,
    pub purpose_class: &'static str,
    pub honours: &'static str,
    pub honours_class: &'static str,
    pub control: bool,
    pub ip_ranges_url: Option<String>,
    pub source_url: String,
    pub source_host: String,
    pub last_reviewed: String,
    pub notes: String,
    /// Lower-cased text the search box matches against.
    pub haystack: String,
}

pub struct OperatorGroup {
    pub operator: String,
    pub bots: Vec<BotEntry>,
}

#[derive(Template)]
#[template(path = "ai_bots/index.html")]
pub struct AiBotsPage {
    pub base: String,
    pub cloud: bool,
    pub umami: Option<UmamiConfig>,
    pub groups: Vec<OperatorGroup>,
    pub total: usize,
    pub operators: usize,
    pub updated: String,
    pub version: u32,
    pub license: String,
}

/// The purpose as the registry's pill says it (shorter than the app's group headings).
fn purpose_short(p: Purpose) -> &'static str {
    match p {
        Purpose::UserFetch => "User-triggered",
        Purpose::Agent => "Agent",
        other => purpose_label(other),
    }
}

/// `developers.openai.com` -> `openai.com`: the operator's own domain, as the link reads.
fn site_of(url: &str) -> String {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned());
    let labels: Vec<&str> = host.split('.').collect();
    match labels.as_slice() {
        [.., a, b] => format!("{a}.{b}"),
        _ => host,
    }
}

fn purpose_class(p: Purpose) -> &'static str {
    match p {
        Purpose::Search => "reg-p-search",
        Purpose::UserFetch => "reg-p-fetch",
        Purpose::Agent => "reg-p-agent",
        Purpose::Training => "reg-p-training",
        Purpose::Ads => "reg-p-ads",
    }
}

async fn page(State(state): State<AppState>) -> Result<Response, AppError> {
    let reg = registry();
    let mut groups: Vec<OperatorGroup> = Vec::new();
    for bot in &reg.bots {
        let (honours, honours_class) = match bot.honours_robots {
            Honours::Yes => ("Yes", "c-ok"),
            Honours::Partial => ("Partly", "c-warn"),
            Honours::No => ("No", "c-err"),
            Honours::Unknown => ("Not stated", "c-muted"),
        };
        let entry = BotEntry {
            token: bot.token.clone(),
            product: bot.product.clone(),
            purpose: purpose_short(bot.purpose),
            purpose_class: purpose_class(bot.purpose),
            honours,
            honours_class,
            control: !bot.crawls,
            ip_ranges_url: bot.ip_ranges_url.clone(),
            source_url: bot.source_url.clone(),
            source_host: site_of(&bot.source_url),
            last_reviewed: bot.last_reviewed.clone(),
            notes: bot.notes.clone(),
            haystack: format!(
                "{} {} {} {}",
                bot.token,
                bot.operator,
                bot.product,
                purpose_label(bot.purpose)
            )
            .to_lowercase(),
        };
        match groups.iter_mut().find(|g| g.operator == bot.operator) {
            Some(g) => g.bots.push(entry),
            None => groups.push(OperatorGroup {
                operator: bot.operator.clone(),
                bots: vec![entry],
            }),
        }
    }
    Ok(html(&AiBotsPage {
        base: state.config.origin(),
        cloud: state.config.mode == Mode::Cloud,
        umami: state.config.umami.clone(),
        total: reg.bots.len(),
        operators: groups.len(),
        groups,
        updated: reg.updated.clone(),
        version: reg.version,
        license: reg.license.clone(),
    })?
    .into_response())
}

async fn json() -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        registry_json(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_links_read_as_the_operator_domain() {
        assert_eq!(
            site_of("https://developers.openai.com/api/docs/bots"),
            "openai.com"
        );
        assert_eq!(site_of("https://www.bing.com/webmasters"), "bing.com");
        assert_eq!(site_of("https://commoncrawl.org/ccbot"), "commoncrawl.org");
    }
}
