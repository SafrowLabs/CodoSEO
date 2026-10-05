//! Read queries behind the site audit and changes screens: a crawl's changes and their counts,
//! the health-score history, and the response-time histogram.

use codoseo_core::change::ChangeKind;
use codoseo_core::check::Severity;
use serde::de::DeserializeOwned;
use sqlx::PgPool;
use uuid::Uuid;

use crate::dbenum::enum_slug;

/// One row of the `changes` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRow {
    pub kind: ChangeKind,
    pub severity: Severity,
    /// The page the change is about; `None` for site-wide changes (robots.txt, sitemap, spike).
    pub url: Option<String>,
    pub before: String,
    pub after: String,
}

/// Change counts for one crawl, per kind and severity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeCounts {
    /// `(kind, severity, n)` for every pair with at least one change.
    pub rows: Vec<(ChangeKind, Severity, i64)>,
}

impl ChangeCounts {
    pub fn total(&self) -> i64 {
        self.rows.iter().map(|r| r.2).sum()
    }

    /// Changes of this kind, at any severity.
    pub fn kind(&self, kind: ChangeKind) -> i64 {
        self.rows.iter().filter(|r| r.0 == kind).map(|r| r.2).sum()
    }

    /// Changes at this severity, of any kind.
    pub fn severity(&self, severity: Severity) -> i64 {
        self.rows
            .iter()
            .filter(|r| r.1 == severity)
            .map(|r| r.2)
            .sum()
    }
}

/// A finished crawl's health score, for the history chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthPoint {
    /// The crawl number, counted the same way as `crawls::Crawl::number`.
    pub number: i64,
    pub score: i16,
}

/// Pages per response-time band. Pages with no response (status 0) aren't counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResponseBuckets {
    /// Under 200 ms.
    pub fast: i64,
    /// 200 ms up to 500 ms.
    pub ok: i64,
    /// 500 ms up to 1 s.
    pub slow: i64,
    /// 1 s or more.
    pub very_slow: i64,
}

impl ResponseBuckets {
    pub fn total(&self) -> i64 {
        self.fast + self.ok + self.slow + self.very_slow
    }
}

/// Parses an enum label read back as text (`kind::text`) into its Rust enum.
pub(crate) fn from_slug<T: DeserializeOwned>(slug: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(slug.to_owned())).ok()
}

/// A crawl's changes, most severe first, in the order the diff produced them within each
/// severity. `severity` narrows to one severity; at most `limit` rows come back.
pub async fn changes_for_crawl(
    pool: &PgPool,
    crawl_id: Uuid,
    severity: Option<Severity>,
    limit: i64,
) -> Result<Vec<ChangeRow>, sqlx::Error> {
    let rows: Vec<(String, String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT kind::text, severity::text, url, before_value, after_value FROM changes \
         WHERE crawl_id = $1 AND ($2::text IS NULL OR severity::text = $2) \
         ORDER BY severity, id LIMIT $3",
    )
    .bind(crawl_id)
    .bind(severity.map(|s| enum_slug(&s)))
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(kind, severity, url, before, after)| {
            Some(ChangeRow {
                kind: from_slug(&kind)?,
                severity: from_slug(&severity)?,
                url,
                before,
                after,
            })
        })
        .collect())
}

/// How many changes a crawl has, per kind and severity.
pub async fn change_kind_counts(
    pool: &PgPool,
    crawl_id: Uuid,
) -> Result<ChangeCounts, sqlx::Error> {
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT kind::text, severity::text, count(*) FROM changes WHERE crawl_id = $1 \
         GROUP BY kind, severity ORDER BY kind, severity",
    )
    .bind(crawl_id)
    .fetch_all(pool)
    .await?;
    Ok(ChangeCounts {
        rows: rows
            .into_iter()
            .filter_map(|(kind, severity, n)| Some((from_slug(&kind)?, from_slug(&severity)?, n)))
            .collect(),
    })
}

/// Health scores of the site's last `limit` finished crawls, oldest first.
pub async fn health_history(
    pool: &PgPool,
    site_id: Uuid,
    limit: i64,
) -> Result<Vec<HealthPoint>, sqlx::Error> {
    let rows: Vec<(i64, i16)> = sqlx::query_as(
        "SELECT number, health_score FROM ( \
           SELECT row_number() OVER (ORDER BY created_at, id) AS number, status, health_score, \
                  finished_at \
           FROM crawls WHERE site_id = $1) c \
         WHERE status = 'done' AND health_score IS NOT NULL \
         ORDER BY finished_at DESC, number DESC LIMIT $2",
    )
    .bind(site_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .rev()
        .map(|(number, score)| HealthPoint { number, score })
        .collect())
}

/// A crawl's pages bucketed by response time, in one pass over `pages`.
pub async fn response_time_buckets(
    pool: &PgPool,
    crawl_id: Uuid,
) -> Result<ResponseBuckets, sqlx::Error> {
    let (fast, ok, slow, very_slow): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE response_ms < 200), \
                count(*) FILTER (WHERE response_ms >= 200 AND response_ms < 500), \
                count(*) FILTER (WHERE response_ms >= 500 AND response_ms < 1000), \
                count(*) FILTER (WHERE response_ms >= 1000) \
         FROM pages WHERE crawl_id = $1 AND status <> 0",
    )
    .bind(crawl_id)
    .fetch_one(pool)
    .await?;
    Ok(ResponseBuckets {
        fast,
        ok,
        slow,
        very_slow,
    })
}

/// The crawl's best pages for RankOrg to look at: indexable 200s with the most inlinks, ties in
/// crawl order.
pub async fn top_pages_by_inlinks(
    pool: &PgPool,
    crawl_id: Uuid,
    limit: i64,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT url FROM pages \
         WHERE crawl_id = $1 AND status = 200 AND indexability = 'indexable' \
         ORDER BY inlinks DESC, id LIMIT $2",
    )
    .bind(crawl_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_parse_to_enums() {
        assert_eq!(
            from_slug::<ChangeKind>("robots_txt_changed"),
            Some(ChangeKind::RobotsTxtChanged)
        );
        assert_eq!(from_slug::<Severity>("warning"), Some(Severity::Warning));
        assert_eq!(from_slug::<Severity>("nope"), None);
    }

    #[test]
    fn counts_sum_by_kind_and_severity() {
        let c = ChangeCounts {
            rows: vec![
                (ChangeKind::StatusChanged, Severity::Warning, 2),
                (ChangeKind::StatusChanged, Severity::Notice, 3),
                (ChangeKind::NewUrl, Severity::Notice, 4),
            ],
        };
        assert_eq!(c.total(), 9);
        assert_eq!(c.kind(ChangeKind::StatusChanged), 5);
        assert_eq!(c.kind(ChangeKind::RemovedUrl), 0);
        assert_eq!(c.severity(Severity::Notice), 7);
        assert_eq!(c.severity(Severity::Critical), 0);
    }
}
