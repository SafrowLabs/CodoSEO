//! Cloudflare Turnstile on the public forms (cloud only, and only when keys are configured):
//! the audit form, the sign-in form and an audit's unlock form. The page loads Cloudflare's
//! script, which adds a `cf-turnstile-response` field to the form; the server checks that token
//! with Cloudflare before starting a crawl or sending an email.
//!
//! Each form's widget carries a `data-action` ([`Action`]), and the server only accepts a token
//! solved for that action on this app's own hostname: a token solved on the audit form can't be
//! replayed on the sign-in form, and one solved on a copy of the page elsewhere is refused.

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

/// Which form a token was solved for: the widget's `data-action`, echoed back by Cloudflare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The landing page's audit form.
    Audit,
    /// The sign-in form.
    Login,
    /// An audit's "get the full report" form.
    Unlock,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Audit => "audit",
            Action::Login => "login",
            Action::Unlock => "unlock",
        }
    }
}

#[derive(Deserialize)]
struct SiteVerify {
    success: bool,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    hostname: Option<String>,
}

pub async fn verify(
    state: &AppState,
    action: Action,
    token: Option<&str>,
    ip: Option<IpAddr>,
) -> Verdict {
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
        Ok(v) if v.success => {
            let expected_host = state.config.base_url.host_str().unwrap_or_default();
            let action_ok = v.action.as_deref() == Some(action.as_str());
            let host_ok = v
                .hostname
                .as_deref()
                .is_some_and(|h| h.eq_ignore_ascii_case(expected_host));
            if action_ok && host_ok {
                Verdict::Passed
            } else {
                tracing::warn!(
                    expected = action.as_str(),
                    action = v.action.as_deref().unwrap_or(""),
                    hostname = v.hostname.as_deref().unwrap_or(""),
                    "turnstile token solved for another form or site"
                );
                Verdict::Failed
            }
        }
        Ok(_) => Verdict::Failed,
        Err(e) => {
            tracing::warn!(error = %e, "turnstile verification returned an unreadable answer");
            Verdict::Unavailable
        }
    }
}
