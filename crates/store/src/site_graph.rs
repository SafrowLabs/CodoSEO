//! Read queries behind the site graph's sitemap, depth, broken & redirects and changes views:
//! a crawl's `pages` counted by sitemap listing, click depth and top folder, its broken pages
//! and redirect chains with the pages linking to them, and its pages diffed against an earlier
//! crawl's by `url_hash`.
//!
//! A page's top folder is the first segment of its path when more follows it (`/blog` for
//! `/blog/` and `/blog/a`), and `""` for a page at the top level (`/`, `/pricing`).

use std::collections::HashMap;

use codoseo_core::check::CheckId;
use codoseo_core::page::Indexability;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::explorer::PageFilter;
use crate::hash;
use crate::reports::from_slug;
use crate::search::like_escape;

/// `p.url`'s top folder in SQL, as the module docs describe.
const FOLDER: &str = "CASE WHEN coalesce(substring(p.url from '^[A-Za-z]+://[^/]+(/[^?#]*)'), '/') \
     ~ '^/[^/]+/' THEN substring(substring(p.url from '^[A-Za-z]+://[^/]+(/[^?#]*)') from '^/[^/]+') \
     ELSE '' END";

/// The top folder of a URL path, the same way [`FOLDER`] reads it in SQL.
pub fn top_folder(path: &str) -> &str {
    let Some(rest) = path.strip_prefix('/') else {
        return "";
    };
    match rest.find('/') {
        Some(i) if i > 0 => &path[..=i],
        _ => "",
    }
}

/// How the pages a crawl recorded split between the sitemap and links, and what the sitemap
/// lists that it shouldn't.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, FromRow)]
pub struct SitemapOverlap {
    /// Listed in a sitemap.
    pub in_sitemap: i64,
    /// Reached by following links (`depth` set).
    pub linked: i64,
    pub in_both: i64,
    pub only_sitemap: i64,
    pub only_linked: i64,
    /// Of `only_linked`, the indexable ones.
    pub only_linked_indexable: i64,
    /// Of `in_both`, the ones that are not indexable.
    pub both_not_indexable: i64,
    /// Listed in a sitemap and not indexable, by why.
    pub sitemap_redirect: i64,
    pub sitemap_broken: i64,
    /// Noindex or blocked by robots.txt.
    pub sitemap_noindex: i64,
    pub sitemap_canonicalised: i64,
    /// Pages failing the `orphan` and `not_in_sitemap` checks, as the explorer counts them.
    pub orphans: i64,
    pub not_in_sitemap: i64,
}

impl SitemapOverlap {
    pub fn sitemap_not_indexable(&self) -> i64 {
        self.sitemap_redirect
            + self.sitemap_broken
            + self.sitemap_noindex
            + self.sitemap_canonicalised
    }
}

pub async fn sitemap_overlap(pool: &PgPool, crawl_id: Uuid) -> Result<SitemapOverlap, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT count(*) FILTER (WHERE in_sitemap) AS in_sitemap, \
                count(*) FILTER (WHERE depth IS NOT NULL) AS linked, \
                count(*) FILTER (WHERE in_sitemap AND depth IS NOT NULL) AS in_both, \
                count(*) FILTER (WHERE in_sitemap AND depth IS NULL) AS only_sitemap, \
                count(*) FILTER (WHERE NOT in_sitemap AND depth IS NOT NULL) AS only_linked, \
                count(*) FILTER (WHERE NOT in_sitemap AND depth IS NOT NULL \
                                 AND indexability = 'indexable') AS only_linked_indexable, \
                count(*) FILTER (WHERE in_sitemap AND depth IS NOT NULL \
                                 AND indexability <> 'indexable') AS both_not_indexable, \
                count(*) FILTER (WHERE in_sitemap AND indexability = 'redirected') \
                    AS sitemap_redirect, \
                count(*) FILTER (WHERE in_sitemap \
                                 AND indexability IN ('client_error', 'server_error')) \
                    AS sitemap_broken, \
                count(*) FILTER (WHERE in_sitemap \
                                 AND indexability IN ('noindex', 'blocked_by_robots')) \
                    AS sitemap_noindex, \
                count(*) FILTER (WHERE in_sitemap AND indexability = 'canonicalised') \
                    AS sitemap_canonicalised, \
                count(*) FILTER (WHERE {}) AS orphans, \
                count(*) FILTER (WHERE {}) AS not_in_sitemap \
         FROM pages p WHERE crawl_id = $1",
        PageFilter::Check(CheckId::Orphan).predicate(),
        PageFilter::Check(CheckId::NotInSitemap).predicate(),
    ))
    .bind(crawl_id)
    .fetch_one(pool)
    .await
}

