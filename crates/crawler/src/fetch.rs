//! HTTP fetching with hand-followed redirects, body caps and timeouts.

use std::collections::HashSet;
use std::error::Error as _;
use std::time::{Duration, Instant};

use bytes::Bytes;
use codoseo_core::crawl::{AddressPolicy, CrawlLimits, USER_AGENT};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, LOCATION};
use reqwest::redirect::Policy;
use url::Url;

use crate::guard::{GuardError, GuardedResolver, SystemLookup, check_url};

#[derive(Debug, Clone)]
pub struct FetcherConfig {
    pub user_agent: String,
    pub address_policy: AddressPolicy,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    pub max_redirects: u8,
    pub max_body_bytes: usize,
}

impl FetcherConfig {
    pub fn new(address_policy: AddressPolicy) -> Self {
        let limits = CrawlLimits::default();
        FetcherConfig {
            user_agent: USER_AGENT.to_owned(),
            address_policy,
            request_timeout: limits.request_timeout,
            connect_timeout: Duration::from_secs(10),
            max_redirects: limits.max_redirects,
            max_body_bytes: limits.max_page_bytes,
        }
    }
}

/// One redirect hop: the status returned and the URL that returned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    pub status: u16,
    pub url: Url,
}

#[derive(Debug, Clone)]
pub struct FetchResult {
    pub final_url: Url,
    pub status: u16,
    pub chain: Vec<Hop>,
    pub headers: HeaderMap,
    pub content_type: Option<String>,
    pub x_robots_tag: Option<String>,
    /// Time from sending the final request to receiving its headers.
    pub response_ms: u32,
    pub size_bytes: u64,
    /// `None` when the body wasn't read (not HTML for `fetch`).
    pub body: Option<Bytes>,
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("blocked: {0}")]
    Blocked(String),
    #[error("request timed out")]
    Timeout,
    #[error("connection failed: {0}")]
    Connect(String),
    #[error("more than the allowed number of redirects")]
    TooManyRedirects { chain: Vec<Hop> },
    #[error("redirect loop")]
    RedirectLoop { chain: Vec<Hop> },
    #[error("invalid redirect target: {location}")]
    InvalidRedirect { location: String },
    #[error("request failed: {0}")]
    Http(String),
    #[error("could not build HTTP client: {0}")]
    Client(String),
}

impl From<GuardError> for FetchError {
    fn from(e: GuardError) -> Self {
        FetchError::Blocked(e.to_string())
    }
}

pub struct Fetcher {
    client: reqwest::Client,
    cfg: FetcherConfig,
}

enum BodyMode {
    HtmlOnly,
    Any(usize),
}

impl Fetcher {
    pub fn new(cfg: FetcherConfig) -> Result<Self, FetchError> {
        let mut builder = reqwest::Client::builder()
            .redirect(Policy::none())
            .user_agent(cfg.user_agent.clone())
            .timeout(cfg.request_timeout)
            .connect_timeout(cfg.connect_timeout)
            .no_proxy();
        if cfg.address_policy == AddressPolicy::Public {
            builder = builder.dns_resolver(GuardedResolver::new(SystemLookup));
        }
        let client = builder
            .build()
            .map_err(|e| FetchError::Client(e.to_string()))?;
        Ok(Fetcher { client, cfg })
    }

    /// Fetches a page. Only HTML bodies are read, up to the configured cap.
    pub async fn fetch(&self, url: &Url) -> Result<FetchResult, FetchError> {
        self.run(url, BodyMode::HtmlOnly).await
    }

    /// Fetches any resource (robots.txt, sitemaps) and reads up to `max_bytes` of its body.
    pub async fn fetch_raw(&self, url: &Url, max_bytes: usize) -> Result<FetchResult, FetchError> {
        self.run(url, BodyMode::Any(max_bytes)).await
    }

