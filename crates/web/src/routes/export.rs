//! `/s/{site}/export.csv?filter=&q=`: the latest finished crawl's pages as CSV, with the
//! explorer's filter and search applied.
//!
//! The body streams: each step reads one keyset-paged batch of rows, encodes it with
//! `csv::Writer` and sends it as one chunk, so a 50,000-page export never holds more than a
//! batch in memory.

use std::io;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use codoseo_core::check::CheckId;
use codoseo_core::page::Indexability;
use codoseo_store::explorer::PageFilter;
use codoseo_store::export::ExportRow;
use futures_util::{Stream, stream};
use serde::Deserialize;
use sqlx::PgPool;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::auth::{CurrentUser, load_site};
use crate::error::AppError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/s/{site}/export.csv", get(export))
}

/// Rows per database read and per body chunk.
const BATCH: i64 = 1000;

const COLUMNS: [&str; 22] = [
    "Address",
    "Status",
    "Indexability",
    "Content type",
    "Title",
    "Title length",
    "Meta description",
    "Description length",
    "H1",
    "Canonical",
    "Meta robots",
    "X-Robots-Tag",
    "Word count",
    "Depth",
    "Inlinks",
    "Outlinks (internal)",
    "Outlinks (external)",
    "Response time (ms)",
    "Size (bytes)",
    "In sitemap",
    "Redirect target",
    "Issues",
];

#[derive(Deserialize)]
struct ExportQuery {
    filter: Option<String>,
    q: Option<String>,
}

async fn export(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
    Query(query): Query<ExportQuery>,
) -> Result<Response, AppError> {
    let site_id = Uuid::parse_str(&site).map_err(|_| AppError::NotFound)?;
    let site = load_site(&state, &user, site_id).await?;
    let crawl = codoseo_store::crawls::latest_done(&state.pool, site.id)
        .await?
        .ok_or_else(|| {
            AppError::BadRequest("There's no finished crawl to export yet.".to_owned())
        })?;
    let filter = PageFilter::parse(query.filter.as_deref().unwrap_or_default());
    let q = query
        .q
        .map(|q| q.trim().to_owned())
        .filter(|q| !q.is_empty());
    let day = crawl
        .finished_at
        .unwrap_or_else(OffsetDateTime::now_utc)
        .date();
    let disposition = format!(
        "attachment; filename=\"{}\"",
        file_name(&site.domain, filter, day)
    );

    let cursor = Cursor {
        pool: state.pool.clone(),
        crawl_id: crawl.id,
        filter,
        q,
        after: 0,
        header: true,
        done: false,
    };
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/csv; charset=utf-8"),
            ),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&disposition).map_err(AppError::internal)?,
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        Body::from_stream(chunks(cursor)),
    )
        .into_response())
}

/// `example.com-check-title_missing-2026-10-04.csv`: the domain, the filter's URL form with
/// `:` made file-safe, and the day the crawl finished.
fn file_name(domain: &str, filter: PageFilter, day: Date) -> String {
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '-'
                }
            })
            .collect()
    };
    format!(
        "{}-{}-{:04}-{:02}-{:02}.csv",
        safe(domain),
        safe(&filter.key()),
        day.year(),
        u8::from(day.month()),
        day.day()
    )
}

/// Where the export has got to.
struct Cursor {
    pool: PgPool,
    crawl_id: Uuid,
    filter: PageFilter,
    q: Option<String>,
    /// The last `pages.id` sent.
    after: i64,
    /// The header row still has to go out (with the first batch).
    header: bool,
    done: bool,
}

