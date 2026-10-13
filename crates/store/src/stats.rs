//! Running crawl totals for the public counter: pages crawled, crawls and distinct websites, in
//! the one `crawl_stats` row. `finalize` bumps it as each crawl is marked done, so reading the
//! totals is a primary-key lookup, never a count over pages or crawls.

use sqlx::{FromRow, PgPool, Postgres, Transaction};

/// Everything crawled so far, monitored crawls and no-signup audits alike.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, FromRow)]
pub struct CrawlTotals {
    pub pages_crawled: i64,
    pub crawls: i64,
    /// Distinct domains. Stored for later; not shown publicly yet.
    pub websites: i64,
}

/// Adds a finished crawl of `domain` with `pages` pages. The domain is counted once ever, by its
/// SHA-256. Called last in `finalize`'s transaction, so the row lock is held only until commit and
/// a rolled-back crawl counts for nothing.
pub(crate) async fn record_crawl(
    tx: &mut Transaction<'_, Postgres>,
    domain: &str,
    pages: u32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "WITH new_site AS ( \
           INSERT INTO crawled_websites (domain_hash) VALUES (sha256(convert_to($1, 'UTF8'))) \
           ON CONFLICT DO NOTHING RETURNING 1) \
         UPDATE crawl_stats SET pages_crawled = pages_crawled + $2, crawls = crawls + 1, \
           websites = websites + (SELECT count(*) FROM new_site), updated_at = now()",
    )
    .bind(domain)
    .bind(i64::from(pages))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The totals: one row by primary key.
pub async fn totals(pool: &PgPool) -> Result<CrawlTotals, sqlx::Error> {
    let row = sqlx::query_as("SELECT pages_crawled, crawls, websites FROM crawl_stats")
        .fetch_optional(pool)
        .await?;
    Ok(row.unwrap_or_default())
}
