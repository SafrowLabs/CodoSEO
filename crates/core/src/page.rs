//! One crawled URL and the fields pulled out of it.

use serde::{Deserialize, Serialize};
use url::Url;

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
}
