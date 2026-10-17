//! Each finished crawl's whole internal link graph, kept in `crawl_graphs` as one gzipped binary
//! body so a 50k-page crawl with a million links stays one small row.
//!
//! The body, little-endian throughout, before gzip:
//!
//! - `CSG` and a version byte (1);
//! - page, anchor and edge counts, `u32` each;
//! - per page: `url_hash` `u64`, crawl-tree parent `u32` (`u32::MAX` for none), link score `u8`;
//! - per anchor: byte length `u32` and the UTF-8 text, at most [`MAX_ANCHOR_CHARS`] chars, each
//!   text once;
//! - per edge: from `u32`, to `u32`, anchor `u32`, nofollow `u8`. One edge per (from, to): the
//!   first the crawl found.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};

use codoseo_core::graph;
use codoseo_core::output::{CrawlOutput, Edge};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

const MAGIC: &[u8; 3] = b"CSG";
const VERSION: u8 = 1;
/// Anchor text is cut to this many chars; the views only show the start.
pub const MAX_ANCHOR_CHARS: usize = 200;
/// The parent of a page with none.
const NO_PARENT: u32 = u32::MAX;

/// One crawled page in a stored graph, at its index in `CrawlOutput::pages`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredPage {
    pub url_hash: u64,
    /// The page the crawler found it from (see [`codoseo_core::graph::GraphFacts::parent`]).
    pub parent: Option<u32>,
    /// 1..=100, see [`codoseo_core::graph::GraphFacts::link_score`].
    pub link_score: u8,
}

/// A crawl's link graph as read back from `crawl_graphs`, with both directions of adjacency
/// built once on decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredGraph {
    pub pages: Vec<StoredPage>,
    /// One per (from, to); `anchor` indexes `anchors`.
    pub edges: Vec<Edge>,
    pub anchors: Vec<String>,
    /// `out_edges[out_offsets[i]..out_offsets[i + 1]]` are the indexes into `edges` of page
    /// `i`'s links, in edge order; the same for `in_*` and the links to it.
    out_offsets: Vec<u32>,
    out_edges: Vec<u32>,
    in_offsets: Vec<u32>,
    in_edges: Vec<u32>,
    by_hash: HashMap<u64, u32>,
}

impl StoredGraph {
    /// The links on page `i`, in the order the crawl found them; none for an unknown page.
    pub fn outlinks(&self, i: u32) -> impl Iterator<Item = &Edge> {
        adjacent(&self.out_offsets, &self.out_edges, i)
            .iter()
            .map(|&e| &self.edges[e as usize])
    }

    /// The links to page `i`, in the order the crawl found them; none for an unknown page.
    pub fn inlinks(&self, i: u32) -> impl Iterator<Item = &Edge> {
        adjacent(&self.in_offsets, &self.in_edges, i)
            .iter()
            .map(|&e| &self.edges[e as usize])
    }

    /// The index of the page with this URL hash.
    pub fn index_of(&self, url_hash: u64) -> Option<u32> {
        self.by_hash.get(&url_hash).copied()
    }

    pub fn anchor(&self, e: &Edge) -> &str {
        &self.anchors[e.anchor as usize]
    }
}

fn adjacent<'a>(offsets: &[u32], edges: &'a [u32], i: u32) -> &'a [u32] {
    let i = i as usize;
    match (offsets.get(i), offsets.get(i + 1)) {
        (Some(&start), Some(&end)) => &edges[start as usize..end as usize],
        _ => &[],
    }
}

/// Why a stored body could not be read.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("graph body is not gzip: {0}")]
    Gzip(#[from] std::io::Error),
    #[error("graph body has no CSG magic")]
    Magic,
    #[error("graph body version {0} is not one we read")]
    Version(u8),
    #[error("graph body is cut short")]
    Truncated,
    #[error("graph body points outside itself")]
    OutOfRange,
    #[error("graph body anchor is not UTF-8")]
    Utf8,
}

