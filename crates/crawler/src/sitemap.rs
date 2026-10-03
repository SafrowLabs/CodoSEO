//! Sitemap parsing (streamed, gzip-aware) and discovery through sitemap indexes.
//!
//! Parsing streams through gunzip and a lossy UTF-8 decoder, so memory stays flat
//! however large a file is. Discovery is bounded three ways: [`MAX_SITEMAP_FILES`]
//! fetch attempts (failures count), a URL cap, and a deadline.

use std::collections::{HashSet, VecDeque};
use std::io::{BufReader, Read};

use codoseo_core::crawl::SitemapSummary;
use codoseo_core::url::{normalize, url_hash};
use encoding_rs::{Decoder, UTF_8};
use flate2::read::GzDecoder;
use quick_xml::Reader;
use quick_xml::events::Event;
use tokio::time::{Instant, timeout_at};
use url::Url;
use xxhash_rust::xxh3::xxh3_64;

use crate::fetch::Fetcher;
use crate::politeness::Limiter;

/// The sitemap protocol's size limit, also applied after decompression.
const MAX_SITEMAP_BYTES: usize = 50 * 1024 * 1024;
/// Seeds are depth 0; indexes at depth 2 are read but their children are not.
const MAX_DEPTH: u8 = 2;
/// Fetch attempts per discovery, successful or not.
pub const MAX_SITEMAP_FILES: usize = 100;

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
}

pub struct SitemapDiscovery {
    pub urls: Vec<Url>,
    pub summary: SitemapSummary,
}

pub fn parse_sitemap(bytes: &[u8]) -> Result<SitemapDoc, SitemapError> {
    let mut locs = Vec::new();
    let kind = parse_stream(bytes, |_, loc| {
        locs.push(loc.to_owned());
        true
    })?;
    Ok(SitemapDoc { kind, locs })
}

