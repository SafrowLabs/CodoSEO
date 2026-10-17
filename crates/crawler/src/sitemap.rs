//! Sitemap parsing (streamed, gzip-aware) and discovery through sitemap indexes.
//!
//! Parsing streams through gunzip and a lossy UTF-8 decoder, so memory stays flat
//! however large a file is. Discovery is bounded three ways: [`MAX_SITEMAP_FILES`]
//! fetch attempts (failures count), a URL cap, and a deadline.

use std::collections::{HashSet, VecDeque};
use std::io::{BufReader, Read};
use std::time::{SystemTime, UNIX_EPOCH};

use codoseo_core::crawl::SitemapSummary;
use codoseo_core::sitemap::{LastmodStats, SitemapFile};
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

pub use codoseo_core::sitemap::SitemapKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SitemapDoc {
    pub kind: SitemapKind,
    pub locs: Vec<String>,
    /// The `<lastmod>` of its `<url>` entries, future ones counted against today (UTC).
    pub lastmod: LastmodStats,
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
    /// Every file fetched or tried, in fetch order, with the URLs each listed.
    pub files: Vec<SitemapFile>,
}

pub fn parse_sitemap(bytes: &[u8]) -> Result<SitemapDoc, SitemapError> {
    let mut locs = Vec::new();
    let mut lastmod = LastmodTally::new(today());
    let kind = parse_stream(bytes, |kind, loc, modified| {
        locs.push(loc.to_owned());
        if kind == SitemapKind::UrlSet {
            lastmod.add(modified);
        }
        true
    })?;
    Ok(SitemapDoc {
        kind,
        locs,
        lastmod: lastmod.finish(),
    })
}

/// [`LastmodStats`] as a file's entries come in.
struct LastmodTally {
    today: String,
    stats: LastmodStats,
    dates: HashSet<String>,
}

impl LastmodTally {
    fn new(today: String) -> Self {
        LastmodTally {
            today,
            stats: LastmodStats::default(),
            dates: HashSet::new(),
        }
    }

    fn add(&mut self, lastmod: Option<&str>) {
        let Some(date) = lastmod.and_then(date_of) else {
            return;
        };
        let s = &mut self.stats;
        s.with += 1;
        if date > self.today.as_str() {
            s.future += 1;
        }
        if s.min.as_deref().is_none_or(|m| date < m) {
            s.min = Some(date.to_owned());
        }
        if s.max.as_deref().is_none_or(|m| date > m) {
            s.max = Some(date.to_owned());
        }
        if !self.dates.contains(date) {
            self.dates.insert(date.to_owned());
        }
    }

    fn finish(mut self) -> LastmodStats {
        self.stats.distinct = u32::try_from(self.dates.len()).unwrap_or(u32::MAX);
        self.stats
    }
}

/// The `YYYY-MM-DD` a W3C datetime starts with; `None` when it doesn't start with one.
fn date_of(value: &str) -> Option<&str> {
    let date = value.get(..10)?;
    let ok = date.bytes().enumerate().all(|(i, b)| match i {
        4 | 7 => b == b'-',
        _ => b.is_ascii_digit(),
    });
    ok.then_some(date)
}

/// Today's UTC date as `YYYY-MM-DD`.
fn today() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    civil_date(i64::try_from(secs / 86_400).unwrap_or(0))
}

/// The `YYYY-MM-DD` of a day counted from 1970-01-01 (Howard Hinnant's civil-from-days).
fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Which child of an entry the parser is reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    None,
    Loc,
    Lastmod,
}

