//! The robots.txt parser, following Google's handling of groups, status codes and size
//! (RFC 9309), plus the AI-use preferences that ride along in the same file.
//!
//! Matching is linear: rules are `*` wildcards with an optional trailing `$`, matched
//! greedily against the path and query with plain substring search. A hostile file
//! can't cost more than [`MAX_RULES`] rules of at most [`MAX_PATTERN_LEN`] bytes each;
//! anything beyond that is skipped, not the whole file.

use serde::{Deserialize, Serialize};

use crate::declared::{is_known_signal_key, is_known_usage_key};
use crate::declared::{parse_content_signal, parse_content_usage};

/// Google ignores everything after the first 500 KiB.
pub const MAX_ROBOTS_BYTES: usize = 500 * 1024;
pub const MAX_RULES: usize = 2_000;
pub const MAX_PATTERN_LEN: usize = 1_024;

/// `CodoSEObot/0.1 (+https://…)` → `codoseobot`; `*` stays `*`.
pub fn product_token(value: &str) -> String {
    let value = value.trim();
    if value.starts_with('*') {
        return "*".to_owned();
    }
    value
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect::<String>()
        .to_ascii_lowercase()
}

struct Rule {
    /// Literal pieces between `*`s; the first must match at the start of the path.
    parts: Vec<String>,
    anchored_end: bool,
    /// Pattern length as written, for longest-match precedence.
    len: usize,
    allow: bool,
    line: u32,
    /// The pattern as written in the file.
    pattern: String,
}

impl Rule {
    fn new(pattern: &str, allow: bool, line: u32) -> Option<Rule> {
        if pattern.is_empty() || pattern.len() > MAX_PATTERN_LEN {
            return None;
        }
        let normalised = if pattern.starts_with('/') || pattern.starts_with('*') {
            pattern.to_owned()
        } else {
            format!("/{pattern}")
        };
        let (body, anchored_end) = match normalised.strip_suffix('$') {
            Some(body) => (body, true),
            None => (normalised.as_str(), false),
        };
        Some(Rule {
            parts: body.split('*').map(str::to_owned).collect(),
            anchored_end,
            len: normalised.len(),
            allow,
            line,
            pattern: pattern.to_owned(),
        })
    }

    fn matches(&self, path: &str) -> bool {
        let (first, rest) = self
            .parts
            .split_first()
            .expect("split always yields one part");
        let Some(mut remaining) = path.strip_prefix(first.as_str()) else {
            return false;
        };
        let Some((last, middle)) = rest.split_last() else {
            return !self.anchored_end || remaining.is_empty();
        };
        for part in middle {
            match remaining.find(part.as_str()) {
                Some(at) => remaining = &remaining[at + part.len()..],
                None => return false,
            }
        }
        if self.anchored_end {
            remaining.len() >= last.len() && remaining.ends_with(last.as_str())
        } else {
            remaining.contains(last.as_str())
        }
    }
}

#[derive(Default)]
struct Group {
    agents: Vec<String>,
    rules: Vec<Rule>,
    delay: Option<String>,
}

/// One `key=value` pair of a declared preference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair {
    /// Lower-cased.
    pub key: String,
    /// As written, trimmed.
    pub value: String,
    /// False when the key is not one the spec defines; the pair is kept regardless.
    pub known: bool,
}

/// A `Content-Signal:` line (Cloudflare Content Signals).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentSignal {
    /// 1-based line number.
    pub line: u32,
    /// Product tokens of the group the line sits in; empty when it sits in no group,
    /// which means it applies to everyone.
    pub agents: Vec<String>,
    pub pairs: Vec<Pair>,
}

/// A `Content-Usage:` line (IETF AIPREF attach draft).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentUsage {
    pub line: u32,
    pub agents: Vec<String>,
    /// The optional leading path (starts with `/`) the preference is limited to.
    pub path: Option<String>,
    pub pairs: Vec<Pair>,
}

fn to_pairs(raw: Vec<(String, String)>, known: fn(&str) -> bool) -> Vec<Pair> {
    raw.into_iter()
        .map(|(key, value)| Pair {
            known: known(&key),
            key,
            value,
        })
        .collect()
}

/// A parsed robots.txt.
pub struct RobotsTxt {
    groups: Vec<Group>,
    sitemaps: Vec<String>,
    content_signals: Vec<ContentSignal>,
    content_usage: Vec<ContentUsage>,
}

/// Which group of the file decided a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupMatch {
    /// A group names the token.
    Named,
    /// No group names it, the `*` groups apply.
    Wildcard,
    /// No group applies: everything is allowed.
    None,
}

/// The rule that won, with its 1-based line in the file and the pattern as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchedRule {
    pub line: u32,
    pub allow: bool,
    pub pattern: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub allowed: bool,
    pub group: GroupMatch,
    /// `None` when no rule matched (or the path is `/robots.txt`).
    pub rule: Option<MatchedRule>,
}

