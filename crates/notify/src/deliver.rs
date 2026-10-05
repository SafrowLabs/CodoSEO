//! Delivering an alert to one channel, and checking a channel's target when it is saved.
//!
//! HTTP channels (Slack, Discord, webhook) are user-supplied URLs, so every request goes through
//! the crawler's address guard: [`check_url`] on the URL, a client whose resolver drops private
//! and internal addresses, no redirects (a public URL that 302s to `localhost` is an error, never
//! followed), no proxy, and a 10 second timeout.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use codoseo_core::crawl::AddressPolicy;
use codoseo_crawler::guard::{GuardError, GuardedResolver, Lookup, SystemLookup, check_url};
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::email::{MailError, Mailer};
use crate::message::{self, AlertMessage};
use crate::{discord, slack, webhook};

const TIMEOUT: Duration = Duration::from_secs(10);
/// How much of an error response is kept for the channel's `last_error`.
const BODY_EXCERPT: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Email,
    Slack,
    Discord,
    Webhook,
}

impl ChannelKind {
    /// The slug stored in `alert_channels.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            ChannelKind::Email => "email",
            ChannelKind::Slack => "slack",
            ChannelKind::Discord => "discord",
            ChannelKind::Webhook => "webhook",
        }
    }

    pub fn parse(slug: &str) -> Option<ChannelKind> {
        match slug {
            "email" => Some(ChannelKind::Email),
            "slack" => Some(ChannelKind::Slack),
            "discord" => Some(ChannelKind::Discord),
            "webhook" => Some(ChannelKind::Webhook),
            _ => None,
        }
    }
}

/// Where a channel delivers to. Slack and Discord URLs carry a token, and a webhook has a secret,
/// so `Debug` shows hosts only.
#[derive(Clone, PartialEq, Eq)]
pub enum ChannelTarget {
    Email { to: String },
    Slack { url: Url },
    Discord { url: Url },
    Webhook { url: Url, secret: String },
}

impl std::fmt::Debug for ChannelTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ChannelTarget::{:?}({})", self.kind(), self.display())
    }
}

impl ChannelTarget {
    pub fn kind(&self) -> ChannelKind {
        match self {
            ChannelTarget::Email { .. } => ChannelKind::Email,
            ChannelTarget::Slack { .. } => ChannelKind::Slack,
            ChannelTarget::Discord { .. } => ChannelKind::Discord,
            ChannelTarget::Webhook { .. } => ChannelKind::Webhook,
        }
    }

    /// What may be shown on screen: the address for email, only the host for URLs (the path of a
    /// Slack or Discord URL is its secret), never a secret.
    pub fn display(&self) -> String {
        match self {
            ChannelTarget::Email { to } => to.clone(),
            ChannelTarget::Slack { url }
            | ChannelTarget::Discord { url }
            | ChannelTarget::Webhook { url, .. } => url.host_str().unwrap_or("").to_owned(),
        }
    }

    /// The JSON that is encrypted at rest: `{to}`, `{url}` or `{url, secret}`.
    pub fn to_json(&self) -> Value {
        match self {
            ChannelTarget::Email { to } => json!({ "to": to }),
            ChannelTarget::Slack { url } | ChannelTarget::Discord { url } => {
                json!({ "url": url.as_str() })
            }
            ChannelTarget::Webhook { url, secret } => {
                json!({ "url": url.as_str(), "secret": secret })
            }
        }
    }

