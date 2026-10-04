//! The ⌘K palette's page search over a crawl's `pages`, and the `LIKE` escaping that every
//! free-text page match (the search, the CSV export's `q`) shares.

use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::hash;

/// Escapes `%`, `_` and `\` so `s` matches literally in a `LIKE`/`ILIKE` pattern that uses
/// Postgres's default escape character (`\`).
pub fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A pattern matching `s` anywhere: `%s%`, with `s` escaped.
pub fn contains_pattern(s: &str) -> String {
    format!("%{}%", like_escape(s))
}

/// A pattern matching values that start with `s`: `s%`, with `s` escaped.
pub fn prefix_pattern(s: &str) -> String {
    format!("{}%", like_escape(s))
}

/// One page the palette offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub url: String,
    pub url_hash: u64,
    /// The URL without its scheme and host: path plus query (empty for a bare origin).
    pub path: String,
    pub title: Option<String>,
}

#[derive(FromRow)]
struct HitRow {
    url: String,
    url_hash: i64,
    path: String,
    title: Option<String>,
}

/// Pages of the crawl whose URL or title contains `q` (case-insensitive), at most `limit`.
/// Prefix matches come first (on the path, with or without its leading slash; on the URL with
/// or without its scheme, so typing the host works; or on the title), then substring matches,
/// and shorter paths first within each tier. An empty `q` finds nothing.
pub async fn pages(
    pool: &PgPool,
    crawl_id: Uuid,
    q: &str,
    limit: i64,
) -> Result<Vec<SearchHit>, sqlx::Error> {
    let q = q.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<HitRow> = sqlx::query_as(
        "SELECT url, url_hash, path, title FROM ( \
           SELECT p.url, p.url_hash, p.title, \
                  regexp_replace(p.url, '^[A-Za-z][A-Za-z0-9+.-]*://[^/?#]*', '') AS path, \
                  regexp_replace(p.url, '^[A-Za-z][A-Za-z0-9+.-]*://', '') AS bare \
           FROM pages p \
           WHERE p.crawl_id = $1 AND (p.url ILIKE $2 OR p.title ILIKE $2) \
         ) m \
         ORDER BY (path ILIKE $3 OR ltrim(path, '/') ILIKE $3 OR bare ILIKE $3 \
                   OR url ILIKE $3 OR title ILIKE $3) DESC, \
                  length(path), path, url_hash \
         LIMIT $4",
    )
    .bind(crawl_id)
    .bind(contains_pattern(q))
    .bind(prefix_pattern(q))
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| SearchHit {
            url: r.url,
            url_hash: hash::from_db(r.url_hash),
            path: r.path,
            title: r.title,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_like_wildcards_and_the_escape_character() {
        assert_eq!(like_escape("plain text"), "plain text");
        assert_eq!(like_escape("50%_off\\now"), "50\\%\\_off\\\\now");
        assert_eq!(contains_pattern("a%b"), "%a\\%b%");
        assert_eq!(prefix_pattern("_x"), "\\_x%");
    }
}
