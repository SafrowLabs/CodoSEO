//! Writing a finished crawl in one transaction: pages and inlinks via `COPY` (batched, so
//! memory stays flat on a large crawl), one `site_files` row, `changes`, the crawl's own
//! `done`/summary update, the 2-crawl retention cleanup, and default-rule alert jobs. A
//! failure partway rolls the whole transaction back, so a crawl never ends up half-written.

use std::collections::HashSet;

use codoseo_core::change::{Change, ChangeKind};
use codoseo_core::output::CrawlOutput;
use codoseo_core::page::{Indexability, PageRecord};
use codoseo_core::report::CrawlReport;
use serde::Serialize;
use serde_json::json;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::dbenum::enum_slug;
use crate::hash;

/// Rows per `COPY` chunk, per spec ("COPY in batches of 500").
const COPY_BATCH: usize = 500;

/// Writes a finished crawl. `worker_id` must match the row's current `worker_id` and the row
/// must still be `running`, or the whole transaction is rolled back instead of committing —
/// this is the guard against a worker whose heartbeat went stale (and was reclaimed by
/// `requeue_stale`, then claimed by another worker) finishing late and overwriting whatever the
/// new owner has written or is about to write.
pub async fn finalize(
    pool: &PgPool,
    crawl_id: Uuid,
    site_id: Uuid,
    worker_id: &str,
    out: &CrawlOutput,
    report: &CrawlReport,
    changes: &[Change],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    copy_pages(&mut tx, crawl_id, site_id, &out.pages).await?;
    copy_inlinks(&mut tx, crawl_id, out, report).await?;
    insert_site_files(&mut tx, crawl_id, site_id, out).await?;
    insert_changes(&mut tx, crawl_id, site_id, changes).await?;

    let summary = build_summary(out, report);
    let result = sqlx::query(
        "UPDATE crawls SET status = 'done', finished_at = now(), health_score = $2, \
         checks_passed = $3, checks_total = $4, summary = $5 \
         WHERE id = $1 AND status = 'running' AND worker_id = $6",
    )
    .bind(crawl_id)
    .bind(report.health_score as i16)
    .bind(report.checks_passed as i16)
    .bind(report.checks_total as i16)
    .bind(summary)
    .bind(worker_id)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        // Someone else now owns this crawl_id (requeued and reclaimed while we were still
        // running). Roll back rather than leave orphaned pages/inlinks/changes for their crawl.
        return Err(sqlx::Error::RowNotFound);
    }

    // Keep only the latest 2 `done` crawls' per-URL data (spec section 6).
    for table in ["pages", "inlinks", "site_files"] {
        let sql = format!(
            "DELETE FROM {table} WHERE crawl_id IN (\
               SELECT id FROM crawls WHERE site_id = $1 AND status = 'done' \
               ORDER BY finished_at DESC, id DESC OFFSET 2)"
        );
        sqlx::query(&sql).bind(site_id).execute(&mut *tx).await?;
    }

    enqueue_alert_jobs(&mut tx, crawl_id, changes).await?;

    tx.commit().await?;
    Ok(())
}

fn build_summary(out: &CrawlOutput, report: &CrawlReport) -> serde_json::Value {
    json!({
        "stop_reason": out.stop,
        "report_summary": report.summary,
        "counts": report.counts,
    })
}

