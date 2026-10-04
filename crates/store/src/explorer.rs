//! URL explorer queries over a crawl's `pages`.
//!
//! [`PageFilter`] is the filter vocabulary shared by the explorer, the CSV export, the sidebar
//! report links and the audit screen's issue rows. Its string form (`s4`, `nx`,
//! `check:title_missing`) is what goes in the URL.
//!
//! The queries below back the explorer screen: the filter counts, a keyset-paginated page of
//! grid rows (`p.id > after ORDER BY p.id`), one page's full record, and its stored inlinks.
//! A search term `q` matches the URL or the title, case-insensitively, as a substring.

use codoseo_core::check::CheckId;
use codoseo_core::page::Indexability;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::hash;

/// One explorer filter. Filters map to a SQL predicate over `pages` (aliased `p`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageFilter {
    All,
    Status2xx,
    Status3xx,
    Status4xx,
    Status5xx,
    /// No HTTP response at all: timeouts, connection errors, redirect loops.
    NoResponse,
    Indexable,
    NonIndexable,
    Html,
    Image,
    Other,
    /// Pages with this check's issue bit set.
    Check(CheckId),
}

impl PageFilter {
    /// Parses the URL form; anything unknown falls back to `All`.
    pub fn parse(s: &str) -> PageFilter {
        match s {
            "s2" => PageFilter::Status2xx,
            "s3" => PageFilter::Status3xx,
            "s4" => PageFilter::Status4xx,
            "s5" => PageFilter::Status5xx,
            "s0" => PageFilter::NoResponse,
            "ix" => PageFilter::Indexable,
            "nx" => PageFilter::NonIndexable,
            "html" => PageFilter::Html,
            "img" => PageFilter::Image,
            "oth" => PageFilter::Other,
            _ => s
                .strip_prefix("check:")
                .and_then(CheckId::from_slug)
                .map_or(PageFilter::All, PageFilter::Check),
        }
    }

    /// The URL form; `parse(f.key()) == f`.
    pub fn key(&self) -> String {
        match self {
            PageFilter::All => "all".to_owned(),
            PageFilter::Status2xx => "s2".to_owned(),
            PageFilter::Status3xx => "s3".to_owned(),
            PageFilter::Status4xx => "s4".to_owned(),
            PageFilter::Status5xx => "s5".to_owned(),
            PageFilter::NoResponse => "s0".to_owned(),
            PageFilter::Indexable => "ix".to_owned(),
            PageFilter::NonIndexable => "nx".to_owned(),
            PageFilter::Html => "html".to_owned(),
            PageFilter::Image => "img".to_owned(),
            PageFilter::Other => "oth".to_owned(),
            PageFilter::Check(id) => format!("check:{}", id.slug()),
        }
    }

    /// A SQL predicate over `pages p`. Only constants are interpolated (status ranges, enum
    /// labels and a check's bit number), never user input.
    pub fn predicate(&self) -> String {
        match self {
            PageFilter::All => "TRUE".to_owned(),
            PageFilter::Status2xx => "p.status BETWEEN 200 AND 299".to_owned(),
            PageFilter::Status3xx => "p.status BETWEEN 300 AND 399".to_owned(),
            PageFilter::Status4xx => "p.status BETWEEN 400 AND 499".to_owned(),
            PageFilter::Status5xx => "p.status BETWEEN 500 AND 599".to_owned(),
            PageFilter::NoResponse => "p.status = 0".to_owned(),
            PageFilter::Indexable => "p.indexability = 'indexable'".to_owned(),
            PageFilter::NonIndexable => "p.indexability <> 'indexable'".to_owned(),
            PageFilter::Html => "p.content_type ILIKE 'text/html%'".to_owned(),
            PageFilter::Image => "p.content_type ILIKE 'image/%'".to_owned(),
            PageFilter::Other => "NOT (coalesce(p.content_type, '') ILIKE 'text/html%' \
                                  OR coalesce(p.content_type, '') ILIKE 'image/%')"
                .to_owned(),
            PageFilter::Check(id) => format!("(p.issues & (1::bigint << {})) <> 0", *id as u8),
        }
    }
}