/// Streams the entries directly under the root (`<url>` or `<sitemap>`) to `on_entry`: the
/// `<loc>` that sits directly under it (so `<image:loc>` and friends are skipped) and its
/// `<lastmod>`, if any. `on_entry` returns false to stop.
fn parse_stream(
    bytes: &[u8],
    mut on_entry: impl FnMut(SitemapKind, &str, Option<&str>) -> bool,
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
    let mut field = Field::None;
    let mut current = String::new();
    // The entry being read: its `<loc>` and `<lastmod>` once each has closed.
    let mut loc: Option<String> = None;
    let mut lastmod: Option<String> = None;
    let mut stopped = false;

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
                    2 => {
                        in_entry = matches!(local.as_ref(), "url" | "sitemap");
                        loc = None;
                        lastmod = None;
                    }
                    3 if in_entry && e.name().prefix().is_none() => {
                        field = match local.as_ref() {
                            "loc" => Field::Loc,
                            "lastmod" => Field::Lastmod,
                            _ => Field::None,
                        };
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
            Ok(Event::Text(t)) if field != Field::None => current.push_str(&t.xml10_content()),
            Ok(Event::CData(c)) if field != Field::None => current.push_str(&c.xml10_content()),
            Ok(Event::GeneralRef(r)) if field != Field::None => {
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
                if depth == 3 && field != Field::None {
                    let value = current.trim();
                    if !value.is_empty() {
                        match field {
                            Field::Loc => loc = Some(value.to_owned()),
                            Field::Lastmod => lastmod = Some(value.to_owned()),
                            Field::None => {}
                        }
                    }
                    field = Field::None;
                } else if depth == 2 && in_entry {
                    in_entry = false;
                    if let (Some(l), Some(k)) = (loc.take(), kind)
                        && !on_entry(k, &l, lastmod.as_deref())
                    {
                        stopped = true;
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
    // Broken XML inside an entry still yields its `<loc>` when that had closed.
    if !stopped
        && in_entry
        && let (Some(l), Some(k)) = (loc, kind)
    {
        on_entry(k, &l, lastmod.as_deref());
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
///
/// Every file tried is kept as a [`SitemapFile`], with the URLs it listed; across all files at
/// most twice `max_urls` of those are kept, so a file repeating one URL can't grow memory.
pub async fn discover(
    fetcher: &Fetcher,
    limiter: Option<&Limiter>,
    seeds: &[Url],
    max_urls: u32,
    deadline: Instant,
) -> SitemapDiscovery {
    let max_urls = max_urls as usize;
    let max_members = max_urls.saturating_mul(2);
    let mut known: HashSet<String> = HashSet::new();
    // Each sitemap with its depth and the index into `sitemap_files` of the index listing it.
    let mut queue: VecDeque<(Url, u8, Option<u32>)> = VecDeque::new();
    for seed in seeds {
        if known.insert(seed.as_str().to_owned()) {
            queue.push_back((seed.clone(), 0, None));
        }
    }
    let mut seen: HashSet<u64> = HashSet::new();
    let mut urls: Vec<Url> = Vec::new();
    let mut files: Vec<Url> = Vec::new();
    let mut sitemap_files: Vec<SitemapFile> = Vec::new();
    let mut members = 0usize;
    let today = today();
    let mut attempts = 0usize;
    let mut failed = 0u32;
    let mut truncated = false;
    let mut dropped = false;
    let mut stopped = false;

    while let Some((sitemap_url, depth, parent)) = queue.pop_front() {
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
            Ok(res) => res,
        };
        let index = u32::try_from(sitemap_files.len()).unwrap_or(u32::MAX);
        sitemap_files.push(SitemapFile {
            url: sitemap_url.clone(),
            kind: None,
            parent,
            failed: true,
            listed: 0,
            lastmod: LastmodStats::default(),
            urls: Vec::new(),
        });
        let Some(body) = res
            .ok()
            .and_then(|res| res.body.filter(|_| (200..300).contains(&res.status)))
        else {
            failed += 1;
            continue;
        };

        let file = sitemap_files.last_mut().expect("pushed above");
        let mut lastmod = LastmodTally::new(today.clone());
        let parsed = parse_stream(&body, |kind, loc, modified| {
            file.listed = file.listed.saturating_add(1);
            match kind {
                SitemapKind::Index => {
                    // Past the depth or file cap the index is still read, for its count.
                    if depth >= MAX_DEPTH {
                        return true;
                    }
                    if attempts + queue.len() >= MAX_SITEMAP_FILES {
                        dropped = true;
                        return true;
                    }
                    if let Some(child) = normalize(&sitemap_url, loc)
                        && known.insert(child.as_str().to_owned())
                    {
                        queue.push_back((child, depth + 1, Some(index)));
                    }
                    true
                }
                SitemapKind::UrlSet => {
                    lastmod.add(modified);
                    let Some(url) = normalize(&sitemap_url, loc) else {
                        return true;
                    };
                    if members < max_members {
                        members += 1;
                        file.urls.push(url.as_str().to_owned());
                    }
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
            }
        });
        match parsed {
            Ok(kind) => {
                file.kind = Some(kind);
                file.failed = false;
                file.lastmod = lastmod.finish();
                files.push(sitemap_url);
            }
            Err(_) => {
                file.listed = 0;
                failed += 1;
            }
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
        files: sitemap_files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_since_the_epoch_are_civil_dates() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(11_016), "2000-02-29");
        assert_eq!(civil_date(20_000), "2024-10-04");
        assert_eq!(today().len(), 10);
    }
}