/// The top folders of the crawl's pages that match `filter`, with how many each holds, most
/// first (then by name), at most `limit`.
pub async fn top_folders(
    pool: &PgPool,
    crawl_id: Uuid,
    filter: PageFilter,
    limit: i64,
) -> Result<Vec<(String, i64)>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {FOLDER} AS folder, count(*) AS n FROM pages p \
         WHERE p.crawl_id = $1 AND ({}) GROUP BY 1 ORDER BY n DESC, folder LIMIT $2",
        filter.predicate(),
    ))
    .bind(crawl_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// A crawl's pages at one click depth, by what they are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DepthBucket {
    /// `None` for pages no link reached (only in a sitemap).
    pub depth: Option<u32>,
    pub indexable: i64,
    /// Answer 2xx but are kept out of search: noindex, canonicalised, blocked by robots.txt.
    pub noindex: i64,
    pub redirect: i64,
    pub broken: i64,
}

impl DepthBucket {
    pub fn total(&self) -> i64 {
        self.indexable + self.noindex + self.redirect + self.broken
    }
}

/// Every click depth the crawl's pages sit at, shallowest first, pages no link reached last.
pub async fn depth_buckets(pool: &PgPool, crawl_id: Uuid) -> Result<Vec<DepthBucket>, sqlx::Error> {
    let rows: Vec<(Option<i32>, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT depth, \
                count(*) FILTER (WHERE indexability = 'indexable'), \
                count(*) FILTER (WHERE indexability IN \
                                 ('noindex', 'canonicalised', 'blocked_by_robots')), \
                count(*) FILTER (WHERE indexability = 'redirected'), \
                count(*) FILTER (WHERE indexability IN ('client_error', 'server_error')) \
         FROM pages WHERE crawl_id = $1 GROUP BY depth ORDER BY depth NULLS LAST",
    )
    .bind(crawl_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(depth, indexable, noindex, redirect, broken)| DepthBucket {
                depth: depth.and_then(|d| u32::try_from(d).ok()),
                indexable,
                noindex,
                redirect,
                broken,
            },
        )
        .collect())
}

/// Pages per top folder and click depth, depth 1 and deeper, with depths from `cap` on counted
/// at `cap`.
pub async fn folder_depths(
    pool: &PgPool,
    crawl_id: Uuid,
    cap: i32,
) -> Result<Vec<(String, i32, i64)>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {FOLDER} AS folder, LEAST(p.depth, $2) AS d, count(*) FROM pages p \
         WHERE p.crawl_id = $1 AND p.depth >= 1 GROUP BY 1, 2 ORDER BY 1, 2",
    ))
    .bind(crawl_id)
    .bind(cap)
    .fetch_all(pool)
    .await
}

/// A page and how well it is linked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedPage {
    pub url: String,
    pub url_hash: u64,
    pub status: u16,
    pub depth: Option<u32>,
    /// Distinct pages linking to it.
    pub inlinks: u32,
}

#[derive(FromRow)]
struct LinkedPageDb {
    url: String,
    url_hash: i64,
    status: i16,
    depth: Option<i32>,
    inlinks: i32,
}

impl From<LinkedPageDb> for LinkedPage {
    fn from(r: LinkedPageDb) -> LinkedPage {
        LinkedPage {
            url: r.url,
            url_hash: hash::from_db(r.url_hash),
            status: u16::try_from(r.status).unwrap_or(0),
            depth: r.depth.and_then(|d| u32::try_from(d).ok()),
            inlinks: u32::try_from(r.inlinks).unwrap_or(0),
        }
    }
}

