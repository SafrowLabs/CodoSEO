//! A crawl's sitemaps as a tree: what `finalize` keeps in `site_files.sitemap_tree` (a
//! [`SitemapTree`]), for the site graph.

use codoseo_core::sitemap::SitemapTree;
use sqlx::PgPool;
use sqlx::types::Json;
use uuid::Uuid;

/// The sitemap tree of `crawl_id`; `None` when the crawl has no `site_files` row (not finished,
/// or past retention) or finished before the tree was kept.
pub async fn load_tree(pool: &PgPool, crawl_id: Uuid) -> Result<Option<SitemapTree>, sqlx::Error> {
    let tree: Option<Option<Json<SitemapTree>>> =
        sqlx::query_scalar("SELECT sitemap_tree FROM site_files WHERE crawl_id = $1 LIMIT 1")
            .bind(crawl_id)
            .fetch_optional(pool)
            .await?;
    Ok(tree.flatten().map(|t| t.0))
}