async fn insert_site_files(
    tx: &mut Transaction<'_, Postgres>,
    crawl_id: Uuid,
    site_id: Uuid,
    out: &CrawlOutput,
) -> Result<(), sqlx::Error> {
    let (robots_status, robots_body, robots_hash) = match &out.robots {
        Some(r) => (
            Some(r.status as i16),
            Some(r.body.clone()),
            Some(hash::to_db(r.hash)),
        ),
        None => (None, None, None),
    };
    sqlx::query(
        "INSERT INTO site_files (crawl_id, site_id, robots_status, robots_body, robots_hash, \
         sitemap_url_count, sitemap_hash) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(crawl_id)
    .bind(site_id)
    .bind(robots_status)
    .bind(robots_body)
    .bind(robots_hash)
    .bind(out.sitemap.url_count as i32)
    .bind(hash::to_db(out.sitemap.hash))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_changes(
    tx: &mut Transaction<'_, Postgres>,
    crawl_id: Uuid,
    site_id: Uuid,
    changes: &[Change],
) -> Result<(), sqlx::Error> {
    for c in changes {
        sqlx::query(
            "INSERT INTO changes (crawl_id, site_id, kind, severity, url, before_value, after_value) \
             VALUES ($1, $2, $3::change_kind, $4::severity, $5, $6, $7)",
        )
        .bind(crawl_id)
        .bind(site_id)
        .bind(enum_slug(&c.kind))
        .bind(enum_slug(&c.severity))
        .bind(c.url.as_ref().map(|u| u.as_str()))
        .bind(&c.before)
        .bind(&c.after)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Default instant-alert shape from spec section 10 (key page noindex, 4xx/5xx spike, robots.txt
/// changed, sitemap shrank 10%+). Key-page filtering for `BecameNoindex` needs `sites.key_pages`,
/// which isn't available here — M7 narrows this once `alert_rules` exist; for now every
/// `BecameNoindex` change queues an alert job.
async fn enqueue_alert_jobs(
    tx: &mut Transaction<'_, Postgres>,
    crawl_id: Uuid,
    changes: &[Change],
) -> Result<(), sqlx::Error> {
    for (i, c) in changes.iter().enumerate() {
        let is_default_instant = matches!(
            c.kind,
            ChangeKind::BecameNoindex
                | ChangeKind::ErrorSpike
                | ChangeKind::RobotsTxtChanged
                | ChangeKind::SitemapShrank
        );
        if !is_default_instant {
            continue;
        }
        let payload = json!({ "crawl_id": crawl_id, "change_index": i });
        sqlx::query("INSERT INTO jobs (kind, payload) VALUES ('send_alert', $1)")
            .bind(payload)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

async fn copy_pages(
    tx: &mut Transaction<'_, Postgres>,
    crawl_id: Uuid,
    site_id: Uuid,
    pages: &[PageRecord],
) -> Result<(), sqlx::Error> {
    if pages.is_empty() {
        return Ok(());
    }
    let mut copy = tx
        .copy_in_raw(
            "COPY pages (crawl_id, site_id, url, url_hash, status, redirect_chain, response_ms, \
             size_bytes, content_type, depth, in_sitemap, indexability, title, \
             meta_description, meta_robots, x_robots_tag, canonical, hreflang, h1, h2, \
             word_count, content_hash, images_missing_alt, og, jsonld_status, mixed_content, \
             inlinks, outlinks_internal, outlinks_external, issues, key_hash) \
             FROM STDIN WITH (FORMAT text)",
        )
        .await?;

    let mut buf = String::new();
    for (i, p) in pages.iter().enumerate() {
        write_page_row(&mut buf, crawl_id, site_id, p);
        if (i + 1) % COPY_BATCH == 0 {
            copy.send(buf.as_bytes()).await?;
            buf.clear();
        }
    }
    if !buf.is_empty() {
        copy.send(buf.as_bytes()).await?;
    }
    copy.finish().await?;
    Ok(())
}

async fn copy_inlinks(
    tx: &mut Transaction<'_, Postgres>,
    crawl_id: Uuid,
    out: &CrawlOutput,
    report: &CrawlReport,
) -> Result<(), sqlx::Error> {
    let mut seen: HashSet<(u32, u32)> = HashSet::new();
    let mut rows: Vec<(u64, String, Option<String>, bool)> = Vec::new();

    for sample in &report.inlink_samples {
        let (Some(target_page), Some(source_page)) = (
            out.pages.get(sample.target as usize),
            out.pages.get(sample.source as usize),
        ) else {
            continue;
        };
        seen.insert((sample.target, sample.source));
        rows.push((
            target_page.url_hash,
            source_page.url.to_string(),
            Some(sample.anchor.clone()),
            sample.nofollow,
        ));
    }

    for edge in &out.links.edges {
        let Some(target_page) = out.pages.get(edge.to as usize) else {
            continue;
        };
        let broken_or_redirect = matches!(
            target_page.indexability,
            Indexability::ClientError | Indexability::ServerError | Indexability::Redirected
        );
        if !broken_or_redirect || seen.contains(&(edge.to, edge.from)) {
            continue;
        }
        let Some(source_page) = out.pages.get(edge.from as usize) else {
            continue;
        };
        seen.insert((edge.to, edge.from));
        rows.push((
            target_page.url_hash,
            source_page.url.to_string(),
            Some(out.links.anchor(edge).to_string()),
            edge.nofollow,
        ));
    }

    if rows.is_empty() {
        return Ok(());
    }

    let mut copy = tx
        .copy_in_raw(
            "COPY inlinks (crawl_id, target_url_hash, from_url, anchor_text, nofollow) \
             FROM STDIN WITH (FORMAT text)",
        )
        .await?;
    let mut buf = String::new();
    for (i, (target_hash, from_url, anchor, nofollow)) in rows.into_iter().enumerate() {
        buf.push_str(&crawl_id.to_string());
        buf.push('\t');
        buf.push_str(&hash::to_db(target_hash).to_string());
        buf.push('\t');
        buf.push_str(&escape(&from_url));
        buf.push('\t');
        buf.push_str(&field_opt(anchor.as_deref()));
        buf.push('\t');
        buf.push_str(if nofollow { "t" } else { "f" });
        buf.push('\n');
        if (i + 1) % COPY_BATCH == 0 {
            copy.send(buf.as_bytes()).await?;
            buf.clear();
        }
    }
    if !buf.is_empty() {
        copy.send(buf.as_bytes()).await?;
    }
    copy.finish().await?;
    Ok(())
}

fn write_page_row(buf: &mut String, crawl_id: Uuid, site_id: Uuid, p: &PageRecord) {
    let fields = &p.fields;
    let cols: [String; 31] = [
        crawl_id.to_string(),
        site_id.to_string(),
        escape(p.url.as_str()),
        hash::to_db(p.url_hash).to_string(),
        p.status.to_string(),
        escape_json(&p.redirect_chain),
        p.response_ms.to_string(),
        p.size_bytes.to_string(),
        field_opt(p.content_type.as_deref()),
        field_opt_num(p.depth),
        bool_field(p.in_sitemap),
        enum_slug(&p.indexability),
        field_opt(fields.title.as_deref()),
        field_opt(fields.meta_description.as_deref()),
        field_opt(fields.meta_robots.as_deref()),
        field_opt(fields.x_robots_tag.as_deref()),
        field_opt(fields.canonical.as_ref().map(|u| u.as_str())),
        escape_json(&fields.hreflang),
        escape_json(&fields.h1),
        escape_json(&fields.h2),
        fields.word_count.to_string(),
        hash::to_db(fields.content_hash).to_string(),
        fields.images_missing_alt.to_string(),
        escape_json(&fields.og),
        escape_json(&fields.jsonld),
        fields.mixed_content.to_string(),
        p.inlinks.to_string(),
        p.outlinks_internal.to_string(),
        p.outlinks_external.to_string(),
        hash::to_db(p.issues.0).to_string(),
        hash::to_db(p.key_hash).to_string(),
    ];
    buf.push_str(&cols.join("\t"));
    buf.push('\n');
}

/// `\N` is the `COPY` TEXT-format NULL marker; a real value is always backslash-escaped first,
/// so a literal two-character `\N` in data is never confused with it.
fn field_opt(v: Option<&str>) -> String {
    match v {
        Some(s) => escape(s),
        None => "\\N".to_string(),
    }
}

fn field_opt_num<T: ToString>(v: Option<T>) -> String {
    match v {
        Some(n) => n.to_string(),
        None => "\\N".to_string(),
    }
}

fn bool_field(b: bool) -> String {
    if b { "t".to_string() } else { "f".to_string() }
}

/// Escapes a value for `COPY ... WITH (FORMAT text)`: backslash, tab, newline and carriage
/// return need it; NUL is dropped outright since Postgres `text` data can never contain one
/// (and nothing upstream filters it — a NUL can reach here from malformed/binary content
/// mislabeled as `text/html`).
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => {}
            _ => out.push(c),
        }
    }
    out
}

fn escape_json<T: Serialize>(v: &T) -> String {
    escape(&serde_json::to_string(v).expect("value always serializes"))
}
