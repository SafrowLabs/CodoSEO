//! The crawl queue: `crawls` is both the work queue and the crawl history. Workers claim by
//! effective priority (one level per hour waited) with `SKIP LOCKED`; a partial unique index
//! on `crawls(domain) WHERE status = 'running'` keeps one active crawl per domain.

use codoseo_core::check::IssueBits;
use codoseo_core::crawl::{RobotsFile, SitemapSummary};
use codoseo_core::output::{Progress, StopReason};
use codoseo_core::page::{Indexability, JsonLdStatus, OgTags, PageFields, PageRecord};
use codoseo_core::snapshot::Snapshot;
use serde::Deserialize;
use sqlx::{FromRow, PgPool, Row};
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

/// Why a crawl was queued. Mirrors the `crawl_trigger` Postgres enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "crawl_trigger", rename_all = "snake_case")]
pub enum CrawlTrigger {
    Schedule,
    Manual,
    First,
    Quick,
}

/// A crawl row claimed by a worker, with the site fields the worker needs to build a
/// `CrawlConfig` and run the crawl.
#[derive(Debug, FromRow)]
pub struct ClaimedCrawl {
    pub id: Uuid,
    pub site_id: Uuid,
    pub domain: String,
    pub trigger: CrawlTrigger,
    pub priority: i16,
    pub source: Option<String>,
    pub attempt: i16,
    pub start_url: String,
    pub crawl_settings: serde_json::Value,
}

pub struct CrawlQueue {
    pool: PgPool,
}

impl CrawlQueue {
    pub fn new(pool: PgPool) -> CrawlQueue {
        CrawlQueue { pool }
    }

