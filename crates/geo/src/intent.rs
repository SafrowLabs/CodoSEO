//! What the site owner wants from each kind of AI bot, so a finding can tell a mistake from a choice.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::registry::{Bot, Purpose};

/// The owner's stance towards a bot or a whole purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stance {
    /// The bot should be able to crawl.
    Allow,
    /// The bot should be kept out by robots.txt.
    Block,
    /// No preference: never reported either way.
    Any,
}

/// Per-purpose stances with per-bot overrides. Stored as JSON on the site; missing keys mean the
/// defaults, and bot tokens the registry doesn't know are kept (the registry may grow) but ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub purposes: BTreeMap<Purpose, Stance>,
    /// Keyed by robots.txt token, matched ignoring case.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bots: BTreeMap<String, Stance>,
}

impl Intent {
    /// What a purpose gets when the owner hasn't said: search and user-triggered fetches are
    /// wanted, everything else is up to the owner.
    pub fn default_stance(purpose: Purpose) -> Stance {
        match purpose {
            Purpose::Search | Purpose::UserFetch => Stance::Allow,
            Purpose::Agent | Purpose::Training | Purpose::Ads => Stance::Any,
        }
    }

    /// The stance for a purpose: the owner's choice, else the default.
    pub fn purpose_stance(&self, purpose: Purpose) -> Stance {
        self.purposes
            .get(&purpose)
            .copied()
            .unwrap_or_else(|| Intent::default_stance(purpose))
    }

    /// The owner's override for one bot, if any.
    pub fn bot_override(&self, token: &str) -> Option<Stance> {
        self.bots
            .iter()
            .find(|(t, _)| t.eq_ignore_ascii_case(token))
            .map(|(_, s)| *s)
    }

    /// The stance that applies to `bot`: its override, else its purpose's stance.
    pub fn effective(&self, bot: &Bot) -> Stance {
        self.bot_override(&bot.token)
            .unwrap_or_else(|| self.purpose_stance(bot.purpose))
    }
}
