//! robots.txt rules for CodoSEObot, following Google's handling of status codes and size.

use std::time::Duration;

use codoseo_core::crawl::{Politeness, RobotsFile};
use texting_robots::Robot;
use url::Url;
use xxhash_rust::xxh3::xxh3_64;

use crate::fetch::{FetchError, Fetcher};

/// The product token sites use to address us in robots.txt.
pub const ROBOTS_AGENT: &str = "CodoSEObot";

/// Google ignores everything after the first 500 KiB.
const MAX_ROBOTS_BYTES: usize = 500 * 1024;

enum Kind {
    AllowAll,
    BlockAll,
    Rules(Box<Robot>),
}

pub struct RobotsRules {
    kind: Kind,
    delay: Option<Duration>,
    sitemaps: Vec<String>,
}

impl RobotsRules {
    /// Parses a robots.txt body. Unparseable files allow everything.
    pub fn parse(body: &[u8], agent: &str) -> RobotsRules {
        let body = &body[..body.len().min(MAX_ROBOTS_BYTES)];
        let max_delay = Politeness::default().max_crawl_delay;
        match Robot::new(agent, body) {
            Ok(robot) => RobotsRules {
                delay: robot
                    .delay
                    .filter(|d| d.is_finite() && *d > 0.0)
                    .map(|d| Duration::from_secs_f32(d).min(max_delay)),
                sitemaps: robot.sitemaps.clone(),
                kind: Kind::Rules(Box::new(robot)),
            },
            Err(_) => RobotsRules::allow_all(),
        }
    }

    /// Rules for a robots.txt that didn't return 2xx: a 4xx means there are no
    /// rules, anything else (5xx) means the whole site is off limits for now.
    pub fn from_status(status: u16) -> RobotsRules {
        if (400..500).contains(&status) {
            RobotsRules::allow_all()
        } else {
            RobotsRules {
                kind: Kind::BlockAll,
                delay: None,
                sitemaps: Vec::new(),
            }
        }
    }

    pub fn from_response(status: u16, body: &[u8], agent: &str) -> RobotsRules {
        if (200..300).contains(&status) {
            RobotsRules::parse(body, agent)
        } else {
            RobotsRules::from_status(status)
        }
    }

    fn allow_all() -> RobotsRules {
        RobotsRules {
            kind: Kind::AllowAll,
            delay: None,
            sitemaps: Vec::new(),
        }
    }

    /// Takes an absolute URL or a path.
    pub fn allowed(&self, url_or_path: &str) -> bool {
        match &self.kind {
            Kind::AllowAll => true,
            Kind::BlockAll => false,
            Kind::Rules(robot) => robot.allowed(url_or_path),
        }
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