/// The fixed filters, in the order the explorer lists them.
pub const FIXED_FILTERS: [PageFilter; 11] = [
    PageFilter::All,
    PageFilter::Status2xx,
    PageFilter::Status3xx,
    PageFilter::Status4xx,
    PageFilter::Status5xx,
    PageFilter::NoResponse,
    PageFilter::Indexable,
    PageFilter::NonIndexable,
    PageFilter::Html,
    PageFilter::Image,
    PageFilter::Other,
];

/// How many of a crawl's pages each fixed filter matches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, FromRow)]
pub struct FilterCounts {
    pub all: i64,
    pub s2: i64,
    pub s3: i64,
    pub s4: i64,
    pub s5: i64,
    pub s0: i64,
    pub ix: i64,
    pub nx: i64,
    pub html: i64,
    pub img: i64,
    pub oth: i64,
}

impl FilterCounts {
    /// The count for a fixed filter; `None` for a check filter (those come from the summary).
    pub fn get(&self, filter: PageFilter) -> Option<i64> {
        Some(match filter {
            PageFilter::All => self.all,
            PageFilter::Status2xx => self.s2,
            PageFilter::Status3xx => self.s3,
            PageFilter::Status4xx => self.s4,
            PageFilter::Status5xx => self.s5,
            PageFilter::NoResponse => self.s0,
            PageFilter::Indexable => self.ix,
            PageFilter::NonIndexable => self.nx,
            PageFilter::Html => self.html,
            PageFilter::Image => self.img,
            PageFilter::Other => self.oth,
            PageFilter::Check(_) => return None,
        })
    }
}

/// Every fixed filter's count over one crawl, in a single pass (`count(*) FILTER (WHERE …)`).
pub async fn filter_counts(pool: &PgPool, crawl_id: Uuid) -> Result<FilterCounts, sqlx::Error> {
    // The aliases are the filter keys, which are also the field names; all constants.
    let cols = FIXED_FILTERS
        .iter()
        .map(|f| {
            format!(
                "count(*) FILTER (WHERE {}) AS \"{}\"",
                f.predicate(),
                f.key()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    sqlx::query_as(&format!("SELECT {cols} FROM pages p WHERE p.crawl_id = $1"))
        .bind(crawl_id)
        .fetch_one(pool)
        .await
}

/// `q` as an `ILIKE` pattern matching it anywhere, with `%`, `_` and `\` taken literally.
/// `None` for a blank search.
pub fn contains_pattern(q: &str) -> Option<String> {
    let q = q.trim();
    if q.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(q.len() + 2);
    out.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    Some(out)
}

/// The search predicate over `pages p`, with the pattern bound as parameter `$n` (NULL
/// matches everything).
fn search_clause(n: u8) -> String {
    format!(
        "(${n}::text IS NULL OR p.url ILIKE ${n} ESCAPE '\\' OR p.title ILIKE ${n} ESCAPE '\\')"
    )
}

/// One row of the explorer grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridRow {
    /// `pages.id`, the keyset pagination cursor.
    pub id: i64,
    pub url: String,
    pub url_hash: u64,
    pub status: u16,
    pub indexability: Indexability,
    pub content_type: Option<String>,
    pub title: Option<String>,
    pub word_count: Option<u32>,
    pub depth: Option<u32>,
    pub inlinks: u32,
    pub response_ms: Option<u32>,
}

#[derive(FromRow)]
struct GridRowDb {
    id: i64,
    url: String,
    url_hash: i64,
    status: i16,
    indexability: String,
    content_type: Option<String>,
    title: Option<String>,
    word_count: Option<i32>,
    depth: Option<i32>,
    inlinks: i32,
    response_ms: Option<i32>,
}

impl From<GridRowDb> for GridRow {
    fn from(r: GridRowDb) -> GridRow {
        GridRow {
            id: r.id,
            url: r.url,
            url_hash: hash::from_db(r.url_hash),
            status: u16::try_from(r.status).unwrap_or(0),
            indexability: indexability(&r.indexability),
            content_type: r.content_type,
            title: r.title,
            word_count: r.word_count.map(unsigned),
            depth: r.depth.map(unsigned),
            inlinks: unsigned(r.inlinks),
            response_ms: r.response_ms.map(unsigned),
        }
    }
}

fn unsigned(n: i32) -> u32 {
    u32::try_from(n).unwrap_or(0)
}

/// The Postgres `indexability` label as the core enum (they share the snake_case names).
fn indexability(label: &str) -> Indexability {
    serde_json::from_value(serde_json::Value::String(label.to_owned()))
        .unwrap_or(Indexability::ServerError)
}

/// Up to `limit` grid rows of the crawl matching `filter` and the search `q`, with
/// `pages.id > after`, in `id` order. Ask for one row more than you show to learn whether
/// another page follows.
pub async fn rows(
    pool: &PgPool,
    crawl_id: Uuid,
    filter: PageFilter,
    q: &str,
    after: i64,
    limit: i64,
) -> Result<Vec<GridRow>, sqlx::Error> {
    let rows: Vec<GridRowDb> = sqlx::query_as(&format!(
        "SELECT p.id, p.url, p.url_hash, p.status, p.indexability::text AS indexability, \
                p.content_type, p.title, p.word_count, p.depth, p.inlinks, p.response_ms \
         FROM pages p \
         WHERE p.crawl_id = $1 AND p.id > $2 AND ({}) AND {} \
         ORDER BY p.id LIMIT $4",
        filter.predicate(),
        search_clause(3),
    ))
    .bind(crawl_id)
    .bind(after)
    .bind(contains_pattern(q))
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(GridRow::from).collect())
}

/// `(matching, total)`: how many of the crawl's pages match `filter` and `q`, and how many
/// pages the crawl has.
pub async fn match_count(
    pool: &PgPool,
    crawl_id: Uuid,
    filter: PageFilter,
    q: &str,
) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT count(*) FILTER (WHERE ({}) AND {}), count(*) FROM pages p WHERE p.crawl_id = $1",
        filter.predicate(),
        search_clause(2),
    ))
    .bind(crawl_id)
    .bind(contains_pattern(q))
    .fetch_one(pool)
    .await
}