    /// Queues a new crawl for `site_id`. `domain` is denormalised from `sites.domain` onto the
    /// row so the one-running-per-domain index doesn't need a join.
    pub async fn enqueue(
        &self,
        site_id: Uuid,
        domain: &str,
        trigger: CrawlTrigger,
        priority: i16,
        source: Option<&str>,
        requester_ip_hash: Option<&[u8]>,
    ) -> Result<Uuid, sqlx::Error> {
        sqlx::query_scalar(
            "INSERT INTO crawls (site_id, domain, trigger, priority, source, requester_ip_hash) \
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(site_id)
        .bind(domain)
        .bind(trigger)
        .bind(priority)
        .bind(source)
        .bind(requester_ip_hash)
        .fetch_one(&self.pool)
        .await
    }

    /// Claims the next eligible queued crawl, aging its effective priority by one level per
    /// hour waited. A `NOT EXISTS` guard against another `running` row on the same domain keeps
    /// this from even attempting a row the partial unique index would reject; `FOR UPDATE SKIP
    /// LOCKED` is what makes concurrent claimers safe.
    pub async fn claim(&self, worker_id: &str) -> Result<Option<ClaimedCrawl>, sqlx::Error> {
        sqlx::query_as(
            "UPDATE crawls c \
             SET status = 'running', started_at = now(), heartbeat_at = now(), worker_id = $1 \
             FROM sites s \
             WHERE c.site_id = s.id \
               AND c.id = ( \
                 SELECT id FROM crawls \
                 WHERE status = 'queued' \
                   AND queued_at <= now() \
                   AND NOT EXISTS ( \
                     SELECT 1 FROM crawls c2 \
                     WHERE c2.domain = crawls.domain AND c2.status = 'running' \
                   ) \
                 ORDER BY (priority - EXTRACT(EPOCH FROM (now() - queued_at)) / 3600.0), queued_at \
                 FOR UPDATE SKIP LOCKED \
                 LIMIT 1 \
               ) \
             RETURNING c.id, c.site_id, c.domain, c.trigger, c.priority, c.source, c.attempt, \
                       s.start_url, s.crawl_settings",
        )
        .bind(worker_id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Records progress on a still-`running` crawl. A crawl that has already been requeued by
    /// `requeue_stale` (e.g. after a dead worker) silently ignores a late heartbeat from the
    /// old worker, since the `WHERE status = 'running'` guard then matches nothing.
    pub async fn heartbeat(&self, id: Uuid, progress: &Progress) -> Result<(), sqlx::Error> {
        let progress = serde_json::to_value(progress).expect("Progress always serializes");
        sqlx::query(
            "UPDATE crawls SET heartbeat_at = now(), progress = $2 WHERE id = $1 AND status = 'running'",
        )
        .bind(id)
        .bind(progress)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Moves `running` crawls whose heartbeat is older than `older_than` back to `queued` (the
    /// dead-worker path), and returns how many rows moved.
    pub async fn requeue_stale(&self, older_than: std::time::Duration) -> Result<u64, sqlx::Error> {
        let age = time::Duration::try_from(older_than).unwrap_or(time::Duration::ZERO);
        let threshold = OffsetDateTime::now_utc() - age;
        let result = sqlx::query(
            "UPDATE crawls SET status = 'queued', worker_id = NULL, heartbeat_at = NULL \
             WHERE status = 'running' AND heartbeat_at < $1",
        )
        .bind(threshold)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// First failure (`attempt = 0`): requeue once, gated 15 minutes out via `queued_at` (which
    /// `claim`'s `queued_at <= now()` filter already honours). Second failure: fail for good.
    /// One statement, so there's no read-then-write race against a concurrent call.
    pub async fn finish_failed(&self, id: Uuid, reason: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE crawls SET \
               status = CASE WHEN attempt = 0 THEN 'queued'::crawl_status ELSE 'failed'::crawl_status END, \
               attempt = CASE WHEN attempt = 0 THEN 1 ELSE attempt END, \
               queued_at = CASE WHEN attempt = 0 THEN now() + interval '15 minutes' ELSE queued_at END, \
               worker_id = CASE WHEN attempt = 0 THEN NULL ELSE worker_id END, \
               heartbeat_at = CASE WHEN attempt = 0 THEN NULL ELSE heartbeat_at END, \
               failure_reason = $2 \
             WHERE id = $1",
        )
        .bind(id)
        .bind(reason)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Rebuilds a `Snapshot` from the most recent `done` crawl of `site_id`, for the diff step
    /// of the next crawl. Best-effort: a few `PageRecord`/`SitemapSummary` fields aren't stored
    /// (see the field-level comments below) and come back as sensible defaults rather than the
    /// exact values from the original crawl.
    pub async fn previous_snapshot(&self, site_id: Uuid) -> Result<Option<Snapshot>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, finished_at, summary FROM crawls \
             WHERE site_id = $1 AND status = 'done' ORDER BY finished_at DESC LIMIT 1",
        )
        .bind(site_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let crawl_id: Uuid = row.get("id");
        let summary: Option<serde_json::Value> = row.get("summary");

        #[derive(Deserialize)]
        struct SummaryStop {
            stop_reason: StopReason,
        }
        let stop = summary
            .as_ref()
            .and_then(|v| serde_json::from_value::<SummaryStop>(v.clone()).ok())
            .map(|s| s.stop_reason)
            .unwrap_or(StopReason::Completed);

        let page_rows = sqlx::query(
            "SELECT id, crawl_id, site_id, url, url_hash, status, redirect_chain, response_ms, \
                    size_bytes, content_type, depth, in_sitemap, indexability::text AS indexability, \
                    title, meta_description, meta_robots, x_robots_tag, canonical, hreflang, h1, h2, \
                    word_count, content_hash, images_missing_alt, og, jsonld_status, mixed_content, \
                    inlinks, outlinks_internal, outlinks_external, issues, key_hash \
             FROM pages WHERE crawl_id = $1",
        )
        .bind(crawl_id)
        .fetch_all(&self.pool)
        .await?;
        let pages = page_rows.into_iter().map(page_record_from_row).collect();

        // origin: approximated from `sites.start_url` — the exact settled address from the
        // original crawl's preflight isn't persisted as its own column.
        let start_url: String = sqlx::query_scalar("SELECT start_url FROM sites WHERE id = $1")
            .bind(site_id)
            .fetch_one(&self.pool)
            .await?;
        let origin = Url::parse(&start_url).unwrap_or_else(|_| {
            Url::parse("https://invalid.example/").expect("fallback URL always parses")
        });

        let site_file = sqlx::query("SELECT * FROM site_files WHERE crawl_id = $1 LIMIT 1")
            .bind(crawl_id)
            .fetch_optional(&self.pool)
            .await?;
        let robots = site_file.as_ref().and_then(robots_file_from_row);
        let sitemap = site_file
            .as_ref()
            .map(sitemap_summary_from_row)
            .unwrap_or_default();

        Ok(Some(Snapshot {
            origin,
            stop,
            pages,
            robots,
            sitemap,
        }))
    }
}

fn page_record_from_row(row: sqlx::postgres::PgRow) -> PageRecord {
    let url_str: String = row.get("url");
    let url = Url::parse(&url_str).unwrap_or_else(|_| {
        Url::parse("https://invalid.example/").expect("fallback URL always parses")
    });
    let canonical: Option<String> = row.get("canonical");
    let canonical = canonical.and_then(|c| Url::parse(&c).ok());

    let redirect_chain: serde_json::Value = row.get("redirect_chain");
    let redirect_chain: Vec<(u16, Url)> =
        serde_json::from_value(redirect_chain).unwrap_or_default();
    let redirect_target = redirect_chain.last().map(|(_, url)| url.clone());

    let hreflang: serde_json::Value = row.get("hreflang");
    let hreflang: Vec<(String, Url)> = serde_json::from_value(hreflang).unwrap_or_default();
    let h1: serde_json::Value = row.get("h1");
    let h1: Vec<String> = serde_json::from_value(h1).unwrap_or_default();
    let h2: serde_json::Value = row.get("h2");
    let h2: Vec<String> = serde_json::from_value(h2).unwrap_or_default();
    let og: Option<serde_json::Value> = row.get("og");
    let og: OgTags = og
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let jsonld_status: Option<String> = row.get("jsonld_status");
    let jsonld: JsonLdStatus = jsonld_status
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let status: i16 = row.get("status");
    let issues: i64 = row.get("issues");
    let key_hash: Option<i64> = row.get("key_hash");
    let url_hash: i64 = row.get("url_hash");
    let content_hash: Option<i64> = row.get("content_hash");
    let indexability: String = row.get("indexability");
    let indexability = indexability_from_text(&indexability);

    PageRecord {
        url,
        url_hash: crate::hash::from_db(url_hash),
        status: status as u16,
        redirect_chain,
        response_ms: row.get::<Option<i32>, _>("response_ms").unwrap_or(0) as u32,
        size_bytes: row.get::<Option<i64>, _>("size_bytes").unwrap_or(0) as u64,
        content_type: row.get("content_type"),
        depth: row.get::<Option<i32>, _>("depth").map(|d| d as u16),
        in_sitemap: row.get("in_sitemap"),
        indexability,
        fields: PageFields {
            title: row.get("title"),
            title_count: 0,
            meta_description: row.get("meta_description"),
            meta_robots: row.get("meta_robots"),
            x_robots_tag: row.get("x_robots_tag"),
            canonical,
            hreflang,
            h1,
            h2,
            word_count: row.get::<Option<i32>, _>("word_count").unwrap_or(0) as u32,
            content_hash: content_hash.map(crate::hash::from_db).unwrap_or(0),
            images_missing_alt: row.get::<Option<i32>, _>("images_missing_alt").unwrap_or(0) as u32,
            og,
            jsonld,
            mixed_content: row.get::<Option<i32>, _>("mixed_content").unwrap_or(0) as u32,
        },
        inlinks: row.get::<i32, _>("inlinks") as u32,
        outlinks_internal: row.get::<i32, _>("outlinks_internal") as u32,
        outlinks_external: row.get::<i32, _>("outlinks_external") as u32,
        issues: IssueBits(crate::hash::from_db(issues)),
        key_hash: key_hash.map(crate::hash::from_db).unwrap_or(0),
        redirect_target,
        outlinks_nofollow: 0,
        error: None,
    }
}

/// `pages.indexability` is a closed Postgres enum written by `codoseo-checks`, so every value
/// the database can hold is listed here; the fallback only matters if a future migration adds
/// a variant this build doesn't know about yet.
fn indexability_from_text(text: &str) -> Indexability {
    match text {
        "indexable" => Indexability::Indexable,
        "noindex" => Indexability::Noindex,
        "canonicalised" => Indexability::Canonicalised,
        "redirected" => Indexability::Redirected,
        "client_error" => Indexability::ClientError,
        "blocked_by_robots" => Indexability::BlockedByRobots,
        _ => Indexability::ServerError,
    }
}

fn robots_file_from_row(row: &sqlx::postgres::PgRow) -> Option<RobotsFile> {
    let status: Option<i16> = row.get("robots_status");
    let body: Option<String> = row.get("robots_body");
    let hash: Option<i64> = row.get("robots_hash");
    match (status, body, hash) {
        (Some(status), Some(body), Some(hash)) => Some(RobotsFile {
            status: status as u16,
            body,
            hash: crate::hash::from_db(hash),
        }),
        _ => None,
    }
}

fn sitemap_summary_from_row(row: &sqlx::postgres::PgRow) -> SitemapSummary {
    let url_count: Option<i32> = row.get("sitemap_url_count");
    let hash: Option<i64> = row.get("sitemap_hash");
    SitemapSummary {
        files: Vec::new(),
        url_count: url_count.unwrap_or(0) as u32,
        hash: hash.map(crate::hash::from_db).unwrap_or(0),
        truncated: false,
        failed_files: 0,
        complete: true,
    }
}