impl RobotsTxt {
    /// Parses a robots.txt body. Lines it doesn't understand are ignored.
    pub fn parse(body: &[u8]) -> RobotsTxt {
        let text = String::from_utf8_lossy(&body[..body.len().min(MAX_ROBOTS_BYTES)]);
        let mut groups: Vec<Group> = Vec::new();
        let mut sitemaps = Vec::new();
        let mut content_signals = Vec::new();
        let mut content_usage = Vec::new();
        let mut reading_agents = false;

        for (index, line) in text.lines().enumerate() {
            let number = u32::try_from(index + 1).unwrap_or(u32::MAX);
            let line = line.split('#').next().unwrap_or("");
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key.trim().to_ascii_lowercase().as_str() {
                "user-agent" => {
                    if !reading_agents {
                        groups.push(Group::default());
                    }
                    reading_agents = true;
                    if let Some(group) = groups.last_mut() {
                        group.agents.push(product_token(value));
                    }
                }
                "sitemap" => sitemaps.push(value.to_owned()),
                key => {
                    reading_agents = false;
                    let agents = groups.last().map(|g| g.agents.clone()).unwrap_or_default();
                    match key {
                        "content-signal" => content_signals.push(ContentSignal {
                            line: number,
                            agents,
                            pairs: to_pairs(parse_content_signal(value), is_known_signal_key),
                        }),
                        "content-usage" => {
                            let (path, pairs) = parse_content_usage(value);
                            content_usage.push(ContentUsage {
                                line: number,
                                agents,
                                path,
                                pairs: to_pairs(pairs, is_known_usage_key),
                            });
                        }
                        _ => {}
                    }
                    let Some(group) = groups.last_mut() else {
                        continue;
                    };
                    match key {
                        "allow" | "disallow" if !value.is_empty() => {
                            if let Some(rule) = Rule::new(value, key == "allow", number) {
                                group.rules.push(rule);
                            }
                        }
                        "crawl-delay" if group.delay.is_none() => {
                            group.delay = Some(value.to_owned())
                        }
                        _ => {}
                    }
                }
            }
        }
        RobotsTxt {
            groups,
            sitemaps,
            content_signals,
            content_usage,
        }
    }

    /// The groups that apply to `token`: every group naming it, else every `*` group.
    fn chosen(&self, token: &str) -> (GroupMatch, Vec<&Group>) {
        let token = product_token(token);
        let named: Vec<&Group> = self
            .groups
            .iter()
            .filter(|g| g.agents.contains(&token))
            .collect();
        if !named.is_empty() {
            return (GroupMatch::Named, named);
        }
        let wild: Vec<&Group> = self
            .groups
            .iter()
            .filter(|g| g.agents.iter().any(|a| a == "*"))
            .collect();
        if wild.is_empty() {
            (GroupMatch::None, wild)
        } else {
            (GroupMatch::Wildcard, wild)
        }
    }

    /// May `token` fetch `path` (with its query, if any)? The longest matching pattern
    /// wins, `Allow` wins ties, and `/robots.txt` is always allowed.
    pub fn verdict(&self, token: &str, path: &str) -> Verdict {
        let (group, chosen) = self.chosen(token);
        if path == "/robots.txt" {
            return Verdict {
                allowed: true,
                group,
                rule: None,
            };
        }
        let mut best: Option<&Rule> = None;
        for rule in chosen
            .iter()
            .flat_map(|g| g.rules.iter())
            .take(MAX_RULES)
            .filter(|r| r.matches(path))
        {
            if best.is_none_or(|b| (rule.len, rule.allow) > (b.len, b.allow)) {
                best = Some(rule);
            }
        }
        Verdict {
            allowed: best.is_none_or(|r| r.allow),
            group,
            rule: best.map(|r| MatchedRule {
                line: r.line,
                allow: r.allow,
                pattern: r.pattern.clone(),
            }),
        }
    }

    /// The first `Crawl-delay` of the groups that apply to `token`, in seconds. Not
    /// capped here; positive and finite only.
    pub fn crawl_delay(&self, token: &str) -> Option<f64> {
        let (_, chosen) = self.chosen(token);
        let secs: f64 = chosen
            .iter()
            .find_map(|g| g.delay.as_deref())?
            .trim()
            .parse()
            .ok()?;
        (secs.is_finite() && secs > 0.0).then_some(secs)
    }

    pub fn sitemaps(&self) -> &[String] {
        &self.sitemaps
    }

    pub fn content_signals(&self) -> &[ContentSignal] {
        &self.content_signals
    }

    pub fn content_usage(&self) -> &[ContentUsage] {
        &self.content_usage
    }
}

/// What the robots.txt response means for crawling, as Google reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RobotsAvailability {
    /// 2xx: the rules in the file apply.
    Ok,
    /// 4xx except 429: no rules, everything is allowed.
    Missing,
    /// 429, 5xx (and anything else that is not a 2xx or 4xx): the whole site is off
    /// limits for now.
    Unavailable,
    /// Not fetched.
    Unknown,
}

pub fn availability(status: Option<u16>) -> RobotsAvailability {
    match status {
        None => RobotsAvailability::Unknown,
        Some(s) if (200..300).contains(&s) => RobotsAvailability::Ok,
        Some(s) if (400..500).contains(&s) && s != 429 => RobotsAvailability::Missing,
        Some(_) => RobotsAvailability::Unavailable,
    }
}