/// Up to `limit` indexable pages `min_depth` clicks deep or more, best linked first.
pub async fn deep_pages(
    pool: &PgPool,
    crawl_id: Uuid,
    min_depth: i32,
    limit: i64,
) -> Result<Vec<LinkedPage>, sqlx::Error> {
    let rows: Vec<LinkedPageDb> = sqlx::query_as(
        "SELECT url, url_hash, status, depth, inlinks FROM pages \
         WHERE crawl_id = $1 AND depth >= $2 AND indexability = 'indexable' \
         ORDER BY inlinks DESC, depth DESC, id LIMIT $3",
    )
    .bind(crawl_id)
    .bind(min_depth)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(LinkedPage::from).collect())
}

/// The URLs of the crawl's pages with these hashes.
pub async fn urls_by_hash(
    pool: &PgPool,
    crawl_id: Uuid,
    hashes: &[u64],
) -> Result<HashMap<u64, String>, sqlx::Error> {
    if hashes.is_empty() {
        return Ok(HashMap::new());
    }
    let db: Vec<i64> = hashes.iter().map(|h| hash::to_db(*h)).collect();
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT url_hash, url FROM pages WHERE crawl_id = $1 AND url_hash = ANY($2)",
    )
    .bind(crawl_id)
    .bind(&db)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(h, url)| (hash::from_db(h), url))
        .collect())
}

/// What the broken & redirects view counts over the whole crawl.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, FromRow)]
pub struct LinkTotals {
    /// Pages answering 4xx, 5xx or nothing.
    pub broken: i64,
    /// Pages linking to them, summed over the broken pages.
    pub broken_links: i64,
    /// Pages that redirect (a non-empty `redirect_chain`).
    pub redirects: i64,
    /// Pages linking to them, summed over the redirecting pages.
    pub redirect_links: i64,
    /// The most redirect hops before the final page.
    pub longest_chain: i32,
    /// Redirects that end on a page answering 4xx, 5xx or nothing, or never end (a loop, too
    /// many hops).
    pub ends_broken: i64,
}

/// The join from a redirecting page `p` to the page it lands on, `t`.
const TARGET_JOIN: &str =
    "LEFT JOIN pages t ON t.crawl_id = p.crawl_id AND t.url = p.redirect_target";
/// A redirect that ends broken, over `p` and `t`.
const ENDS_BROKEN: &str = "(p.redirect_target IS NULL OR t.status = 0 OR t.status >= 400)";

pub async fn link_totals(pool: &PgPool, crawl_id: Uuid) -> Result<LinkTotals, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT count(*) FILTER (WHERE p.indexability IN ('client_error', 'server_error')) \
                    AS broken, \
                coalesce(sum(p.inlinks) FILTER (WHERE p.indexability IN \
                         ('client_error', 'server_error')), 0)::bigint AS broken_links, \
                count(*) FILTER (WHERE jsonb_array_length(p.redirect_chain) > 0) AS redirects, \
                coalesce(sum(p.inlinks) FILTER (WHERE jsonb_array_length(p.redirect_chain) > 0), \
                         0)::bigint AS redirect_links, \
                coalesce(max(jsonb_array_length(p.redirect_chain)), 0)::int AS longest_chain, \
                count(*) FILTER (WHERE jsonb_array_length(p.redirect_chain) > 0 \
                                 AND {ENDS_BROKEN}) AS ends_broken \
         FROM pages p {TARGET_JOIN} WHERE p.crawl_id = $1",
    ))
    .bind(crawl_id)
    .fetch_one(pool)
    .await
}

/// Up to `limit` broken pages, most linked first.
pub async fn broken_pages(
    pool: &PgPool,
    crawl_id: Uuid,
    limit: i64,
) -> Result<Vec<LinkedPage>, sqlx::Error> {
    let rows: Vec<LinkedPageDb> = sqlx::query_as(
        "SELECT url, url_hash, status, depth, inlinks FROM pages \
         WHERE crawl_id = $1 AND indexability IN ('client_error', 'server_error') \
         ORDER BY inlinks DESC, id LIMIT $2",
    )
    .bind(crawl_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(LinkedPage::from).collect())
}