/// Keeps the crawl's graph, in the finalize transaction.
pub async fn write(
    tx: &mut Transaction<'_, Postgres>,
    crawl_id: Uuid,
    out: &CrawlOutput,
) -> Result<(), sqlx::Error> {
    let encoded = encode(out);
    sqlx::query("INSERT INTO crawl_graphs (crawl_id, pages, edges, body) VALUES ($1, $2, $3, $4)")
        .bind(crawl_id)
        .bind(i32::try_from(out.pages.len()).unwrap_or(i32::MAX))
        .bind(i32::try_from(encoded.edges).unwrap_or(i32::MAX))
        .bind(encoded.body)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// The crawl's graph; `None` when it has none (a crawl from before graphs were kept, or one the
/// retention cleanup has dropped) or its body can't be read.
pub async fn load(pool: &PgPool, crawl_id: Uuid) -> Result<Option<StoredGraph>, sqlx::Error> {
    let body: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT body FROM crawl_graphs WHERE crawl_id = $1")
            .bind(crawl_id)
            .fetch_optional(pool)
            .await?;
    let Some(body) = body else {
        return Ok(None);
    };
    match decode(&body) {
        Ok(g) => Ok(Some(g)),
        Err(e) => {
            tracing::warn!(%crawl_id, error = %e, "unreadable crawl graph");
            Ok(None)
        }
    }
}

struct Encoded {
    body: Vec<u8>,
    edges: usize,
}

fn encode(out: &CrawlOutput) -> Encoded {
    let facts = graph::facts(&out.pages, &out.links);
    let n = out.pages.len();

    let mut anchors: Vec<String> = Vec::new();
    let mut anchor_ids: HashMap<String, u32> = HashMap::new();
    let mut seen: HashSet<(u32, u32)> = HashSet::new();
    let mut edges: Vec<Edge> = Vec::new();
    for e in &out.links.edges {
        if e.from as usize >= n || e.to as usize >= n || !seen.insert((e.from, e.to)) {
            continue;
        }
        let text = truncate_chars(
            out.links
                .anchors
                .get(e.anchor as usize)
                .map_or("", String::as_str),
            MAX_ANCHOR_CHARS,
        );
        let anchor = match anchor_ids.get(text) {
            Some(&id) => id,
            None => {
                let id = anchors.len() as u32;
                anchors.push(text.to_owned());
                anchor_ids.insert(text.to_owned(), id);
                id
            }
        };
        edges.push(Edge {
            from: e.from,
            to: e.to,
            anchor,
            nofollow: e.nofollow,
        });
    }

    let mut buf = Vec::with_capacity(16 + n * 13 + edges.len() * 13);
    buf.extend_from_slice(MAGIC);
    buf.push(VERSION);
    buf.extend_from_slice(&(n as u32).to_le_bytes());
    buf.extend_from_slice(&(anchors.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(edges.len() as u32).to_le_bytes());
    for (i, p) in out.pages.iter().enumerate() {
        buf.extend_from_slice(&p.url_hash.to_le_bytes());
        buf.extend_from_slice(&facts.parent[i].unwrap_or(NO_PARENT).to_le_bytes());
        buf.push(facts.link_score[i]);
    }
    for a in &anchors {
        buf.extend_from_slice(&(a.len() as u32).to_le_bytes());
        buf.extend_from_slice(a.as_bytes());
    }
    for e in &edges {
        buf.extend_from_slice(&e.from.to_le_bytes());
        buf.extend_from_slice(&e.to.to_le_bytes());
        buf.extend_from_slice(&e.anchor.to_le_bytes());
        buf.push(u8::from(e.nofollow));
    }

    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(&buf).expect("writing to a Vec never fails");
    Encoded {
        body: gz.finish().expect("writing to a Vec never fails"),
        edges: edges.len(),
    }
}

/// The first `max` chars of `s`.
fn truncate_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((at, _)) => &s[..at],
        None => s,
    }
}

