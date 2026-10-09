//! The public AI bot registry: `data/ai-bots.json`, compiled in and parsed once.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const REGISTRY_JSON: &str = include_str!("../data/ai-bots.json");

/// What a bot is for. Drives the intent defaults (block training, allow search).
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bot {
    /// The robots.txt product token exactly as the operator writes it.
    pub token: String,
    pub operator: String,
    pub product: String,
    pub purpose: Purpose,
    pub honours_robots: Honours,
    /// False for control tokens (Google-Extended) that never make requests.
    pub crawls: bool,
    pub user_agent_contains: Option<String>,
    /// A JSON file of `prefixes`, when the operator publishes one.
    pub ip_ranges_url: Option<String>,
    pub reverse_dns: Vec<String>,
    pub signature_agent: Option<String>,
    pub source_url: String,
    /// `YYYY-MM-DD`.
    pub last_reviewed: String,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
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