/// Streams the `<loc>` values that sit directly under `<url>` or `<sitemap>` (so
/// `<image:loc>` and friends are skipped) to `on_loc`, which returns false to stop.
fn parse_stream(
    bytes: &[u8],
    mut on_loc: impl FnMut(SitemapKind, &str) -> bool,
) -> Result<SitemapKind, SitemapError> {
    let source: Box<dyn Read + '_> = if bytes.starts_with(&[0x1f, 0x8b]) {
        Box::new(GzDecoder::new(bytes).take(MAX_SITEMAP_BYTES as u64))
    } else {
        Box::new(bytes)
    };
    let mut reader = Reader::from_reader(BufReader::new(LossyUtf8::new(source)));
    let mut buf = Vec::new();
    let mut kind = None;
    let mut depth = 0usize;
    let mut in_entry = false;
    let mut in_loc = false;
    let mut current = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                depth += 1;
                let local = e.local_name();
                match depth {
                    1 => {
                        kind = Some(match local.as_ref() {
                            "urlset" => SitemapKind::UrlSet,
                            "sitemapindex" => SitemapKind::Index,
                            _ => return Err(SitemapError::NotASitemap),
                        })
                    }
                    2 => in_entry = matches!(local.as_ref(), "url" | "sitemap"),
                    3 if in_entry && local.as_ref() == "loc" && e.name().prefix().is_none() => {
                        in_loc = true;
                        current.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) if depth == 0 => {
                return match e.local_name().as_ref() {
                    "urlset" => Ok(SitemapKind::UrlSet),
                    "sitemapindex" => Ok(SitemapKind::Index),
                    _ => Err(SitemapError::NotASitemap),
                };
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
            Ok(Event::End(_)) => {
                if in_loc {
                    in_loc = false;
                    let loc = current.trim();
                    if !loc.is_empty()
                        && let Some(k) = kind
                        && !on_loc(k, loc)
                    {
                        break;
                    }
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Eof) => break,
            // Broken XML after the root element still yields what we read so far.
            Err(e) if kind.is_none() => return Err(SitemapError::Xml(e.to_string())),
            Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }
    kind.ok_or(SitemapError::NotASitemap)
}

/// Decodes bytes as UTF-8, replacing invalid sequences, so one bad byte in a
/// sitemap doesn't end parsing. Also drops a UTF-8 BOM.
struct LossyUtf8<R: Read> {
    inner: R,
    decoder: Decoder,
    input: Vec<u8>,
    output: Vec<u8>,
    pos: usize,
    done: bool,
}

impl<R: Read> LossyUtf8<R> {
    fn new(inner: R) -> Self {
        LossyUtf8 {
            inner,
            decoder: UTF_8.new_decoder_with_bom_removal(),
            input: vec![0; 16 * 1024],
            output: Vec::new(),
            pos: 0,
            done: false,
        }
    }
}

impl<R: Read> Read for LossyUtf8<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.pos == self.output.len() {
            if self.done {
                return Ok(0);
            }
            let n = self.inner.read(&mut self.input)?;
            let last = n == 0;
            let capacity = self.decoder.max_utf8_buffer_length(n).unwrap_or(n * 3 + 4);
            self.output.resize(capacity, 0);
            let (_, _, written, _) =
                self.decoder
                    .decode_to_utf8(&self.input[..n], &mut self.output, last);
            self.output.truncate(written);
            self.pos = 0;
            self.done = last;
        }
        let n = out.len().min(self.output.len() - self.pos);
        out[..n].copy_from_slice(&self.output[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Fetches the seed sitemaps, follows indexes up to depth 2, and returns up to
/// `max_urls` unique page URLs. Stops at `deadline` with what it has. With a `limiter`,
/// every sitemap fetch waits for a permit first.
pub async fn discover(
    fetcher: &Fetcher,
    limiter: Option<&Limiter>,
    seeds: &[Url],
    max_urls: u32,
    deadline: Instant,
) -> SitemapDiscovery {
    let max_urls = max_urls as usize;
    let mut known: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(Url, u8)> = VecDeque::new();
    for seed in seeds {
        if known.insert(seed.as_str().to_owned()) {
            queue.push_back((seed.clone(), 0));
        }
    }
    let mut seen: HashSet<u64> = HashSet::new();
    let mut urls: Vec<Url> = Vec::new();
    let mut files: Vec<Url> = Vec::new();
    let mut attempts = 0usize;
    let mut failed = 0u32;
    let mut truncated = false;
    let mut dropped = false;
    let mut stopped = false;

    while let Some((sitemap_url, depth)) = queue.pop_front() {
        if truncated {
            break;
        }
        if Instant::now() >= deadline {
            stopped = true;
            break;
        }
        attempts += 1;
        let fetch = async {
            let _permit = match limiter {
                Some(l) => Some(l.acquire().await),
                None => None,
            };
            fetcher.fetch_raw(&sitemap_url, MAX_SITEMAP_BYTES).await
        };
        let res = match timeout_at(deadline, fetch).await {
            Err(_) => {
                stopped = true;
                break;
            }
            Ok(Err(_)) => {
                failed += 1;
                continue;
            }
            Ok(Ok(res)) => res,
        };
        let Some(body) = res.body.filter(|_| (200..300).contains(&res.status)) else {
            failed += 1;
            continue;
        };

        let parsed = parse_stream(&body, |kind, loc| match kind {
            SitemapKind::Index => {
                if depth >= MAX_DEPTH {
                    return false;
                }
                if attempts + queue.len() >= MAX_SITEMAP_FILES {
                    dropped = true;
                    return false;
                }
                if let Some(child) = normalize(&sitemap_url, loc)
                    && known.insert(child.as_str().to_owned())
                {
                    queue.push_back((child, depth + 1));
                }
                true
            }
            SitemapKind::UrlSet => {
                let Some(url) = normalize(&sitemap_url, loc) else {
                    return true;
                };
                if !seen.insert(url_hash(&url)) {
                    return true;
                }
                if urls.len() >= max_urls {
                    truncated = true;
                    return false;
                }
                urls.push(url);
                true
            }
        });
        match parsed {
            Ok(_) => files.push(sitemap_url),
            Err(_) => failed += 1,
        }
    }

    let complete = !stopped && !dropped && (queue.is_empty() || truncated);
    let mut hashes: Vec<u64> = urls.iter().map(url_hash).collect();
    hashes.sort_unstable();
    let hash_bytes: Vec<u8> = hashes.iter().flat_map(|h| h.to_le_bytes()).collect();
    SitemapDiscovery {
        summary: SitemapSummary {
            files,
            url_count: urls.len() as u32,
            hash: xxh3_64(&hash_bytes),
            truncated,
            failed_files: failed,
            complete,
        },
        urls,
    }
}
