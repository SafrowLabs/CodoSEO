//! Cloudflare Turnstile on the audit form (cloud only, and only when keys are configured).
//! The page loads Cloudflare's script, which adds a `cf-turnstile-response` field to the form;
//! the server checks that token with Cloudflare before starting any crawl.

use std::net::IpAddr;

use serde::Deserialize;

use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Turnstile isn't configured, or Cloudflare confirmed the token.
    Passed,
    /// No token, or Cloudflare rejected it.
    Failed,
    /// Cloudflare couldn't be asked. The form fails closed rather than let bots through.
    Unavailable,
}

#[derive(Deserialize)]
struct SiteVerify {
    success: bool,
}

pub async fn verify(state: &AppState, token: Option<&str>, ip: Option<IpAddr>) -> Verdict {
    let Some(cfg) = &state.config.turnstile else {
        return Verdict::Passed;
    };
    // Tokens are a few hundred bytes; refuse anything absurd without a round trip.
    let Some(token) = token
        .map(str::trim)
        .filter(|t| !t.is_empty() && t.len() <= 2048)
    else {
        return Verdict::Failed;
    };
    let mut form = vec![
        ("secret", cfg.secret.clone()),
        ("response", token.to_owned()),
    ];
    if let Some(ip) = ip {
        form.push(("remoteip", ip.to_string()));
    }
    let response = state
        .http
        .post(cfg.verify_url.clone())
        .form(&form)
        .send()
        .await
        .and_then(|r| r.error_for_status());
    let body = match response {
        Ok(r) => r.json::<SiteVerify>().await,
        Err(e) => {
            tracing::warn!(error = %e, "turnstile verification unavailable");
            return Verdict::Unavailable;
        }
    };
    match body {
        Ok(v) if v.success => Verdict::Passed,
        Ok(_) => Verdict::Failed,
        Err(e) => {
            tracing::warn!(error = %e, "turnstile verification returned an unreadable answer");
            Verdict::Unavailable
        }
    }
}
