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
    /// What the page says to AI answer engines; empty for pages without any such markup.
    #[serde(default)]
    pub ai: AiMeta,
}

/// Page-level markup that speaks to AI and answer engines, read by the extractor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiMeta {
    /// `<meta name="X" content="…">` for robots-style names other than `robots`
    /// (`googlebot`, `bingbot`, a registry token…): name lower-cased, content cleaned like
    /// `meta_robots`. At most 16 entries.
    #[serde(default)]
    pub bot_meta: Vec<(String, String)>,
    /// Words of visible text inside elements carrying `data-nosnippet`, nested ones once.
    #[serde(default)]
    pub nosnippet_words: u32,
    /// `<meta name="tdm-reservation">`.
    #[serde(default)]
    pub tdm_reservation: Option<String>,
    /// `<meta name="tdm-policy">`.
    #[serde(default)]
    pub tdm_policy: Option<String>,
}

impl AiMeta {
    pub fn is_empty(&self) -> bool {
        *self == AiMeta::default()
    }
}

impl PageFields {
    /// [`is_noindex`] plus `<meta name="googlebot">` and `<meta name="codoseobot">`, which
    /// apply to us like the `googlebot:` prefix does.
    pub fn is_noindex(&self) -> bool {
        is_noindex(self.meta_robots.as_deref(), self.x_robots_tag.as_deref())
            || self.our_bot_meta_has(["noindex", "none"])
    }

    /// [`is_nofollow`] plus the bot-named metas, as for [`PageFields::is_noindex`].
    pub fn is_nofollow(&self) -> bool {
        is_nofollow(self.meta_robots.as_deref(), self.x_robots_tag.as_deref())
            || self.our_bot_meta_has(["nofollow", "none"])
    }

    fn our_bot_meta_has(&self, words: [&str; 2]) -> bool {
        self.ai
            .bot_meta
            .iter()
            .filter(|(name, _)| OUR_AGENTS.contains(&name.as_str()))
            .any(|(_, content)| has_directive(Some(content), words))
    }
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
        // Only when present, so pages without bot-named metas keep the hash they had before.
        if !self.fields.ai.bot_meta.is_empty() {
            h.update(&(self.fields.ai.bot_meta.len() as u64).to_le_bytes());
            for (name, content) in &self.fields.ai.bot_meta {
                hash_str(&mut h, Some(name));
                hash_str(&mut h, Some(content));
            }
        }
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
    directives_for(value, &OUR_AGENTS)
        .iter()
        .any(|d| words.contains(&d.as_str()))
}

/// The directives of a robots list (meta robots or `X-Robots-Tag`) that apply to a crawler
/// answering to any of `scopes` (lower-case agent names, e.g. `robots`, `googlebot`).
/// Same rules as Google's: an `agent:` prefix scopes every following comma-separated directive
/// until the next prefix, and directives before any prefix apply to all. Directives come back
/// lower-cased and trimmed; value directives (`max-snippet:50`) are kept whole with the
/// whitespace after the colon removed.
pub fn directives_for(value: Option<&str>, scopes: &[&str]) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let mut applies = true;
    let mut out = Vec::new();
    for token in value.to_ascii_lowercase().split(',') {
        let directive = match token.split_once(':') {
            // A prefix is a bare agent token; anything else (the date in `unavailable_after:
            // Fri, 25-Aug-2010 15:00:00 PST` after its comma) is not one and must not rescope.
            Some((name, rest))
                if !VALUE_DIRECTIVES.contains(&name.trim())
                    && !name.trim().is_empty()
                    && name
                        .trim()
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') =>
            {
                applies = scopes.contains(&name.trim());
                rest
            }
            _ => token,
        };
        if applies {
            let directive = directive.trim();
            if directive.is_empty() {
                continue;
            }
            if directive.contains(':') {
                out.push(directive.split_whitespace().collect());
            } else {
                out.push(directive.to_owned());
            }
        }
    }
    out
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
            if fields.is_noindex() {
                Indexability::Noindex
            } else if fields.canonical.as_ref().is_some_and(|c| c != url) {
                Indexability::Canonicalised
            } else {
                Indexability::Indexable
            }
        }
    }
}