    async fn run(&self, start: &Url, mode: BodyMode) -> Result<FetchResult, FetchError> {
        let mut current = start.clone();
        let mut chain: Vec<Hop> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            check_url(&current, self.cfg.address_policy)?;
            seen.insert(current.as_str().to_owned());

            let sent = Instant::now();
            let resp = self
                .client
                .get(current.clone())
                .send()
                .await
                .map_err(map_err)?;
            let response_ms = sent.elapsed().as_millis().min(u32::MAX as u128) as u32;
            let status = resp.status().as_u16();

            if resp.status().is_redirection()
                && let Some(location) = resp.headers().get(LOCATION)
            {
                let location = String::from_utf8_lossy(location.as_bytes()).into_owned();
                let next = current
                    .join(&location)
                    .ok()
                    .filter(|u| matches!(u.scheme(), "http" | "https"))
                    .ok_or(FetchError::InvalidRedirect { location })?;
                chain.push(Hop {
                    status,
                    url: current,
                });
                if seen.contains(next.as_str()) {
                    return Err(FetchError::RedirectLoop { chain });
                }
                if chain.len() > usize::from(self.cfg.max_redirects) {
                    return Err(FetchError::TooManyRedirects { chain });
                }
                current = next;
                continue;
            }

            return self.finish(current, chain, resp, response_ms, mode).await;
        }
    }

    async fn finish(
        &self,
        final_url: Url,
        chain: Vec<Hop>,
        mut resp: reqwest::Response,
        response_ms: u32,
        mode: BodyMode,
    ) -> Result<FetchResult, FetchError> {
        let headers = resp.headers().clone();
        let content_type = header_str(&headers, CONTENT_TYPE.as_str());
        let x_robots_tag = joined(&headers, "x-robots-tag");
        let declared_len = headers
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());

        let cap = match mode {
            BodyMode::Any(max) => Some(max),
            BodyMode::HtmlOnly if is_html(content_type.as_deref()) => Some(self.cfg.max_body_bytes),
            BodyMode::HtmlOnly => None,
        };

        let (body, truncated, read) = match cap {
            None => (None, false, 0),
            Some(cap) => {
                // Size the buffer from Content-Length and never let it grow past
                // the cap, so a capped body holds `cap` bytes, not the next power of two.
                let initial = declared_len.map_or(64 * 1024, |len| len as usize).min(cap);
                let mut buf: Vec<u8> = Vec::with_capacity(initial);
                let mut truncated = false;
                while let Some(chunk) = resp.chunk().await.map_err(map_err)? {
                    let take = chunk.len().min(cap - buf.len());
                    let needed = buf.len() + take;
                    if needed > buf.capacity() {
                        let target = (buf.capacity() * 2).max(needed).min(cap);
                        buf.reserve_exact(target - buf.len());
                    }
                    buf.extend_from_slice(&chunk[..take]);
                    if take < chunk.len() {
                        truncated = true;
                        break;
                    }
                }
                let read = buf.len() as u64;
                (Some(Bytes::from(buf)), truncated, read)
            }
        };

        Ok(FetchResult {
            final_url,
            status: resp.status().as_u16(),
            chain,
            headers,
            content_type,
            x_robots_tag,
            response_ms,
            size_bytes: declared_len.unwrap_or(read),
            body,
            truncated,
        })
    }
}

fn is_html(content_type: Option<&str>) -> bool {
    match content_type {
        None => true,
        Some(ct) => {
            let mime = ct
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            mime == "text/html" || mime == "application/xhtml+xml"
        }
    }
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn joined(headers: &HeaderMap, name: &str) -> Option<String> {
    let values: Vec<&str> = headers
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

fn map_err(e: reqwest::Error) -> FetchError {
    let mut source = e.source();
    while let Some(s) = source {
        if let Some(guard) = s.downcast_ref::<GuardError>() {
            return FetchError::Blocked(guard.to_string());
        }
        source = s.source();
    }
    if e.is_timeout() {
        FetchError::Timeout
    } else if e.is_connect() {
        FetchError::Connect(error_chain(&e))
    } else {
        FetchError::Http(error_chain(&e))
    }
}

fn error_chain(e: &reqwest::Error) -> String {
    let mut msg = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        source = s.source();
    }
    msg
}
