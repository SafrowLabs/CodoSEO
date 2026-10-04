//! Pages for the CSV export, read in keyset-paged batches (`id > after ORDER BY id`) so a
//! large crawl streams out with flat memory. Takes the explorer's filter and search.

use codoseo_core::check::IssueBits;
use codoseo_core::page::Indexability;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::explorer::PageFilter;
use crate::hash;
use crate::search::contains_pattern;

/// One exported page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRow {
    /// The keyset cursor: pass the last row's `id` as the next batch's `after`.
    pub id: i64,
    pub url: String,
    pub status: u16,
    pub indexability: Indexability,
    pub content_type: Option<String>,
    pub title: Option<String>,
    pub meta_description: Option<String>,
    /// The first H1.
    pub h1: Option<String>,
    pub canonical: Option<String>,
    pub meta_robots: Option<String>,
    pub x_robots_tag: Option<String>,
    pub word_count: Option<u32>,
    pub depth: Option<u32>,
    pub inlinks: u32,
    pub outlinks_internal: u32,
    pub outlinks_external: u32,
    pub response_ms: Option<u32>,
    pub size_bytes: Option<u64>,
    pub in_sitemap: bool,
    /// The next hop of a multi-hop redirect chain. `pages` stores each hop's own URL but not
    /// where the last one lands, so a single-hop redirect has no target here.
    pub redirect_target: Option<String>,
    pub issues: IssueBits,
}

#[derive(FromRow)]
struct Row {
    id: i64,
    url: String,
    status: i16,
    indexability: String,
    content_type: Option<String>,
    title: Option<String>,
    meta_description: Option<String>,
    h1: Option<String>,
    canonical: Option<String>,
    meta_robots: Option<String>,
    x_robots_tag: Option<String>,
    word_count: Option<i32>,
    depth: Option<i32>,
    inlinks: i32,
    outlinks_internal: i32,
    outlinks_external: i32,
    response_ms: Option<i32>,
    size_bytes: Option<i64>,
    in_sitemap: bool,
    redirect_target: Option<String>,
    issues: i64,
}

impl TryFrom<Row> for ExportRow {
    type Error = sqlx::Error;

    fn try_from(r: Row) -> Result<ExportRow, sqlx::Error> {
        let indexability = serde_json::from_value(serde_json::Value::String(r.indexability))
            .map_err(|e| sqlx::Error::ColumnDecode {
                index: "indexability".to_owned(),
                source: Box::new(e),
            })?;
        let count = |n: i32| u32::try_from(n).unwrap_or(0);
        Ok(ExportRow {
            id: r.id,
            url: r.url,
            status: u16::try_from(r.status).unwrap_or(0),
            indexability,
            content_type: r.content_type,
            title: r.title,
            meta_description: r.meta_description,
            h1: r.h1,
            canonical: r.canonical,
            meta_robots: r.meta_robots,
            x_robots_tag: r.x_robots_tag,
            word_count: r.word_count.map(count),
            depth: r.depth.map(count),
            inlinks: count(r.inlinks),
            outlinks_internal: count(r.outlinks_internal),
            outlinks_external: count(r.outlinks_external),
            response_ms: r.response_ms.map(count),
            size_bytes: r.size_bytes.map(|n| u64::try_from(n).unwrap_or(0)),
            in_sitemap: r.in_sitemap,
            redirect_target: r.redirect_target,
            issues: IssueBits(hash::from_db(r.issues)),
        })
    }
}

/// Up to `limit` pages of the crawl with `id > after`, in `id` order, matching `filter` and,
/// when given, `q` (case-insensitive, anywhere in the URL or title, wildcards literal). An
/// empty batch means the export is done.
pub async fn batch(
    pool: &PgPool,
    crawl_id: Uuid,
    filter: PageFilter,
    q: Option<&str>,
    after: i64,
    limit: i64,
) -> Result<Vec<ExportRow>, sqlx::Error> {
    let sql = format!(
        "SELECT p.id, p.url, p.status, p.indexability::text AS indexability, p.content_type, \
                p.title, p.meta_description, p.h1->>0 AS h1, p.canonical, p.meta_robots, \
                p.x_robots_tag, p.word_count, p.depth, p.inlinks, p.outlinks_internal, \
                p.outlinks_external, p.response_ms, p.size_bytes, p.in_sitemap, \
                p.redirect_chain->1->>1 AS redirect_target, p.issues \
         FROM pages p \
         WHERE p.crawl_id = $1 AND p.id > $2 AND ({}) \
           AND ($3::text IS NULL OR p.url ILIKE $3 OR p.title ILIKE $3) \
         ORDER BY p.id LIMIT $4",
        filter.predicate()
    );
    let rows: Vec<Row> = sqlx::query_as(&sql)
        .bind(crawl_id)
        .bind(after)
        .bind(q.map(contains_pattern))
        .bind(limit)
        .fetch_all(pool)
        .await?;
    rows.into_iter().map(ExportRow::try_from).collect()
}