/// A page linking to another, with the link's anchor text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSource {
    pub from_url: String,
    pub anchor: Option<String>,
}

/// Up to `limit` distinct pages linking to the page `target_hash`, from the crawl's stored
/// inlinks (every link to a broken or redirecting page is kept), in the order the crawl found
/// them.
pub async fn link_sources(
    pool: &PgPool,
    crawl_id: Uuid,
    target_hash: u64,
    limit: i64,
) -> Result<Vec<LinkSource>, sqlx::Error> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT from_url, anchor_text FROM ( \
             SELECT DISTINCT ON (from_url) id, from_url, anchor_text FROM inlinks \
             WHERE crawl_id = $1 AND target_url_hash = $2 ORDER BY from_url, id) s \
         ORDER BY id LIMIT $3",
    )
    .bind(crawl_id)
    .bind(hash::to_db(target_hash))
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(from_url, anchor)| LinkSource { from_url, anchor })
        .collect())
}

/// Up to `limit` indexable 200 pages that could stand in for a broken one: in the folder
/// `folder_url` (an absolute URL ending in `/`), or ending in the same last path segment `slug`
/// anywhere on the site.
pub async fn stand_ins(
    pool: &PgPool,
    crawl_id: Uuid,
    folder_url: &str,
    slug: &str,
    limit: i64,
) -> Result<Vec<String>, sqlx::Error> {
    let slug_pattern = if slug.is_empty() {
        None
    } else {
        Some(format!("%/{}", like_escape(slug)))
    };
    sqlx::query_scalar(
        "SELECT url FROM pages \
         WHERE crawl_id = $1 AND status = 200 AND indexability = 'indexable' \
           AND (url LIKE $2 ESCAPE '\\' OR url LIKE $3 ESCAPE '\\') \
         ORDER BY id LIMIT $4",
    )
    .bind(crawl_id)
    .bind(format!("{}%", like_escape(folder_url)))
    .bind(slug_pattern)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// A redirecting page: each hop, where it lands and how that page answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectChain {
    pub url: String,
    pub url_hash: u64,
    /// Every hop in order: the status returned and the URL that returned it.
    pub hops: Vec<(u16, String)>,
    /// Where it lands; `None` when it never does (a loop, too many hops).
    pub target: Option<String>,
    /// The status of the page it lands on; `None` when the crawl has no record of it.
    pub target_status: Option<u16>,
    /// Pages linking to the first URL.
    pub inlinks: u32,
}

#[derive(FromRow)]
struct RedirectChainDb {
    url: String,
    url_hash: i64,
    redirect_chain: serde_json::Value,
    redirect_target: Option<String>,
    target_status: Option<i16>,
    inlinks: i32,
}

/// Up to `limit` redirecting pages, most linked first, then longest chain first.
pub async fn redirect_chains(
    pool: &PgPool,
    crawl_id: Uuid,
    limit: i64,
) -> Result<Vec<RedirectChain>, sqlx::Error> {
    let rows: Vec<RedirectChainDb> = sqlx::query_as(&format!(
        "SELECT p.url, p.url_hash, p.redirect_chain, p.redirect_target, \
                t.status AS target_status, p.inlinks \
         FROM pages p {TARGET_JOIN} \
         WHERE p.crawl_id = $1 AND jsonb_array_length(p.redirect_chain) > 0 \
         ORDER BY p.inlinks DESC, jsonb_array_length(p.redirect_chain) DESC, p.id LIMIT $2",
    ))
    .bind(crawl_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| RedirectChain {
            url: r.url,
            url_hash: hash::from_db(r.url_hash),
            hops: serde_json::from_value(r.redirect_chain).unwrap_or_default(),
            target: r.redirect_target,
            target_status: r.target_status.and_then(|s| u16::try_from(s).ok()),
            inlinks: u32::try_from(r.inlinks).unwrap_or(0),
        })
        .collect())
}

/// One page as a crawl recorded it, for [`page_diffs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageState {
    pub status: u16,
    pub depth: Option<u32>,
    pub indexability: Indexability,
    pub in_sitemap: bool,
    pub inlinks: u32,
}

