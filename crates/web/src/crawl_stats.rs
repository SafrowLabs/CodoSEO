//! The crawl totals behind the landing page's live "pages crawled" counter, cached in memory so
//! landing traffic reads the database at most once every [`TTL`] per web process.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use codoseo_store::stats::{self, CrawlTotals};
use sqlx::PgPool;

/// How long a read of the totals is served before the next one goes to the database.
pub const TTL: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct StatsCache {
    last: Mutex<Option<(Instant, CrawlTotals)>>,
}

impl StatsCache {
    /// The totals, from memory when the last read is under [`TTL`] old. A failed read serves the
    /// last totals it has (a counter is never worth an error page), or `None` before any.
    pub async fn totals(&self, pool: &PgPool) -> Option<CrawlTotals> {
        if let Some((at, totals)) = *self.lock()
            && at.elapsed() < TTL
        {
            return Some(totals);
        }
        match stats::totals(pool).await {
            Ok(totals) => {
                *self.lock() = Some((Instant::now(), totals));
                Some(totals)
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not read the crawl totals");
                self.cached()
            }
        }
    }

    /// The last totals read, however old, without touching the database.
    pub fn cached(&self) -> Option<CrawlTotals> {
        self.lock().map(|(_, totals)| totals)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(Instant, CrawlTotals)>> {
        self.last.lock().unwrap_or_else(|e| e.into_inner())
    }
}
