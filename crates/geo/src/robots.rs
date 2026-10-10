//! The robots.txt parser, following Google's handling of groups, status codes and size
//! (RFC 9309), plus the AI-use preferences that ride along in the same file.
//!
//! Matching is linear: rules are `*` wildcards with an optional trailing `$`, matched
//! greedily against the path and query with plain substring search. A hostile file
//! can't cost more than [`MAX_RULES`] rules of at most [`MAX_PATTERN_LEN`] bytes each;
//! anything beyond that is skipped, not the whole file.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::declared::{clean_declared, is_known_signal_key, is_known_usage_key};
use crate::declared::{parse_content_signal, parse_content_usage};

/// Google ignores everything after the first 500 KiB.
pub const MAX_ROBOTS_BYTES: usize = 500 * 1024;
pub const MAX_RULES: usize = 2_000;
pub const MAX_PATTERN_LEN: usize = 1_024;
/// Most `Content-Signal` and `Content-Usage` lines kept, each: they copy the agents of
/// their group, so an unbounded count would cost quadratic memory on a hostile file.
const MAX_DECLARED_LINES: usize = 100;
/// Most agents a declared line copies from its group; the rest are only counted.
pub const MAX_DECLARED_AGENTS: usize = 20;
/// Longest product token a declared line copies, in characters.
const MAX_DECLARED_AGENT_LEN: usize = 64;
/// Most `key=value` pairs kept per declared line.
pub const MAX_DECLARED_PAIRS: usize = 20;
/// Declared lines stop being kept once the text they hold passes this many bytes, so a
/// hostile file can't make a stored report large whatever its line count.
const MAX_DECLARED_BYTES: usize = 8 * 1024;

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
        let mut parts: Vec<String> = body.split('*').map(str::to_owned).collect();
        // Runs of `*` match the same as one; dropping the empty pieces keeps a hostile
        // `*****…` pattern from costing a step per star on every verdict.
        let last = parts.len() - 1;
        let mut index = 0;
        parts.retain(|part| {
            let keep = index == 0 || index == last || !part.is_empty();
            index += 1;
            keep
        });
        Some(Rule {
            parts,
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

struct Group {
    agents: Vec<String>,
    /// The group's rules in `RobotsTxt::rules`: only the newest group gets rules, so they are
    /// one contiguous run.
    rules: Range<usize>,
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
    /// Product tokens of the group the line sits in (the first [`MAX_DECLARED_AGENTS`]);
    /// empty when it sits in no group, which means it applies to everyone.
    pub agents: Vec<String>,
    /// How many more agents the group names.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub more_agents: u32,
    /// At most [`MAX_DECLARED_PAIRS`].
    pub pairs: Vec<Pair>,
}

/// A `Content-Usage:` line (IETF AIPREF attach draft).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentUsage {
    pub line: u32,
    pub agents: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub more_agents: u32,
    /// The optional leading path (starts with `/`) the preference is limited to.
    pub path: Option<String>,
    pub pairs: Vec<Pair>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// The pairs of a declared line as they are kept: at most [`MAX_DECLARED_PAIRS`], keys and
/// values cleaned by [`clean_declared`]. Shared by robots.txt lines and response headers.
pub fn declared_pairs(raw: Vec<(String, String)>, known: fn(&str) -> bool) -> Vec<Pair> {
    raw.into_iter()
        .map(|(key, value)| (clean_declared(&key), clean_declared(&value)))
        .filter(|(key, value)| !key.is_empty() && !value.is_empty())
        .take(MAX_DECLARED_PAIRS)
        .map(|(key, value)| Pair {
            known: known(&key),
            key,
            value,
        })
        .collect()
}

/// The agents a declared line copies from its group: at most [`MAX_DECLARED_AGENTS`] tokens of
/// at most [`MAX_DECLARED_AGENT_LEN`] characters, and how many more there are.
fn declared_agents(group: Option<&Group>) -> (Vec<String>, u32) {
    let Some(group) = group else {
        return (Vec::new(), 0);
    };
    let agents = group
        .agents
        .iter()
        .take(MAX_DECLARED_AGENTS)
        .map(|a| a.chars().take(MAX_DECLARED_AGENT_LEN).collect())
        .collect();
    let more = group.agents.len().saturating_sub(MAX_DECLARED_AGENTS);
    (agents, u32::try_from(more).unwrap_or(u32::MAX))
}

/// What a declared line costs against [`MAX_DECLARED_BYTES`].
fn declared_bytes(agents: &[String], path: Option<&str>, pairs: &[Pair]) -> usize {
    agents.iter().map(String::len).sum::<usize>()
        + path.map_or(0, str::len)
        + pairs
            .iter()
            .map(|p| p.key.len() + p.value.len())
            .sum::<usize>()
}

/// A parsed robots.txt.
pub struct RobotsTxt {
    groups: Vec<Group>,
    /// Every group's rules, in file order.
    rules: Vec<Rule>,
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
    /// No group names the token, but groups name the token its operator says it follows
    /// instead (Applebot follows Googlebot's rules).
    Fallback,
    /// No group names it, the `*` groups apply.
    Wildcard,
    /// No group applies: everything is allowed.
    None,
}

/// The rules that apply to one product token, chosen once so that repeated verdicts for the
/// same token don't select the groups again. It points into the [`RobotsTxt`] it came from
/// and means nothing with another file.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRules {
    group: GroupMatch,
    /// Indexes into `RobotsTxt::rules`, in file order, at most [`MAX_RULES`].
    rules: Vec<u32>,
    delay: Option<f64>,
}

impl AgentRules {
    /// Which groups were chosen.
    pub fn group(&self) -> GroupMatch {
        self.group
    }

    /// The first `Crawl-delay` of the chosen groups, in seconds. Not capped here; positive
    /// and finite only.
    pub fn crawl_delay(&self) -> Option<f64> {
        self.delay
    }
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
    /// The rules a robots.txt response gives, or `None` when there are no verdicts to speak of
    /// (a 5xx, 429 or a connection failure). A missing file (4xx, or a redirect that never
    /// reached one) parses as an empty one.
    pub fn from_response(status: Option<u16>, body: &[u8]) -> Option<RobotsTxt> {
        match availability(status) {
            RobotsAvailability::Ok => Some(RobotsTxt::parse(body)),
            RobotsAvailability::Missing => Some(RobotsTxt::parse(b"")),
            _ => None,
        }
    }

    /// Parses a robots.txt body. Lines it doesn't understand are ignored.
    pub fn parse(body: &[u8]) -> RobotsTxt {
        let text = String::from_utf8_lossy(&body[..body.len().min(MAX_ROBOTS_BYTES)]);
        // A byte order mark is not part of the first line's field name.
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let mut groups: Vec<Group> = Vec::new();
        let mut rules: Vec<Rule> = Vec::new();
        let mut sitemaps = Vec::new();
        let mut content_signals = Vec::new();
        let mut content_usage = Vec::new();
        let mut declared = 0usize;
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
                        groups.push(Group {
                            agents: Vec::new(),
                            rules: rules.len()..rules.len(),
                            delay: None,
                        });
                    }
                    reading_agents = true;
                    if let Some(group) = groups.last_mut() {
                        group.agents.push(product_token(value));
                    }
                }
                "sitemap" => sitemaps.push(value.to_owned()),
                key => {
                    reading_agents = false;
                    let room = declared < MAX_DECLARED_BYTES;
                    match key {
                        "content-signal" if room && content_signals.len() < MAX_DECLARED_LINES => {
                            let (agents, more_agents) = declared_agents(groups.last());
                            let pairs =
                                declared_pairs(parse_content_signal(value), is_known_signal_key);
                            declared += declared_bytes(&agents, None, &pairs);
                            content_signals.push(ContentSignal {
                                line: number,
                                agents,
                                more_agents,
                                pairs,
                            });
                        }
                        "content-usage" if room && content_usage.len() < MAX_DECLARED_LINES => {
                            let (agents, more_agents) = declared_agents(groups.last());
                            let (path, pairs) = parse_content_usage(value);
                            let path = path.map(|p| clean_declared(&p));
                            let pairs = declared_pairs(pairs, is_known_usage_key);
                            declared += declared_bytes(&agents, path.as_deref(), &pairs);
                            content_usage.push(ContentUsage {
                                line: number,
                                agents,
                                more_agents,
                                path,
                                pairs,
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
                                rules.push(rule);
                                group.rules.end = rules.len();
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
            rules,
            sitemaps,
            content_signals,
            content_usage,
        }
    }

    /// Chooses the groups that apply to `token`, as Google does: every group naming it, else
    /// every `*` group. `fallback` is the token the bot's operator says it follows when no
    /// group names it (Applebot follows Googlebot); its groups come before the `*` ones.
    /// Resolve once per token and keep the result: this walks every group.
    pub fn resolve(&self, token: &str, fallback: Option<&str>) -> AgentRules {
        let named = |token: &str| -> Vec<&Group> {
            let token = product_token(token);
            self.groups
                .iter()
                .filter(|g| g.agents.contains(&token))
                .collect()
        };
        let mut chosen = named(token);
        let mut group = GroupMatch::Named;
        if chosen.is_empty()
            && let Some(fallback) = fallback
        {
            chosen = named(fallback);
            group = GroupMatch::Fallback;
        }
        if chosen.is_empty() {
            chosen = self
                .groups
                .iter()
                .filter(|g| g.agents.iter().any(|a| a == "*"))
                .collect();
            group = if chosen.is_empty() {
                GroupMatch::None
            } else {
                GroupMatch::Wildcard
            };
        }
        let rules = chosen
            .iter()
            .flat_map(|g| g.rules.clone())
            .take(MAX_RULES)
            .map(|i| i as u32)
            .collect();
        let delay = chosen
            .iter()
            .find_map(|g| g.delay.as_deref())
            .and_then(|d| d.trim().parse::<f64>().ok())
            .filter(|secs| secs.is_finite() && *secs > 0.0);
        AgentRules {
            group,
            rules,
            delay,
        }
    }

    /// The longest matching rule of `agent` for `path`; `Allow` wins ties.
    fn best(&self, agent: &AgentRules, path: &str) -> Option<&Rule> {
        let mut best: Option<&Rule> = None;
        for rule in agent
            .rules
            .iter()
            .filter_map(|i| self.rules.get(*i as usize))
            .filter(|r| r.matches(path))
        {
            if best.is_none_or(|b| (rule.len, rule.allow) > (b.len, b.allow)) {
                best = Some(rule);
            }
        }
        best
    }

    /// May the agent fetch `path` (with its query, if any)? The longest matching pattern wins,
    /// `Allow` wins ties, and `/robots.txt` is always allowed.
    pub fn verdict_for(&self, agent: &AgentRules, path: &str) -> Verdict {
        let best = if path == "/robots.txt" {
            None
        } else {
            self.best(agent, path)
        };
        Verdict {
            allowed: best.is_none_or(|r| r.allow),
            group: agent.group,
            rule: best.map(|r| MatchedRule {
                line: r.line,
                allow: r.allow,
                pattern: r.pattern.clone(),
            }),
        }
    }

    /// [`verdict_for`](Self::verdict_for) without the rule: allocates nothing.
    pub fn allowed_for(&self, agent: &AgentRules, path: &str) -> bool {
        path == "/robots.txt" || self.best(agent, path).is_none_or(|r| r.allow)
    }

    /// May `token` fetch `path`? For one question; resolve the token once when asking many.
    pub fn verdict(&self, token: &str, path: &str) -> Verdict {
        self.verdict_for(&self.resolve(token, None), path)
    }

    /// The first `Crawl-delay` of the groups that apply to `token`, in seconds. Not
    /// capped here; positive and finite only.
    pub fn crawl_delay(&self, token: &str) -> Option<f64> {
        self.resolve(token, None).delay
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

/// What the robots.txt response means for crawling, as Google and RFC 9309 read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RobotsAvailability {
    /// 2xx: the rules in the file apply.
    Ok,
    /// 4xx except 429: no rules, everything is allowed. A final 3xx (redirects that never
    /// reached a file) counts the same: RFC 9309 lets a crawler treat it as unavailable in the
    /// 4xx sense.
    Missing,
    /// 429, 5xx (and anything else that is not a 2xx, 3xx or 4xx): the whole site is off
    /// limits for now.
    Unavailable,
    /// Not fetched.
    Unknown,
}

pub fn availability(status: Option<u16>) -> RobotsAvailability {
    match status {
        None => RobotsAvailability::Unknown,
        Some(s) if (200..300).contains(&s) => RobotsAvailability::Ok,
        Some(s) if (300..400).contains(&s) => RobotsAvailability::Missing,
        Some(s) if (400..500).contains(&s) && s != 429 => RobotsAvailability::Missing,
        Some(_) => RobotsAvailability::Unavailable,
    }
}