/// A URL whose record differs between two crawls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageDiff {
    pub url: String,
    pub url_hash: u64,
    /// `None` when the earlier crawl didn't have it.
    pub before: Option<PageState>,
    /// `None` when the later crawl doesn't have it.
    pub after: Option<PageState>,
}

#[derive(FromRow)]
struct PageDiffDb {
    url: String,
    url_hash: i64,
    b_status: Option<i16>,
    b_depth: Option<i32>,
    b_indexability: Option<String>,
    b_in_sitemap: Option<bool>,
    b_inlinks: Option<i32>,
    a_status: Option<i16>,
    a_depth: Option<i32>,
    a_indexability: Option<String>,
    a_in_sitemap: Option<bool>,
    a_inlinks: Option<i32>,
}

fn state(
    status: Option<i16>,
    depth: Option<i32>,
    indexability: Option<String>,
    in_sitemap: Option<bool>,
    inlinks: Option<i32>,
) -> Option<PageState> {
    Some(PageState {
        status: u16::try_from(status?).unwrap_or(0),
        depth: depth.and_then(|d| u32::try_from(d).ok()),
        indexability: from_slug(&indexability?).unwrap_or(Indexability::ServerError),
        in_sitemap: in_sitemap?,
        inlinks: inlinks.and_then(|n| u32::try_from(n).ok()).unwrap_or(0),
    })
}

/// Up to `limit` URLs that are new in `after_crawl`, gone from it, or differ from
/// `before_crawl` in status, click depth, indexability or sitemap listing, in URL order.
pub async fn page_diffs(
    pool: &PgPool,
    before_crawl: Uuid,
    after_crawl: Uuid,
    limit: i64,
) -> Result<Vec<PageDiff>, sqlx::Error> {
    let rows: Vec<PageDiffDb> = sqlx::query_as(
        "SELECT coalesce(a.url, b.url) AS url, coalesce(a.url_hash, b.url_hash) AS url_hash, \
                b.status AS b_status, b.depth AS b_depth, \
                b.indexability::text AS b_indexability, b.in_sitemap AS b_in_sitemap, \
                b.inlinks AS b_inlinks, \
                a.status AS a_status, a.depth AS a_depth, \
                a.indexability::text AS a_indexability, a.in_sitemap AS a_in_sitemap, \
                a.inlinks AS a_inlinks \
         FROM (SELECT url, url_hash, status, depth, indexability, in_sitemap, inlinks \
               FROM pages WHERE crawl_id = $2) a \
         FULL JOIN (SELECT url, url_hash, status, depth, indexability, in_sitemap, inlinks \
                    FROM pages WHERE crawl_id = $1) b ON a.url_hash = b.url_hash \
         WHERE a.url_hash IS NULL OR b.url_hash IS NULL OR a.status <> b.status \
            OR a.depth IS DISTINCT FROM b.depth OR a.indexability <> b.indexability \
            OR a.in_sitemap <> b.in_sitemap \
         ORDER BY 1 LIMIT $3",
    )
    .bind(before_crawl)
    .bind(after_crawl)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| PageDiff {
            url: r.url,
            url_hash: hash::from_db(r.url_hash),
            before: state(
                r.b_status,
                r.b_depth,
                r.b_indexability,
                r.b_in_sitemap,
                r.b_inlinks,
            ),
            after: state(
                r.a_status,
                r.a_depth,
                r.a_indexability,
                r.a_in_sitemap,
                r.a_inlinks,
            ),
        })
        .collect())
}

/// How many pages a crawl recorded; `0` once retention has dropped them.
pub async fn page_count(pool: &PgPool, crawl_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM pages WHERE crawl_id = $1")
        .bind(crawl_id)
        .fetch_one(pool)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_folder_needs_something_after_the_first_segment() {
        assert_eq!(top_folder("/blog/a"), "/blog");
        assert_eq!(top_folder("/blog/"), "/blog");
        assert_eq!(top_folder("/blog"), "");
        assert_eq!(top_folder("/"), "");
        assert_eq!(top_folder("//x"), "");
        assert_eq!(top_folder(""), "");
    }
}
