//! robots.txt rules for CodoSEObot, following Google's handling of groups, status
//! codes and size.
//!
//! The parsing and matching live in [`codoseo_geo::robots`], shared with the AI-access
//! report; this wrapper adds the status handling, URL-or-path input and the crawl-delay
//! cap the crawler needs, and chooses our groups once, not on every URL.

use std::time::Duration;

use codoseo_core::crawl::{Politeness, RobotsFile};
use codoseo_geo::robots::{AgentRules, MAX_ROBOTS_BYTES, RobotsTxt};
use url::Url;
use xxhash_rust::xxh3::xxh3_64;

use crate::fetch::{FetchError, Fetcher};

pub use codoseo_geo::robots::{MAX_PATTERN_LEN, MAX_RULES};

/// The product token sites use to address us in robots.txt.
pub const ROBOTS_AGENT: &str = "CodoSEObot";

enum Kind {
    AllowAll,
    BlockAll,
    /// The parsed file and the rules it gives the product token it was read for.
    Rules(Box<RobotsTxt>, AgentRules),
}

pub struct RobotsRules {
    kind: Kind,
    delay: Option<Duration>,
    sitemaps: Vec<String>,
}

impl RobotsRules {
    /// Parses a robots.txt body for `agent`. Lines it doesn't understand are ignored.
    pub fn parse(body: &[u8], agent: &str) -> RobotsRules {
        let txt = RobotsTxt::parse(body);
        let rules = txt.resolve(agent, None);
        RobotsRules {
            sitemaps: txt.sitemaps().to_vec(),
            delay: rules.crawl_delay().map(cap_delay),
            kind: Kind::Rules(Box::new(txt), rules),
        }
    }

    /// Rules for a robots.txt that didn't return 2xx. Like Google: a 4xx (except 429)
    /// means there are no rules; 429 and 5xx mean the whole site is off limits for now. A
    /// redirect that never reached a file (a final 3xx) keeps us out too: more cautious than
    /// the AI access report, which reads it as missing as RFC 9309 allows.
    pub fn from_status(status: u16) -> RobotsRules {
        let kind = match status {
            429 => Kind::BlockAll,
            400..=499 => Kind::AllowAll,
            _ => Kind::BlockAll,
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
        let (txt, rules) = match &self.kind {
            Kind::AllowAll => return true,
            Kind::BlockAll => return false,
            Kind::Rules(txt, rules) => (txt, rules),
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
        txt.allowed_for(rules, path)
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

fn cap_delay(secs: f64) -> Duration {
    let max = Politeness::default().max_crawl_delay;
    Duration::from_secs_f64(secs.min(max.as_secs_f64()))
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
