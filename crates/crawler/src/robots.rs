//! robots.txt rules for CodoSEObot, following Google's handling of groups, status
//! codes and size.
//!
//! Matching is linear: rules are `*` wildcards with an optional trailing `$`, matched
//! greedily against the path and query with plain substring search. A hostile file
//! can't cost more than [`MAX_RULES`] rules of at most [`MAX_PATTERN_LEN`] bytes each;
//! anything beyond that is skipped, not the whole file.

use std::time::Duration;

use codoseo_core::crawl::{Politeness, RobotsFile};
use url::Url;
use xxhash_rust::xxh3::xxh3_64;

use crate::fetch::{FetchError, Fetcher};

/// The product token sites use to address us in robots.txt.
pub const ROBOTS_AGENT: &str = "CodoSEObot";

/// Google ignores everything after the first 500 KiB.
const MAX_ROBOTS_BYTES: usize = 500 * 1024;
pub const MAX_RULES: usize = 2_000;
pub const MAX_PATTERN_LEN: usize = 1_024;

struct Rule {
    /// Literal pieces between `*`s; the first must match at the start of the path.
    parts: Vec<String>,
    anchored_end: bool,
    /// Pattern length as written, for longest-match precedence.
    len: usize,
    allow: bool,
}

impl Rule {
    fn new(pattern: &str, allow: bool) -> Option<Rule> {
        if pattern.is_empty() || pattern.len() > MAX_PATTERN_LEN {
            return None;
        }
        let pattern = if pattern.starts_with('/') || pattern.starts_with('*') {
            pattern.to_owned()
        } else {
            format!("/{pattern}")
        };
        let (body, anchored_end) = match pattern.strip_suffix('$') {
            Some(body) => (body, true),
            None => (pattern.as_str(), false),
        };
        Some(Rule {
            parts: body.split('*').map(str::to_owned).collect(),
            anchored_end,
            len: pattern.len(),
            allow,
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

enum Kind {
    AllowAll,
    BlockAll,
    Rules(Vec<Rule>),
}

pub struct RobotsRules {
    kind: Kind,
    delay: Option<Duration>,
    sitemaps: Vec<String>,
}

#[derive(Default)]
struct Group {
    agents: Vec<String>,
    rules: Vec<(String, bool)>,
    delay: Option<String>,
}

/// `CodoSEObot/0.1 (+https://…)` → `codoseobot`; `*` stays `*`.
fn product_token(value: &str) -> String {
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

impl RobotsRules {
    /// Parses a robots.txt body for `agent`. Lines it doesn't understand are ignored.
    pub fn parse(body: &[u8], agent: &str) -> RobotsRules {
        let text = String::from_utf8_lossy(&body[..body.len().min(MAX_ROBOTS_BYTES)]);
        let ours = product_token(agent);
        let mut groups: Vec<Group> = Vec::new();
        let mut sitemaps = Vec::new();
        let mut reading_agents = false;

        for line in text.lines() {
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
                    let Some(group) = groups.last_mut() else {
                        continue;
                    };
                    match key {
                        "allow" | "disallow" if !value.is_empty() => {
                            group.rules.push((value.to_owned(), key == "allow"));
                        }
                        "crawl-delay" if group.delay.is_none() => {
                            group.delay = Some(value.to_owned())
                        }
                        _ => {}
                    }
                }
            }
        }

        let named: Vec<&Group> = groups.iter().filter(|g| g.agents.contains(&ours)).collect();
        let chosen = if named.is_empty() {
            groups
                .iter()
                .filter(|g| g.agents.iter().any(|a| a == "*"))
                .collect()
        } else {
            named
        };
        let rules: Vec<Rule> = chosen
            .iter()
            .flat_map(|g| g.rules.iter())
            .filter_map(|(pattern, allow)| Rule::new(pattern, *allow))
            .take(MAX_RULES)
            .collect();
        let delay = chosen
            .iter()
            .find_map(|g| g.delay.as_deref())
            .and_then(parse_delay);
        RobotsRules {
            kind: Kind::Rules(rules),
            delay,
            sitemaps,
        }
    }

    /// Rules for a robots.txt that didn't return 2xx. Like Google: a 4xx (except 429)
    /// means there are no rules; 429 and 5xx mean the whole site is off limits for now.
    pub fn from_status(status: u16) -> RobotsRules {
        let kind = if (400..500).contains(&status) && status != 429 {
            Kind::AllowAll
        } else {
            Kind::BlockAll
        };
        RobotsRules {
            kind,
            delay: None,
            sitemaps: Vec::new(),
        }
    }

    pub fn from_response(status: u16, body: &[u8], agent: &str) -> RobotsRules {
        if (200..300).contains(&status) {
            RobotsRules::parse(body, agent)
        } else {
            RobotsRules::from_status(status)
        }
    }

    /// Takes an absolute URL or a path (with optional query).
    pub fn allowed(&self, url_or_path: &str) -> bool {
        let rules = match &self.kind {
            Kind::AllowAll => return true,
            Kind::BlockAll => return false,
            Kind::Rules(rules) => rules,
        };
        let owned;
        let path = match Url::parse(url_or_path) {
            Ok(url) => {
                owned = match url.query() {
                    Some(q) => format!("{}?{q}", url.path()),
                    None => url.path().to_owned(),
                };
                owned.as_str()
            }
            Err(_) => url_or_path,
        };
        if path == "/robots.txt" {
            return true;
        }
        let best = rules
            .iter()
            .filter(|r| r.matches(path))
            .max_by_key(|r| (r.len, r.allow));
        best.is_none_or(|r| r.allow)
    }

    /// True when the homepage itself is off limits, so there is nothing to crawl.
    pub fn blocks_everything(&self) -> bool {
        !self.allowed("/")
    }

    /// `Crawl-delay`, capped at 10 seconds.
    pub fn crawl_delay(&self) -> Option<Duration> {
        self.delay
    }

    pub fn sitemaps(&self) -> &[String] {
        &self.sitemaps
    }
}

fn parse_delay(value: &str) -> Option<Duration> {
    let max = Politeness::default().max_crawl_delay;
    let secs: f64 = value.trim().parse().ok()?;
    if !secs.is_finite() || secs <= 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(secs.min(max.as_secs_f64())))
}

/// Fetches `/robots.txt` for the site `url` belongs to. Connection-level failures
/// are returned as errors; HTTP error statuses become rules.
pub async fn fetch_robots(
    fetcher: &Fetcher,
    url: &Url,
) -> Result<(RobotsRules, RobotsFile), FetchError> {
    let robots_url = url
        .join("/robots.txt")
        .map_err(|e| FetchError::Http(format!("bad robots.txt URL: {e}")))?;
    let res = fetcher.fetch_raw(&robots_url, MAX_ROBOTS_BYTES).await?;
    let ok = (200..300).contains(&res.status);
    let body = if ok {
        res.body.unwrap_or_default()
    } else {
        Default::default()
    };
    let rules = RobotsRules::from_response(res.status, &body, ROBOTS_AGENT);
    let file = RobotsFile {
        status: res.status,
        body: String::from_utf8_lossy(&body).into_owned(),
        hash: xxh3_64(&body),
    };
    Ok((rules, file))
}
