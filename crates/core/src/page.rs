//! One crawled URL and the fields pulled out of it.

use serde::{Deserialize, Serialize};
use url::Url;
use xxhash_rust::xxh3::Xxh3;

use crate::check::IssueBits;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Indexability {
    Indexable,
    Noindex,
    Canonicalised,
    Redirected,
    ClientError,
    ServerError,
    BlockedByRobots,
}

/// Why a fetch produced no response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchFailure {
    Timeout,
    Connect,
    RedirectLoop,
    TooManyRedirects,
    InvalidRedirect,
    Blocked,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OgTags {
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonLdStatus {
    #[default]
    Absent,
    /// Number of JSON-LD blocks, all of which parse as JSON.
    Valid(u16),
    /// At least one block does not parse as JSON.
    Invalid,
    /// A block is too big to check (over 1 MB).
    TooLarge,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageFields {
    pub title: Option<String>,
    pub title_count: u8,
    pub meta_description: Option<String>,
    pub meta_robots: Option<String>,
    /// From the `X-Robots-Tag` header; filled by the crawler, not the extractor.
    pub x_robots_tag: Option<String>,
    pub canonical: Option<Url>,
    pub hreflang: Vec<(String, Url)>,
    pub h1: Vec<String>,
    pub h2: Vec<String>,
    pub word_count: u32,
    pub content_hash: u64,
    pub images_missing_alt: u32,
    pub og: OgTags,
    pub jsonld: JsonLdStatus,
    pub mixed_content: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRecord {
    pub url: Url,
    pub url_hash: u64,
    pub status: u16,
    /// Every redirect hop in order: the status returned and the URL that returned it.
    pub redirect_chain: Vec<(u16, Url)>,
    pub response_ms: u32,
    pub size_bytes: u64,
    pub content_type: Option<String>,
    /// Clicks from the homepage; `None` for pages only found in sitemaps.
    pub depth: Option<u16>,
    pub in_sitemap: bool,
    pub indexability: Indexability,
    pub fields: PageFields,
    pub inlinks: u32,
    pub outlinks_internal: u32,
    pub outlinks_external: u32,
    pub issues: IssueBits,
    /// Hash of the fields change detection compares.
    pub key_hash: u64,
    /// Where a redirecting URL ends up.
    #[serde(default)]
    pub redirect_target: Option<Url>,
    /// Internal links on the page marked `rel=nofollow`.
    #[serde(default)]
    pub outlinks_nofollow: u32,
    /// Set when there was no response (`status` is 0).
    #[serde(default)]
    pub error: Option<FetchFailure>,
}

impl PageRecord {
    /// Hash of the fields change detection compares: status, title, description, first H1,
    /// canonical, both robots directives, indexability, redirect target and sitemap
    /// membership. Timings and counts are left out so they don't read as changes.
    pub fn compute_key_hash(&self) -> u64 {
        let mut h = Xxh3::new();
        h.update(&self.status.to_le_bytes());
        hash_str(&mut h, self.fields.title.as_deref());
        hash_str(&mut h, self.fields.meta_description.as_deref());
        hash_str(&mut h, self.fields.h1.first().map(String::as_str));
        hash_str(&mut h, self.fields.canonical.as_ref().map(Url::as_str));
        hash_str(&mut h, self.fields.meta_robots.as_deref());
        hash_str(&mut h, self.fields.x_robots_tag.as_deref());
        h.update(&[indexability_code(self.indexability)]);
        hash_str(&mut h, self.redirect_target.as_ref().map(Url::as_str));
        h.update(&[u8::from(self.in_sitemap)]);
        h.digest()
    }

    /// A successful response that can be read as an HTML page.
    pub fn is_html_ok(&self) -> bool {
        if !(200..300).contains(&self.status) || self.error.is_some() {
            return false;
        }
        match &self.content_type {
            None => true,
            Some(ct) => {
                let ct = ct.to_ascii_lowercase();
                ct.contains("text/html") || ct.contains("application/xhtml+xml")
            }
        }
    }
}

/// Length-prefixed, so neighbouring fields can't blur into each other.
fn hash_str(h: &mut Xxh3, value: Option<&str>) {
    match value {
        None => h.update(&[0]),
        Some(s) => {
            h.update(&[1]);
            h.update(&(s.len() as u64).to_le_bytes());
            h.update(s.as_bytes())
        }
    };
}

/// Fixed codes, so the hash doesn't change if variants are reordered.
fn indexability_code(i: Indexability) -> u8 {
    match i {
        Indexability::Indexable => 0,
        Indexability::Noindex => 1,
        Indexability::Canonicalised => 2,
        Indexability::Redirected => 3,
        Indexability::ClientError => 4,
        Indexability::ServerError => 5,
        Indexability::BlockedByRobots => 6,
    }
}

/// Bots whose scoped robots directives (`googlebot: noindex`) apply to us.
const OUR_AGENTS: [&str; 2] = ["codoseobot", "googlebot"];

/// Directives that carry a value after a colon; these are not agent prefixes.
const VALUE_DIRECTIVES: [&str; 4] = [
    "unavailable_after",
    "max-snippet",
    "max-image-preview",
    "max-video-preview",
];

/// True when a robots directive list (meta robots or `X-Robots-Tag`) contains one of `words`
/// for us. Follows Google's rules: an `agent:` prefix scopes every following comma-separated
/// directive until the next prefix, directives before any prefix apply to all bots, and only
/// `codoseobot` and `googlebot` scopes apply to us.
fn has_directive(value: Option<&str>, words: [&str; 2]) -> bool {
    let Some(value) = value else { return false };
    let mut applies = true;
    for token in value.to_ascii_lowercase().split(',') {
        let directive = match token.split_once(':') {
            Some((name, rest)) if !VALUE_DIRECTIVES.contains(&name.trim()) => {
                applies = OUR_AGENTS.contains(&name.trim());
                rest
            }
            _ => token,
        };
        if applies && words.contains(&directive.trim()) {
            return true;
        }
    }
    false
}

/// `noindex` or `none` in the meta robots tag or the `X-Robots-Tag` header.
pub fn is_noindex(meta_robots: Option<&str>, x_robots_tag: Option<&str>) -> bool {
    let words = ["noindex", "none"];
    has_directive(meta_robots, words) || has_directive(x_robots_tag, words)
}

/// `nofollow` or `none` in the meta robots tag or the `X-Robots-Tag` header.
pub fn is_nofollow(meta_robots: Option<&str>, x_robots_tag: Option<&str>) -> bool {
    let words = ["nofollow", "none"];
    has_directive(meta_robots, words) || has_directive(x_robots_tag, words)
}

/// Whether search engines would index this URL. Status 0 (no response) counts as a server error.
pub fn indexability(
    url: &Url,
    status: u16,
    fields: &PageFields,
    robots_blocked: bool,
) -> Indexability {
    if robots_blocked {
        return Indexability::BlockedByRobots;
    }
    match status {
        0..=199 | 500.. => Indexability::ServerError,
        300..=399 => Indexability::Redirected,
        400..=499 => Indexability::ClientError,
        200..=299 => {
            if is_noindex(
                fields.meta_robots.as_deref(),
                fields.x_robots_tag.as_deref(),
            ) {
                Indexability::Noindex
            } else if fields.canonical.as_ref().is_some_and(|c| c != url) {
                Indexability::Canonicalised
            } else {
                Indexability::Indexable
            }
        }
    }
}