/// Reads a body [`write`] stored. Never panics on a damaged or unknown body.
pub fn decode(body: &[u8]) -> Result<StoredGraph, DecodeError> {
    let mut buf = Vec::new();
    GzDecoder::new(body).read_to_end(&mut buf)?;
    let mut r = Reader { buf: &buf, at: 0 };
    if r.take(3)? != MAGIC {
        return Err(DecodeError::Magic);
    }
    let version = r.u8()?;
    if version != VERSION {
        return Err(DecodeError::Version(version));
    }
    let n = r.u32()?;
    let anchor_count = r.u32()?;
    let edge_count = r.u32()?;

    // Counts come from the body, so nothing is reserved up front past what it can hold.
    let mut pages = Vec::with_capacity((n as usize).min(r.left() / 13));
    for _ in 0..n {
        let url_hash = r.u64()?;
        let parent = r.u32()?;
        let link_score = r.u8()?;
        if parent != NO_PARENT && parent >= n {
            return Err(DecodeError::OutOfRange);
        }
        pages.push(StoredPage {
            url_hash,
            parent: (parent != NO_PARENT).then_some(parent),
            link_score,
        });
    }
    let mut anchors = Vec::with_capacity((anchor_count as usize).min(r.left() / 4));
    for _ in 0..anchor_count {
        let len = r.u32()? as usize;
        let text = std::str::from_utf8(r.take(len)?).map_err(|_| DecodeError::Utf8)?;
        anchors.push(text.to_owned());
    }
    let mut edges = Vec::with_capacity((edge_count as usize).min(r.left() / 13));
    for _ in 0..edge_count {
        let e = Edge {
            from: r.u32()?,
            to: r.u32()?,
            anchor: r.u32()?,
            nofollow: r.u8()? != 0,
        };
        if e.from >= n || e.to >= n || e.anchor >= anchor_count {
            return Err(DecodeError::OutOfRange);
        }
        edges.push(e);
    }

    let (out_offsets, out_edges) = csr(n as usize, &edges, |e| e.from);
    let (in_offsets, in_edges) = csr(n as usize, &edges, |e| e.to);
    let mut by_hash = HashMap::with_capacity(pages.len());
    for (i, p) in pages.iter().enumerate() {
        by_hash.entry(p.url_hash).or_insert(i as u32);
    }
    Ok(StoredGraph {
        pages,
        edges,
        anchors,
        out_offsets,
        out_edges,
        in_offsets,
        in_edges,
        by_hash,
    })
}

/// Edge indexes grouped by the page `key` picks, each group in edge order, with the offsets
/// where each page's group starts (`n + 1` of them).
fn csr(n: usize, edges: &[Edge], key: impl Fn(&Edge) -> u32) -> (Vec<u32>, Vec<u32>) {
    let mut offsets = vec![0u32; n + 1];
    for e in edges {
        offsets[key(e) as usize + 1] += 1;
    }
    for i in 0..n {
        offsets[i + 1] += offsets[i];
    }
    let mut next = offsets.clone();
    let mut grouped = vec![0u32; edges.len()];
    for (i, e) in edges.iter().enumerate() {
        let slot = &mut next[key(e) as usize];
        grouped[*slot as usize] = i as u32;
        *slot += 1;
    }
    (offsets, grouped)
}

struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn left(&self) -> usize {
        self.buf.len() - self.at
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        if len > self.left() {
            return Err(DecodeError::Truncated);
        }
        let bytes = &self.buf[self.at..self.at + len];
        self.at += len;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        let mut b = [0u8; 8];
        b.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_core::check::IssueBits;
    use codoseo_core::crawl::SitemapSummary;
    use codoseo_core::output::{LinkGraph, SiteSignals, StopReason};
    use codoseo_core::page::{Indexability, PageFields, PageRecord};
    use url::Url;

    fn page(i: u64, depth: Option<u16>) -> PageRecord {
        PageRecord {
            url: Url::parse(&format!("https://e.com/p{i}")).unwrap(),
            url_hash: 1000 + i,
            status: 200,
            redirect_chain: Vec::new(),
            response_ms: 0,
            size_bytes: 0,
            content_type: None,
            depth,
            in_sitemap: false,
            indexability: Indexability::Indexable,
            fields: PageFields::default(),
            inlinks: 0,
            outlinks_internal: 0,
            outlinks_external: 0,
            issues: IssueBits::default(),
            key_hash: 0,
            redirect_target: None,
            outlinks_nofollow: 0,
            error: None,
        }
    }

    fn edge(from: u32, to: u32, anchor: u32, nofollow: bool) -> Edge {
        Edge {
            from,
            to,
            anchor,
            nofollow,
        }
    }

    fn output() -> CrawlOutput {
        let long = "é".repeat(250);
        CrawlOutput {
            origin: Url::parse("https://e.com/").unwrap(),
            pages: vec![
                page(0, Some(0)),
                page(1, Some(1)),
                page(2, Some(2)),
                page(3, None),
            ],
            links: LinkGraph {
                edges: vec![
                    edge(0, 1, 0, false),
                    edge(1, 2, 1, true),
                    // A repeat of the first link: dropped.
                    edge(0, 1, 2, false),
                    edge(2, 0, 2, false),
                    edge(0, 2, 3, false),
                    // Points past the pages: dropped.
                    edge(0, 9, 0, false),
                ],
                anchors: vec![
                    "Home".to_owned(),
                    "日本語 ✓".to_owned(),
                    "Again".to_owned(),
                    long,
                ],
            },
            robots: None,
            sitemap: SitemapSummary::default(),
            sitemap_files: Vec::new(),
            stop: StopReason::Completed,
            duration_ms: 0,
            signals: SiteSignals::default(),
        }
    }

    #[test]
    fn a_graph_round_trips() {
        let out = output();
        let encoded = encode(&out);
        assert_eq!(encoded.edges, 4);
        let g = decode(&encoded.body).unwrap();

        let facts = graph::facts(&out.pages, &out.links);
        assert_eq!(g.pages.len(), 4);
        for (i, p) in g.pages.iter().enumerate() {
            assert_eq!(p.url_hash, 1000 + i as u64);
            assert_eq!(p.parent, facts.parent[i]);
            assert_eq!(p.link_score, facts.link_score[i]);
        }
        assert_eq!(
            g.pages.iter().map(|p| p.parent).collect::<Vec<_>>(),
            vec![None, Some(0), Some(1), None]
        );
        let pairs: Vec<_> = g
            .edges
            .iter()
            .map(|e| (e.from, e.to, g.anchor(e), e.nofollow))
            .collect();
        assert_eq!(pairs[0], (0, 1, "Home", false));
        assert_eq!(pairs[1], (1, 2, "日本語 ✓", true));
        assert_eq!(pairs[2], (2, 0, "Again", false));
        // Cut to 200 chars, on a char boundary.
        assert_eq!(pairs[3].2, "é".repeat(200));
        assert_eq!(pairs.len(), 4);
        // Only the anchors the kept edges use, once each.
        assert_eq!(g.anchors.len(), 4);

        let out_of_0: Vec<_> = g.outlinks(0).map(|e| e.to).collect();
        assert_eq!(out_of_0, vec![1, 2]);
        let into_2: Vec<_> = g.inlinks(2).map(|e| e.from).collect();
        assert_eq!(into_2, vec![1, 0]);
        assert_eq!(g.inlinks(3).count(), 0);
        assert_eq!(g.outlinks(99).count(), 0);
        assert_eq!(g.index_of(1002), Some(2));
        assert_eq!(g.index_of(7), None);
    }

    #[test]
    fn an_empty_crawl_round_trips() {
        let mut out = output();
        out.pages.clear();
        out.links = LinkGraph::default();
        let g = decode(&encode(&out).body).unwrap();
        assert!(g.pages.is_empty() && g.edges.is_empty() && g.anchors.is_empty());
        assert_eq!(g.outlinks(0).count(), 0);
    }

    fn gzip(raw: &[u8]) -> Vec<u8> {
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(raw).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn a_damaged_or_unknown_body_is_an_error() {
        assert!(matches!(decode(b"not gzip"), Err(DecodeError::Gzip(_))));
        assert!(matches!(decode(&gzip(b"XYZ\x01")), Err(DecodeError::Magic)));
        assert!(matches!(
            decode(&gzip(b"CSG\x02")),
            Err(DecodeError::Version(2))
        ));

        let body = encode(&output()).body;
        let mut raw = Vec::new();
        GzDecoder::new(&body[..]).read_to_end(&mut raw).unwrap();
        for cut in 0..raw.len() {
            assert!(decode(&gzip(&raw[..cut])).is_err(), "cut at {cut}");
        }
        // A huge page count with nothing behind it.
        let mut huge = b"CSG\x01".to_vec();
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        huge.extend_from_slice(&[0; 8]);
        assert!(matches!(decode(&gzip(&huge)), Err(DecodeError::Truncated)));
        // An edge pointing past the pages.
        let mut bad = raw.clone();
        let last_edge = bad.len() - 13;
        bad[last_edge..last_edge + 4].copy_from_slice(&50u32.to_le_bytes());
        assert!(matches!(decode(&gzip(&bad)), Err(DecodeError::OutOfRange)));
    }
}
