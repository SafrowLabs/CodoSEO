//! Sitemap parsing (streamed, gzip-aware) and discovery through sitemap indexes.

use std::borrow::Cow;
use std::collections::{HashSet, VecDeque};
use std::io::Read;

use codoseo_core::crawl::SitemapSummary;
use codoseo_core::url::{normalize, url_hash};
use flate2::read::GzDecoder;
use quick_xml::Reader;
use quick_xml::events::Event;
use url::Url;
use xxhash_rust::xxh3::xxh3_64;

use crate::fetch::Fetcher;

/// The sitemap protocol's size limit, also applied after decompression.
const MAX_SITEMAP_BYTES: usize = 50 * 1024 * 1024;
/// Seeds are depth 0; indexes at depth 2 are read but their children are not.
const MAX_DEPTH: u8 = 2;
const MAX_FILES: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SitemapKind {
    UrlSet,
    Index,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SitemapDoc {
    pub kind: SitemapKind,
    pub locs: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SitemapError {
    #[error("not a sitemap")]
    NotASitemap,
    #[error("invalid XML: {0}")]
    Xml(String),
    #[error("could not decompress: {0}")]
    Gzip(String),
}

pub struct SitemapDiscovery {
    pub urls: Vec<Url>,
    pub summary: SitemapSummary,
}

pub fn parse_sitemap(bytes: &[u8]) -> Result<SitemapDoc, SitemapError> {
    parse_limited(bytes, usize::MAX).map(|(doc, _)| doc)
}

/// Parses at most `max_locs` locations; the flag says whether more were left.
fn parse_limited(bytes: &[u8], max_locs: usize) -> Result<(SitemapDoc, bool), SitemapError> {
    let data = maybe_gunzip(bytes)?;
    let mut reader = Reader::from_reader(data.as_ref());
    let mut buf = Vec::new();
    let mut kind = None;
    let mut in_loc = false;
    let mut current = String::new();
    let mut locs = Vec::new();
    let mut truncated = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if kind.is_none() => {
                kind = Some(match e.local_name().as_ref() {
                    "urlset" => SitemapKind::UrlSet,
                    "sitemapindex" => SitemapKind::Index,
                    _ => return Err(SitemapError::NotASitemap),
                });
            }
            Ok(Event::Start(e)) if e.local_name().as_ref() == "loc" => {
                in_loc = true;
                current.clear();
            }
            Ok(Event::Text(t)) if in_loc => current.push_str(&t.xml10_content()),
            Ok(Event::CData(c)) if in_loc => current.push_str(&c.xml10_content()),
            Ok(Event::GeneralRef(r)) if in_loc => {
                let ch = match r.resolve_char_ref() {
                    Ok(Some(ch)) => Some(ch),
                    _ => match r.as_ref() {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        _ => None,
                    },
                };
                current.extend(ch);
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == "loc" => {
                in_loc = false;
                let loc = current.trim();
                if !loc.is_empty() {
                    if locs.len() >= max_locs {
                        truncated = true;
                        break;
                    }
                    locs.push(loc.to_owned());
                }
            }
            Ok(Event::Eof) => break,
            // Broken XML after the root element still yields what we read so far.
            Err(e) if kind.is_none() => return Err(SitemapError::Xml(e.to_string())),
            Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }

    let kind = kind.ok_or(SitemapError::NotASitemap)?;
    Ok((SitemapDoc { kind, locs }, truncated))
}

fn maybe_gunzip(bytes: &[u8]) -> Result<Cow<'_, [u8]>, SitemapError> {
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return Ok(Cow::Borrowed(bytes));
    }
    let mut out = Vec::new();
    GzDecoder::new(bytes)
        .take(MAX_SITEMAP_BYTES as u64)
        .read_to_end(&mut out)
        .map_err(|e| SitemapError::Gzip(e.to_string()))?;
    Ok(Cow::Owned(out))
}

/// Fetches the seed sitemaps, follows indexes up to depth 2, and returns up to
/// `max_urls` unique page URLs. Missing or broken sitemaps are skipped.
pub async fn discover(fetcher: &Fetcher, seeds: &[Url], max_urls: u32) -> SitemapDiscovery {
    let max_urls = max_urls as usize;
    let mut queue: VecDeque<(Url, u8)> = seeds.iter().map(|u| (u.clone(), 0)).collect();
    let mut visited: HashSet<String> = HashSet::new();
    let mut seen: HashSet<u64> = HashSet::new();
    let mut urls: Vec<Url> = Vec::new();
    let mut files: Vec<Url> = Vec::new();
    let mut truncated = false;

    while let Some((sitemap_url, depth)) = queue.pop_front() {
        if truncated || files.len() >= MAX_FILES {
            break;
        }
        if !visited.insert(sitemap_url.as_str().to_owned()) {
            continue;
        }
        let Ok(res) = fetcher.fetch_raw(&sitemap_url, MAX_SITEMAP_BYTES).await else {
            continue;
        };
        let Some(body) = res.body.filter(|_| (200..300).contains(&res.status)) else {
            continue;
        };
        let Ok((doc, cut)) = parse_limited(&body, max_urls) else {
            continue;
        };
        files.push(sitemap_url.clone());
        let resolved = doc
            .locs
            .iter()
            .filter_map(|loc| normalize(&sitemap_url, loc));
        match doc.kind {
            SitemapKind::Index => {
                if depth < MAX_DEPTH {
                    queue.extend(resolved.map(|u| (u, depth + 1)));
                }
            }
            SitemapKind::UrlSet => {
                for url in resolved {
                    if seen.insert(url_hash(&url)) {
                        if urls.len() >= max_urls {
                            truncated = true;
                            break;
                        }
                        urls.push(url);
                    }
                }
                truncated |= cut;
            }
        }
    }

    let mut hashes: Vec<u64> = urls.iter().map(url_hash).collect();
    hashes.sort_unstable();
    let hash_bytes: Vec<u8> = hashes.iter().flat_map(|h| h.to_le_bytes()).collect();
    SitemapDiscovery {
        summary: SitemapSummary {
            files,
            url_count: urls.len() as u32,
            hash: xxh3_64(&hash_bytes),
            truncated,
        },
        urls,
    }
}
