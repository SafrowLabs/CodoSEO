//! The structure of a site's sitemaps: each file discovery fetched, what it lists and how those
//! URLs fared in the crawl.
//!
//! The crawler records a [`SitemapFile`] per fetch attempt, with the URLs it listed kept in memory
//! only. Finalize turns them into a [`SitemapTree`], which is what gets stored: per file, its
//! lastmod stats, a [`SitemapBuckets`] breakdown of its URLs and a few sample URLs per bucket.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::page::{Indexability, PageRecord};
use crate::url::url_hash;

/// Sample URLs a tree keeps per file and bucket.
pub const MAX_SAMPLES: usize = 10;

/// What a sitemap file is: a list of pages or a list of other sitemaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SitemapKind {
    UrlSet,
    Index,
}

/// What the `<lastmod>` of a file's `<url>` entries say. Dates are the first 10 characters of
/// the value (`YYYY-MM-DD`); a value that doesn't start with a date is not counted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LastmodStats {
    /// Entries with a lastmod.
    pub with: u32,
    /// Distinct dates among them.
    pub distinct: u32,
    pub min: Option<String>,
    pub max: Option<String>,
    /// Entries dated after the day the file was fetched (UTC).
    pub future: u32,
}

/// One sitemap file discovery tried to read, in the order it was fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SitemapFile {
    pub url: Url,
    /// `None` when the file failed.
    pub kind: Option<SitemapKind>,
    /// Index into `CrawlOutput::sitemap_files` of the index that listed it; `None` for a seed.
    pub parent: Option<u32>,
    /// It could not be fetched (or answered other than 2xx) or was not a sitemap.
    pub failed: bool,
    /// `<loc>` values it listed, child sitemaps for an index.
    pub listed: u32,
    #[serde(default)]
    pub lastmod: LastmodStats,
    /// The normalized page URLs it listed, for [`SitemapTree::build`]. Kept in memory only, and
    /// only up to discovery's own cap, so `listed` can be more than these.
    #[serde(skip)]
    pub urls: Vec<String>,
}

/// How one URL a sitemap lists fared in the crawl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SitemapBucket {
    /// Indexable and reachable through links.
    Ok,
    /// Indexable, but found only through the sitemap.
    Orphan,
    /// Answers 2xx but is not indexable: noindex, canonicalised elsewhere, blocked by robots.txt.
    Noindex,
    Redirect,
    /// 4xx, 5xx or no response.
    Broken,
    /// The crawl never recorded it (page limit, time limit, outside the site).
    NotCrawled,
}

/// The bucket of a listed URL, given the crawl's record of it.
pub fn classify(page: Option<&PageRecord>) -> SitemapBucket {
    let Some(page) = page else {
        return SitemapBucket::NotCrawled;
    };
    match page.indexability {
        Indexability::Redirected => SitemapBucket::Redirect,
        Indexability::ClientError | Indexability::ServerError => SitemapBucket::Broken,
        Indexability::Indexable if page.depth.is_none() => SitemapBucket::Orphan,
        Indexability::Indexable => SitemapBucket::Ok,
        Indexability::Noindex | Indexability::Canonicalised | Indexability::BlockedByRobots => {
            SitemapBucket::Noindex
        }
    }
}

/// How many of a file's URLs fell in each [`SitemapBucket`]. An index sums its children.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SitemapBuckets {
    pub ok: u32,
    pub orphan: u32,
    pub noindex: u32,
    pub redirect: u32,
    pub broken: u32,
    pub not_crawled: u32,
}

impl SitemapBuckets {
    fn add(&mut self, bucket: SitemapBucket) {
        *self.slot(bucket) += 1;
    }

    fn slot(&mut self, bucket: SitemapBucket) -> &mut u32 {
        match bucket {
            SitemapBucket::Ok => &mut self.ok,
            SitemapBucket::Orphan => &mut self.orphan,
            SitemapBucket::Noindex => &mut self.noindex,
            SitemapBucket::Redirect => &mut self.redirect,
            SitemapBucket::Broken => &mut self.broken,
            SitemapBucket::NotCrawled => &mut self.not_crawled,
        }
    }

