//! `/robots.txt` and `/llms.txt` for the cloud domain. The static landing page used to serve
//! them; now that the app owns `/`, it does. Self-hosted instances serve neither.

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};

use super::quick::require_cloud;
use crate::error::AppError;
use crate::state::AppState;

const LLMS_TXT: &str = include_str!("../../assets/llms.txt");

/// Crawlers that get their own group in robots.txt (the AI crawlers are welcome, as on the
/// static page this replaces). A crawler with its own group ignores the `*` group, so the
/// private paths are repeated in every group.
const AGENTS: [&str; 6] = [
    "*",
    "GPTBot",
    "ClaudeBot",
    "PerplexityBot",
    "GoogleOther",
    "Googlebot",
];

/// Paths that are for one visitor or one account, not for search engines.
const PRIVATE: [&str; 6] = ["/audit/", "/s/", "/login", "/auth/", "/admin", "/go/"];

pub fn robots_body(origin: &str) -> String {
    let mut out = String::new();
    for agent in AGENTS {
        out.push_str(&format!("User-agent: {agent}\nAllow: /\n"));
        for path in PRIVATE {
            out.push_str(&format!("Disallow: {path}\n"));
        }
        out.push('\n');
    }
    out.push_str(&format!("Sitemap: {origin}/sitemap.xml\n"));
    out
}

pub async fn robots(State(state): State<AppState>) -> Result<Response, AppError> {
    require_cloud(&state)?;
    Ok((
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        robots_body(&state.config.origin()),
    )
        .into_response())
}

pub async fn llms(State(state): State<AppState>) -> Result<Response, AppError> {
    require_cloud(&state)?;
    Ok((
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        LLMS_TXT,
    )
        .into_response())
}