    /// Reverses [`ChannelTarget::to_json`].
    pub fn from_json(kind: ChannelKind, value: &Value) -> Result<ChannelTarget, TargetError> {
        let field = |name: &str| {
            value
                .get(name)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| TargetError::Invalid(format!("the target has no `{name}`")))
        };
        let url = || {
            Url::parse(field("url")?)
                .map_err(|e| TargetError::Invalid(format!("the target URL is invalid: {e}")))
        };
        Ok(match kind {
            ChannelKind::Email => ChannelTarget::Email {
                to: field("to")?.to_owned(),
            },
            ChannelKind::Slack => ChannelTarget::Slack { url: url()? },
            ChannelKind::Discord => ChannelTarget::Discord { url: url()? },
            ChannelKind::Webhook => ChannelTarget::Webhook {
                url: url()?,
                secret: field("secret")?.to_owned(),
            },
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TargetError {
    #[error("{0}")]
    Invalid(String),
    #[error("that address is not allowed: {0}")]
    Blocked(#[from] GuardError),
}

/// Checks a URL a user wants to save as a channel target: Slack must be
/// `https://hooks.slack.com/...`, Discord `https://discord.com/api/webhooks/...` (or
/// `discordapp.com`), a webhook any `https` URL (plain `http` only under `AllowPrivate`); then
/// the address guard. The same guard runs again at delivery.
pub fn validate_target(
    kind: ChannelKind,
    url: &str,
    policy: AddressPolicy,
) -> Result<Url, TargetError> {
    let bad = |msg: &str| TargetError::Invalid(msg.to_owned());
    let url = Url::parse(url.trim()).map_err(|_| bad("that doesn't look like a web address"))?;
    let has_host = |hosts: &[&str]| url.host_str().is_some_and(|h| hosts.contains(&h));
    match kind {
        ChannelKind::Email => return Err(bad("email channels take an address, not a URL")),
        ChannelKind::Slack => {
            if url.scheme() != "https" || !has_host(&["hooks.slack.com"]) || url.path().len() < 2 {
                return Err(bad(
                    "a Slack webhook looks like https://hooks.slack.com/services/...",
                ));
            }
        }
        ChannelKind::Discord => {
            if url.scheme() != "https"
                || !has_host(&["discord.com", "discordapp.com"])
                || !url.path().starts_with("/api/webhooks/")
            {
                return Err(bad(
                    "a Discord webhook looks like https://discord.com/api/webhooks/...",
                ));
            }
        }
        ChannelKind::Webhook => {
            let scheme_ok = url.scheme() == "https"
                || (url.scheme() == "http" && policy == AddressPolicy::AllowPrivate);
            if !scheme_ok || url.host_str().is_none() {
                return Err(bad("a webhook URL must start with https://"));
            }
        }
    }
    check_url(&url, policy)?;
    Ok(url)
}

#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    /// The address guard refused the URL; nothing was sent.
    #[error("refused: {0}")]
    Blocked(#[from] GuardError),
    /// The request could not be made (DNS refused by the guard, connection, timeout, ...).
    #[error("request failed: {0}")]
    Request(String),
    /// The server answered something other than 2xx (a redirect counts: it is not followed).
    #[error("the server answered {status}: {body}")]
    Status { status: u16, body: String },
    #[error(transparent)]
    Mail(#[from] MailError),
}

/// The client HTTP channels are delivered with, resolving names through the system resolver.
/// Build it once and keep it. `Public` drops private and internal addresses from DNS answers;
/// `AllowPrivate` (self-hosted) resolves normally.
pub fn guarded_client(policy: AddressPolicy) -> Result<reqwest::Client, reqwest::Error> {
    guarded_client_with(policy, SystemLookup)
}

/// [`guarded_client`] with another resolver (tests resolve names to chosen addresses).
pub fn guarded_client_with<L: Lookup>(
    policy: AddressPolicy,
    lookup: L,
) -> Result<reqwest::Client, reqwest::Error> {
    let mut builder = reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(TIMEOUT)
        .user_agent("CodoSEO-Notify")
        .no_proxy();
    if policy == AddressPolicy::Public {
        builder = builder.dns_resolver(GuardedResolver::new(lookup));
    }
    builder.build()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Delivers `msg` to one channel. `http` must come from [`guarded_client`] (a plain client would
/// skip the DNS guard); `policy` is the same policy it was built with. The URL is checked first.
pub async fn deliver(
    http: &reqwest::Client,
    policy: AddressPolicy,
    mailer: &Mailer,
    target: &ChannelTarget,
    msg: &AlertMessage,
) -> Result<(), DeliveryError> {
    match target {
        ChannelTarget::Email { to } => Ok(mailer.send(message::email(msg, to)).await?),
        ChannelTarget::Slack { url } => post(http, policy, url, &slack::payload(msg), None).await,
        ChannelTarget::Discord { url } => {
            post(http, policy, url, &discord::payload(msg), None).await
        }
        ChannelTarget::Webhook { url, secret } => {
            post(http, policy, url, &webhook::payload(msg), Some(secret)).await
        }
    }
}

async fn post(
    http: &reqwest::Client,
    policy: AddressPolicy,
    url: &Url,
    payload: &Value,
    sign_with: Option<&str>,
) -> Result<(), DeliveryError> {
    check_url(url, policy)?;
    let body = serde_json::to_vec(payload).expect("a JSON value always serializes");
    let mut request = http
        .post(url.clone())
        .header(reqwest::header::CONTENT_TYPE, "application/json");
    if let Some(secret) = sign_with {
        let timestamp = unix_now();
        request = request
            .header(webhook::TIMESTAMP_HEADER, timestamp.to_string())
            .header(
                webhook::SIGNATURE_HEADER,
                webhook::sign(secret, timestamp, &body),
            );
    }
    // The error text must not carry the URL: Slack and Discord URLs hold their token.
    let mut response = request
        .body(body)
        .send()
        .await
        .map_err(|e| DeliveryError::Request(e.without_url().to_string()))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let excerpt = match response.chunk().await {
        Ok(Some(chunk)) => String::from_utf8_lossy(&chunk).into_owned(),
        _ => String::new(),
    };
    Err(DeliveryError::Status {
        status: status.as_u16(),
        body: message::truncate(&message::one_line(&excerpt), BODY_EXCERPT),
    })
}