/// The CSV body, one chunk per batch. A database error partway is logged and ends the body
/// with an error: the status and headers have already gone out, so the client sees an
/// interrupted download rather than a file that looks complete.
fn chunks(cursor: Cursor) -> impl Stream<Item = Result<Bytes, io::Error>> + Send + 'static {
    stream::unfold(cursor, |mut c| async move {
        if c.done {
            return None;
        }
        let rows = match codoseo_store::export::batch(
            &c.pool,
            c.crawl_id,
            c.filter,
            c.q.as_deref(),
            c.after,
            BATCH,
        )
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!(error = %e, crawl_id = %c.crawl_id, "CSV export failed partway");
                c.done = true;
                return Some((Err(io::Error::other(e)), c));
            }
        };
        if rows.is_empty() && !c.header {
            return None;
        }
        c.done = (rows.len() as i64) < BATCH;
        if let Some(last) = rows.last() {
            c.after = last.id;
        }
        let chunk = encode(c.header, &rows).map(Bytes::from);
        c.header = false;
        if chunk.is_err() {
            c.done = true;
        }
        Some((chunk, c))
    })
}

/// One chunk of CSV: the header row first if asked, then a record per row.
fn encode(header: bool, rows: &[ExportRow]) -> Result<Vec<u8>, io::Error> {
    let mut w = csv::Writer::from_writer(Vec::with_capacity(rows.len() * 512 + 512));
    if header {
        w.write_record(COLUMNS).map_err(io::Error::other)?;
    }
    for r in rows {
        w.write_record(record(r)).map_err(io::Error::other)?;
    }
    w.into_inner().map_err(|e| e.into_error())
}

fn record(r: &ExportRow) -> [String; 22] {
    let opt = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
    let len = |s: &Option<String>| s.as_deref().map_or(0, |s| s.chars().count()).to_string();
    let issues: Vec<&str> = CheckId::ALL
        .iter()
        .filter(|c| r.issues.has_check(**c))
        .map(|c| c.slug())
        .collect();
    [
        text(Some(&r.url)),
        r.status.to_string(),
        indexability_label(r.indexability).to_owned(),
        text(r.content_type.as_deref()),
        text(r.title.as_deref()),
        len(&r.title),
        text(r.meta_description.as_deref()),
        len(&r.meta_description),
        text(r.h1.as_deref()),
        text(r.canonical.as_deref()),
        text(r.meta_robots.as_deref()),
        text(r.x_robots_tag.as_deref()),
        opt(r.word_count),
        opt(r.depth),
        r.inlinks.to_string(),
        r.outlinks_internal.to_string(),
        r.outlinks_external.to_string(),
        opt(r.response_ms),
        r.size_bytes.map(|n| n.to_string()).unwrap_or_default(),
        if r.in_sitemap { "Yes" } else { "No" }.to_owned(),
        text(r.redirect_target.as_deref()),
        issues.join("; "),
    ]
}

/// A crawled text cell. Text a spreadsheet would run as a formula (starting with `=`, `+`,
/// `-`, `@`, a tab or a CR) gets a leading `'`, since crawled pages are untrusted input.
fn text(value: Option<&str>) -> String {
    let value = value.unwrap_or_default();
    if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_owned()
    }
}

fn indexability_label(i: Indexability) -> &'static str {
    match i {
        Indexability::Indexable => "Indexable",
        Indexability::Noindex => "Noindex",
        Indexability::Canonicalised => "Canonicalised",
        Indexability::Redirected => "Redirected",
        Indexability::ClientError => "Client error",
        Indexability::ServerError => "Server error",
        Indexability::BlockedByRobots => "Blocked by robots.txt",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn file_names_are_safe() {
        let day = date!(2026 - 10 - 04);
        assert_eq!(
            file_name("example.com", PageFilter::All, day),
            "example.com-all-2026-10-04.csv"
        );
        assert_eq!(
            file_name("example.com", PageFilter::Check(CheckId::TitleMissing), day),
            "example.com-check-title_missing-2026-10-04.csv"
        );
        assert_eq!(
            file_name("we\"ird/host", PageFilter::Status4xx, day),
            "we-ird-host-s4-2026-10-04.csv"
        );
    }

    #[test]
    fn formula_cells_are_neutralised() {
        assert_eq!(text(Some("=1+1")), "'=1+1");
        assert_eq!(text(Some("-5")), "'-5");
        assert_eq!(text(Some("@cmd")), "'@cmd");
        assert_eq!(text(Some("Plain, \"quoted\"")), "Plain, \"quoted\"");
        assert_eq!(text(None), "");
    }
}