/// Everything stored about one page of a crawl, for the explorer's detail panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageDetail {
    pub id: i64,
    pub url: String,
    pub url_hash: u64,
    pub status: u16,
    /// Every redirect hop in order: the status and the URL that returned it.
    pub redirect_chain: Vec<(u16, String)>,
    pub response_ms: Option<u32>,
    pub size_bytes: Option<u64>,
    pub content_type: Option<String>,
    pub depth: Option<u32>,
    pub in_sitemap: bool,
    pub indexability: Indexability,
    pub title: Option<String>,
    pub meta_description: Option<String>,
    pub meta_robots: Option<String>,
    pub x_robots_tag: Option<String>,
    pub canonical: Option<String>,
    pub h1: Vec<String>,
    pub h2: Vec<String>,
    pub word_count: Option<u32>,
    pub inlinks: u32,
    pub outlinks_internal: u32,
    pub outlinks_external: u32,
    /// The issue bitmask (`IssueBits`).
    pub issues: u64,
}

#[derive(FromRow)]
struct PageDetailDb {
    id: i64,
    url: String,
    url_hash: i64,
    status: i16,
    redirect_chain: serde_json::Value,
    response_ms: Option<i32>,
    size_bytes: Option<i64>,
    content_type: Option<String>,
    depth: Option<i32>,
    in_sitemap: bool,
    indexability: String,
    title: Option<String>,
    meta_description: Option<String>,
    meta_robots: Option<String>,
    x_robots_tag: Option<String>,
    canonical: Option<String>,
    h1: serde_json::Value,
    h2: serde_json::Value,
    word_count: Option<i32>,
    inlinks: i32,
    outlinks_internal: i32,
    outlinks_external: i32,
    issues: i64,
}

impl From<PageDetailDb> for PageDetail {
    fn from(r: PageDetailDb) -> PageDetail {
        PageDetail {
            id: r.id,
            url: r.url,
            url_hash: hash::from_db(r.url_hash),
            status: u16::try_from(r.status).unwrap_or(0),
            redirect_chain: serde_json::from_value(r.redirect_chain).unwrap_or_default(),
            response_ms: r.response_ms.map(unsigned),
            size_bytes: r.size_bytes.and_then(|n| u64::try_from(n).ok()),
            content_type: r.content_type,
            depth: r.depth.map(unsigned),
            in_sitemap: r.in_sitemap,
            indexability: indexability(&r.indexability),
            title: r.title,
            meta_description: r.meta_description,
            meta_robots: r.meta_robots,
            x_robots_tag: r.x_robots_tag,
            canonical: r.canonical,
            h1: serde_json::from_value(r.h1).unwrap_or_default(),
            h2: serde_json::from_value(r.h2).unwrap_or_default(),
            word_count: r.word_count.map(unsigned),
            inlinks: unsigned(r.inlinks),
            outlinks_internal: unsigned(r.outlinks_internal),
            outlinks_external: unsigned(r.outlinks_external),
            issues: hash::from_db(r.issues),
        }
    }
}

