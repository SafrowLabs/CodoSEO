//! The public AI bot registry: `data/ai-bots.json`, compiled in and parsed once.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const REGISTRY_JSON: &str = include_str!("../data/ai-bots.json");

/// What a bot is for. Drives the intent defaults: search and user fetches are allowed, while agents,
/// training and ads have no default preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Search,
    UserFetch,
    Agent,
    Training,
    Ads,
}

/// Whether the operator says the bot honours robots.txt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Honours {
    Yes,
    Partial,
    No,
    Unknown,
}

/// One documented AI crawler, fetcher or control token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bot {
    /// The robots.txt product token exactly as the operator writes it.
    pub token: String,
    /// Who runs it (OpenAI, Google, ...).
    pub operator: String,
    /// The product it feeds.
    pub product: String,
    pub purpose: Purpose,
    /// Whether the operator says it honours robots.txt.
    pub honours_robots: Honours,
    /// False for control tokens (Google-Extended) that never make requests.
    pub crawls: bool,
    /// A substring to look for in the User-Agent header.
    pub user_agent_contains: Option<String>,
    /// A JSON file of `prefixes`, when the operator publishes one.
    pub ip_ranges_url: Option<String>,
    /// Host suffixes for reverse-DNS verification.
    pub reverse_dns: Vec<String>,
    /// The Web Bot Auth `Signature-Agent` identity, when the operator publishes one.
    pub signature_agent: Option<String>,
    /// The operator-owned page the entry was checked against.
    pub source_url: String,
    /// When the entry was last checked against `source_url`, as `YYYY-MM-DD`.
    pub last_reviewed: String,
    /// One sentence of context.
    pub notes: String,
}

/// The whole registry file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    /// The `$schema` pointer, kept so the file round-trips.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub name: String,
    pub version: u32,
    pub updated: String,
    pub license: String,
    pub homepage: String,
    pub bots: Vec<Bot>,
}

impl Registry {
    /// Looks a bot up by robots.txt token, ignoring case.
    pub fn bot(&self, token: &str) -> Option<&Bot> {
        self.bots
            .iter()
            .find(|b| b.token.eq_ignore_ascii_case(token))
    }
}

/// The registry compiled into the binary. A malformed file panics here on first use; the registry
/// tests parse it, so that cannot reach a release.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        serde_json::from_str(REGISTRY_JSON).expect("data/ai-bots.json is not a valid registry")
    })
}

/// The raw registry file, as served at `/ai-bots.json`.
pub fn registry_json() -> &'static str {
    REGISTRY_JSON
}