    fn absorb(&mut self, other: &SitemapBuckets) {
        self.ok += other.ok;
        self.orphan += other.orphan;
        self.noindex += other.noindex;
        self.redirect += other.redirect;
        self.broken += other.broken;
        self.not_crawled += other.not_crawled;
    }
}

/// Up to [`MAX_SAMPLES`] URLs of each bucket worth a look, in the order the file lists them.
/// Only a urlset has samples.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SitemapSamples {
    pub orphan: Vec<String>,
    pub noindex: Vec<String>,
    pub redirect: Vec<String>,
    pub broken: Vec<String>,
    pub not_crawled: Vec<String>,
}

impl SitemapSamples {
    fn add(&mut self, bucket: SitemapBucket, url: &str) {
        let list = match bucket {
            SitemapBucket::Ok => return,
            SitemapBucket::Orphan => &mut self.orphan,
            SitemapBucket::Noindex => &mut self.noindex,
            SitemapBucket::Redirect => &mut self.redirect,
            SitemapBucket::Broken => &mut self.broken,
            SitemapBucket::NotCrawled => &mut self.not_crawled,
        };
        if list.len() < MAX_SAMPLES {
            list.push(url.to_owned());
        }
    }
}

/// One file of a [`SitemapTree`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SitemapTreeFile {
    pub url: String,
    /// `None` when the file failed.
    pub kind: Option<SitemapKind>,
    /// Index into `SitemapTree::files` of the index that listed it; `None` for a seed.
    pub parent: Option<u32>,
    pub failed: bool,
    pub listed: u32,
    #[serde(default)]
    pub lastmod: LastmodStats,
    #[serde(default)]
    pub buckets: SitemapBuckets,
    #[serde(default)]
    pub samples: SitemapSamples,
}

/// A crawl's sitemaps as stored in `site_files.sitemap_tree`: every file discovery tried, in
/// fetch order, so a parent always comes before its children.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SitemapTree {
    #[serde(default)]
    pub files: Vec<SitemapTreeFile>,
}

impl SitemapTree {
    /// Classifies each file's URLs against the crawl's `pages` (a URL a file lists twice counts
    /// once) and sums every index's children into it.
    pub fn build(files: &[SitemapFile], pages: &[PageRecord]) -> SitemapTree {
        let by_hash: HashMap<u64, &PageRecord> = pages.iter().map(|p| (p.url_hash, p)).collect();
        let mut out: Vec<SitemapTreeFile> = files
            .iter()
            .map(|f| {
                let mut buckets = SitemapBuckets::default();
                let mut samples = SitemapSamples::default();
                let mut seen: HashSet<u64> = HashSet::new();
                for url in &f.urls {
                    let Ok(parsed) = Url::parse(url) else {
                        continue;
                    };
                    let hash = url_hash(&parsed);
                    if !seen.insert(hash) {
                        continue;
                    }
                    let bucket = classify(by_hash.get(&hash).copied());
                    buckets.add(bucket);
                    samples.add(bucket, url);
                }
                SitemapTreeFile {
                    url: f.url.to_string(),
                    kind: f.kind,
                    parent: f.parent,
                    failed: f.failed,
                    listed: f.listed,
                    lastmod: f.lastmod.clone(),
                    buckets,
                    samples,
                }
            })
            .collect();
        // Children come after their parent, so going backwards sums nested indexes bottom-up.
        for i in (0..out.len()).rev() {
            let Some(parent) = out[i].parent.map(|p| p as usize) else {
                continue;
            };
            if parent < i {
                let child = out[i].buckets.clone();
                out[parent].buckets.absorb(&child);
            }
        }
        SitemapTree { files: out }
    }
}