/// One page of a crawl by URL hash.
pub async fn page(
    pool: &PgPool,
    site_id: Uuid,
    crawl_id: Uuid,
    url_hash: u64,
) -> Result<Option<PageDetail>, sqlx::Error> {
    let row: Option<PageDetailDb> = sqlx::query_as(
        "SELECT id, url, url_hash, status, redirect_chain, response_ms, size_bytes, content_type, \
                depth, in_sitemap, indexability::text AS indexability, title, meta_description, \
                meta_robots, x_robots_tag, canonical, h1, h2, word_count, inlinks, \
                outlinks_internal, outlinks_external, issues \
         FROM pages WHERE site_id = $1 AND url_hash = $2 AND crawl_id = $3 \
         ORDER BY id LIMIT 1",
    )
    .bind(site_id)
    .bind(hash::to_db(url_hash))
    .bind(crawl_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(PageDetail::from))
}

/// Whether any kept crawl of the site has a page with this URL hash.
pub async fn page_exists(pool: &PgPool, site_id: Uuid, url_hash: u64) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pages WHERE site_id = $1 AND url_hash = $2)")
        .bind(site_id)
        .bind(hash::to_db(url_hash))
        .fetch_one(pool)
        .await
}

/// A stored link to a page: one of its samples, or any link to a broken or redirecting page.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct Inlink {
    pub from_url: String,
    pub anchor_text: Option<String>,
    pub nofollow: bool,
}

/// Up to `limit` stored inlinks to the page with `url_hash` in this crawl.
pub async fn inlinks(
    pool: &PgPool,
    crawl_id: Uuid,
    url_hash: u64,
    limit: i64,
) -> Result<Vec<Inlink>, sqlx::Error> {
    sqlx::query_as(
        "SELECT from_url, anchor_text, nofollow FROM inlinks \
         WHERE crawl_id = $1 AND target_url_hash = $2 ORDER BY id LIMIT $3",
    )
    .bind(crawl_id)
    .bind(hash::to_db(url_hash))
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_patterns_escape_wildcards() {
        assert_eq!(contains_pattern("  "), None);
        assert_eq!(contains_pattern(" Blog ").as_deref(), Some("%Blog%"));
        assert_eq!(contains_pattern("50%_off").as_deref(), Some(r"%50\%\_off%"));
        assert_eq!(contains_pattern(r"a\b").as_deref(), Some(r"%a\\b%"));
    }

    #[test]
    fn fixed_filter_counts_line_up_with_keys() {
        let c = FilterCounts {
            all: 1,
            s2: 2,
            s3: 3,
            s4: 4,
            s5: 5,
            s0: 6,
            ix: 7,
            nx: 8,
            html: 9,
            img: 10,
            oth: 11,
        };
        let got: Vec<i64> = FIXED_FILTERS.iter().filter_map(|f| c.get(*f)).collect();
        assert_eq!(got, (1..=11).collect::<Vec<_>>());
        assert_eq!(c.get(PageFilter::Check(CheckId::TitleMissing)), None);
    }

    #[test]
    fn keys_round_trip() {
        let mut all = vec![
            PageFilter::All,
            PageFilter::Status2xx,
            PageFilter::Status3xx,
            PageFilter::Status4xx,
            PageFilter::Status5xx,
            PageFilter::NoResponse,
            PageFilter::Indexable,
            PageFilter::NonIndexable,
            PageFilter::Html,
            PageFilter::Image,
            PageFilter::Other,
        ];
        all.extend(CheckId::ALL.iter().map(|&c| PageFilter::Check(c)));
        for f in all {
            assert_eq!(PageFilter::parse(&f.key()), f, "{}", f.key());
        }
    }

    #[test]
    fn unknown_filters_fall_back_to_all() {
        assert_eq!(PageFilter::parse("check:nope"), PageFilter::All);
        assert_eq!(
            PageFilter::parse("'; DROP TABLE pages; --"),
            PageFilter::All
        );
    }
}
