//! Content Signals in robots.txt (contentsignals.org): `Content-Signal: search=yes, ai-input=yes,
//! ai-train=no`, usually in the `User-agent: *` group. The robots.txt parser keeps every such line
//! ([`crate::robots::ContentSignal`]); this reads them as the policy defines: three signals, each
//! `yes` or `no`.
//!
//! The check passes when at least one line declares at least one of the three signals with a valid
//! value. It is a nice-to-have (Notice at most): the signals are declared, not enforced.

use serde::{Deserialize, Serialize};

use crate::intent::{Intent, Stance};
use crate::registry::Purpose;
use crate::robots::ContentSignal;

/// The Content Signals Policy's site.
pub const CONTENT_SIGNALS_URL: &str = "https://contentsignals.org/";
/// The signals the policy defines.
pub const SIGNALS: [&str; 3] = ["search", "ai-input", "ai-train"];
/// The line the fix suggests.
pub const SUGGESTED_LINE: &str = "Content-Signal: search=yes, ai-input=yes, ai-train=no";
/// Malformed pairs a summary lists.
const MAX_MALFORMED: usize = 10;

/// One valid signal as declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signal {
    /// `search`, `ai-input` or `ai-train`.
    pub key: String,
    /// `yes`.
    pub allowed: bool,
    /// The group's product tokens; empty when the line applies to everyone (no group, or `*`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
    pub line: u32,
}

/// The Content-Signal lines of a robots.txt, read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalsSummary {
    /// `Content-Signal` lines in the file.
    pub lines: u32,
    /// Valid signals, in file order; a key repeated for the same agents keeps its first value.
    pub signals: Vec<Signal>,
    /// Pairs that are not a known signal with `yes` or `no`, as `line 3: ai-train=maybe`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub malformed: Vec<String>,
}

impl SignalsSummary {
    /// At least one valid signal: the check passes.
    pub fn declared(&self) -> bool {
        !self.signals.is_empty()
    }

    /// The value for everyone (`User-agent: *` or no group) of a signal.
    pub fn for_everyone(&self, key: &str) -> Option<bool> {
        self.signals
            .iter()
            .find(|s| s.key == key && s.agents.is_empty())
            .map(|s| s.allowed)
    }

    /// The signals for everyone, as written: `search=yes, ai-input=yes, ai-train=no`; else those
    /// of the first group that has any, with the group named.
    pub fn line_text(&self) -> Option<String> {
        let everyone: Vec<&Signal> = self
            .signals
            .iter()
            .filter(|s| s.agents.is_empty())
            .collect();
        let (chosen, agents) = if everyone.is_empty() {
            let first = self.signals.first()?;
            (
                self.signals
                    .iter()
                    .filter(|s| s.agents == first.agents)
                    .collect::<Vec<_>>(),
                Some(first.agents.join(", ")),
            )
        } else {
            (everyone, None)
        };
        let pairs = chosen
            .iter()
            .map(|s| format!("{}={}", s.key, if s.allowed { "yes" } else { "no" }))
            .collect::<Vec<_>>()
            .join(", ");
        Some(match agents {
            Some(a) => format!("{pairs} (for {a})"),
            None => pairs,
        })
    }
}

fn applies_to_everyone(agents: &[String]) -> bool {
    agents.is_empty() || agents.iter().any(|a| a == "*")
}

/// Reads the Content-Signal lines of a robots.txt.
pub fn summarize(lines: &[ContentSignal]) -> SignalsSummary {
    let mut out = SignalsSummary {
        lines: u32::try_from(lines.len()).unwrap_or(u32::MAX),
        ..SignalsSummary::default()
    };
    for line in lines {
        let agents: Vec<String> = if applies_to_everyone(&line.agents) {
            Vec::new()
        } else {
            line.agents.clone()
        };
        if line.pairs.is_empty() && out.malformed.len() < MAX_MALFORMED {
            out.malformed
                .push(format!("line {}: no key=value pair", line.line));
        }
        for pair in &line.pairs {
            let key = pair.key.to_ascii_lowercase();
            let value = pair.value.to_ascii_lowercase();
            let valid_key = SIGNALS.contains(&key.as_str());
            let allowed = match value.as_str() {
                "yes" => Some(true),
                "no" => Some(false),
                _ => None,
            };
            match (valid_key, allowed) {
                (true, Some(allowed)) => {
                    if !out
                        .signals
                        .iter()
                        .any(|s| s.key == key && s.agents == agents)
                    {
                        out.signals.push(Signal {
                            key,
                            allowed,
                            agents: agents.clone(),
                            line: line.line,
                        });
                    }
                }
                _ if out.malformed.len() < MAX_MALFORMED => {
                    let why = if valid_key {
                        "the value must be yes or no"
                    } else {
                        "not a signal the policy defines"
                    };
                    out.malformed.push(format!(
                        "line {}: {}={} ({why})",
                        line.line, pair.key, pair.value
                    ));
                }
                _ => {}
            }
        }
    }
    out
}

/// When the robots.txt `ai-train` signal for everyone says the opposite of the owner's intent for
/// AI training bots, a sentence saying so; `None` when they agree or either is unset.
pub fn training_hint(summary: &SignalsSummary, intent: &Intent) -> Option<String> {
    let train = summary.for_everyone("ai-train")?;
    match (train, intent.purpose_stance(Purpose::Training)) {
        (false, Stance::Allow) => Some(
            "robots.txt says ai-train=no, but your intent lets AI training bots crawl: one of the two is out of date.".to_owned(),
        ),
        (true, Stance::Block) => Some(
            "robots.txt says ai-train=yes, but your intent keeps AI training bots out: one of the two is out of date.".to_owned(),
        ),
        _ => None,
    }
}
