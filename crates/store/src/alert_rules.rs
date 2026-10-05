//! Alert rules: for each site, which change kinds go to which channel the moment a crawl finds
//! them (mode `instant`). A kind with no instant rule for a channel is left for that week's
//! digest. The settings screen edits these as a grid of change kinds by channels.

use std::collections::HashSet;

use codoseo_core::change::ChangeKind;
use codoseo_core::check::Severity;
use sqlx::PgPool;
use uuid::Uuid;

use crate::dbenum::enum_slug;

/// What is instant by default: a key page going noindex, a 4xx/5xx spike, robots.txt changing,
/// the sitemap losing 10% or more, and the site moving.
pub const DEFAULT_INSTANT: [ChangeKind; 5] = [
    ChangeKind::BecameNoindex,
    ChangeKind::ErrorSpike,
    ChangeKind::RobotsTxtChanged,
    ChangeKind::SitemapShrank,
    ChangeKind::SiteMoved,
];

/// Every change kind, in the order the settings grid lists them.
pub const ALL_KINDS: [ChangeKind; 12] = [
    ChangeKind::BecameNoindex,
    ChangeKind::ErrorSpike,
    ChangeKind::RobotsTxtChanged,
    ChangeKind::SitemapShrank,
    ChangeKind::SiteMoved,
    ChangeKind::StatusChanged,
    ChangeKind::RemovedUrl,
    ChangeKind::NewUrl,
    ChangeKind::TitleChanged,
    ChangeKind::TitleRemoved,
    ChangeKind::CanonicalChanged,
    ChangeKind::RedirectChainGrew,
];

/// One rule as the grid shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleCell {
    pub site_id: Uuid,
    pub kind: ChangeKind,
    pub channel_id: Uuid,
    pub instant: bool,
}

/// The default instant rules for `channel_id` on `site_id`. Existing rules are left alone, so
/// a default the user switched off stays off.
pub async fn create_defaults(
    pool: &PgPool,
    site_id: Uuid,
    channel_id: Uuid,
) -> Result<(), sqlx::Error> {
    let kinds: Vec<String> = DEFAULT_INSTANT.iter().map(enum_slug).collect();
    sqlx::query(
        "INSERT INTO alert_rules (site_id, change_kind, channel_id, mode) \
         SELECT $1, k::change_kind, $2, 'instant' FROM unnest($3::text[]) AS k \
         ON CONFLICT DO NOTHING",
    )
    .bind(site_id)
    .bind(channel_id)
    .bind(kinds)
    .execute(pool)
    .await?;
    Ok(())
}

/// The default instant rules for `channel_id` on every site of the account (a channel was just
/// added).
pub async fn create_defaults_for_account(
    pool: &PgPool,
    account_id: Uuid,
    channel_id: Uuid,
) -> Result<(), sqlx::Error> {
    let kinds: Vec<String> = DEFAULT_INSTANT.iter().map(enum_slug).collect();
    sqlx::query(
        "INSERT INTO alert_rules (site_id, change_kind, channel_id, mode) \
         SELECT s.id, k::change_kind, c.id, 'instant' \
         FROM sites s \
         JOIN alert_channels c ON c.id = $2 AND c.account_id = s.account_id \
         CROSS JOIN unnest($3::text[]) AS k \
         WHERE s.account_id = $1 \
         ON CONFLICT DO NOTHING",
    )
    .bind(account_id)
    .bind(channel_id)
    .bind(kinds)
    .execute(pool)
    .await?;
    Ok(())
}

/// Sets one rule: `instant` sends that kind to the channel at once, otherwise it waits for the
/// digest. Returns `false` (and writes nothing) unless the site and the channel belong to the
/// same account.
pub async fn set(
    pool: &PgPool,
    site_id: Uuid,
    kind: ChangeKind,
    channel_id: Uuid,
    instant: bool,
) -> Result<bool, sqlx::Error> {
    let n = sqlx::query(
        "INSERT INTO alert_rules (site_id, change_kind, channel_id, mode) \
         SELECT s.id, $2::change_kind, c.id, $4::alert_mode \
         FROM sites s JOIN alert_channels c ON c.account_id = s.account_id \
         WHERE s.id = $1 AND c.id = $3 \
         ON CONFLICT (site_id, change_kind, channel_id) DO UPDATE SET mode = EXCLUDED.mode",
    )
    .bind(site_id)
    .bind(enum_slug(&kind))
    .bind(channel_id)
    .bind(if instant { "instant" } else { "digest" })
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n == 1)
}

/// Every rule on the account's sites.
pub async fn grid(pool: &PgPool, account_id: Uuid) -> Result<Vec<RuleCell>, sqlx::Error> {
    let rows: Vec<(Uuid, String, Uuid, String)> = sqlx::query_as(
        "SELECT r.site_id, r.change_kind::text, r.channel_id, r.mode::text \
         FROM alert_rules r JOIN sites s ON s.id = r.site_id \
         WHERE s.account_id = $1 AND r.change_kind IS NOT NULL",
    )
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(site_id, kind, channel_id, mode)| {
            Some(RuleCell {
                site_id,
                kind: serde_json::from_value(serde_json::Value::String(kind)).ok()?,
                channel_id,
                instant: mode == "instant",
            })
        })
        .collect())
}

/// The channels this kind of change is sent to at once on this site.
pub async fn instant_channels_for(
    pool: &PgPool,
    site_id: Uuid,
    kind: ChangeKind,
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT channel_id FROM alert_rules \
         WHERE site_id = $1 AND change_kind = $2::change_kind AND mode = 'instant' \
         ORDER BY created_at, id",
    )
    .bind(site_id)
    .bind(enum_slug(&kind))
    .fetch_all(pool)
    .await
}

/// The crawl, its site and the owner, as the alert planner needs them.
#[derive(Debug, Clone)]
pub struct AlertCrawl {
    pub crawl_id: Uuid,
    pub site_id: Uuid,
    pub domain: String,
    /// `None` for a site nobody owns (a no-signup audit).
    pub account_id: Option<Uuid>,
    /// A no-signup audit: never alerts.
    pub quick: bool,
    pub failure_reason: Option<String>,
    /// Starred key pages (URL hashes).
    pub starred: Vec<u64>,
}

type CrawlRow = (Uuid, String, Option<Uuid>, bool, Option<String>, Vec<i64>);

pub async fn alert_crawl(pool: &PgPool, crawl_id: Uuid) -> Result<Option<AlertCrawl>, sqlx::Error> {
    let row: Option<CrawlRow> = sqlx::query_as(
        "SELECT s.id, s.domain, s.account_id, c.trigger = 'quick', c.failure_reason, s.key_pages \
         FROM crawls c JOIN sites s ON s.id = c.site_id WHERE c.id = $1",
    )
    .bind(crawl_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(
        |(site_id, domain, account_id, quick, failure_reason, key_pages)| AlertCrawl {
            crawl_id,
            site_id,
            domain,
            account_id,
            quick,
            failure_reason,
            starred: key_pages.into_iter().map(crate::hash::from_db).collect(),
        },
    ))
}

/// One row of `changes`, with its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredChange {
    pub id: i64,
    pub kind: ChangeKind,
    pub severity: Severity,
    pub url: Option<String>,
    pub before: String,
    pub after: String,
}

type ChangeRow = (i64, String, String, Option<String>, String, String);

fn stored(rows: Vec<ChangeRow>) -> Vec<StoredChange> {
    rows.into_iter()
        .filter_map(|(id, kind, severity, url, before, after)| {
            Some(StoredChange {
                id,
                kind: serde_json::from_value(serde_json::Value::String(kind)).ok()?,
                severity: serde_json::from_value(serde_json::Value::String(severity)).ok()?,
                url,
                before,
                after,
            })
        })
        .collect()
}

/// The crawl's changes nobody has been told about yet (`alerted_at` unset), most severe first
/// (the order `finalize` wrote them in).
pub async fn unalerted_changes(
    pool: &PgPool,
    crawl_id: Uuid,
) -> Result<Vec<StoredChange>, sqlx::Error> {
    let rows: Vec<ChangeRow> = sqlx::query_as(
        "SELECT id, kind::text, severity::text, url, before_value, after_value FROM changes \
         WHERE crawl_id = $1 AND alerted_at IS NULL ORDER BY id",
    )
    .bind(crawl_id)
    .fetch_all(pool)
    .await?;
    Ok(stored(rows))
}

/// Specific changes of a crawl, in the order they were recorded.
pub async fn changes_by_ids(
    pool: &PgPool,
    crawl_id: Uuid,
    ids: &[i64],
) -> Result<Vec<StoredChange>, sqlx::Error> {
    let rows: Vec<ChangeRow> = sqlx::query_as(
        "SELECT id, kind::text, severity::text, url, before_value, after_value FROM changes \
         WHERE crawl_id = $1 AND id = ANY($2) ORDER BY id",
    )
    .bind(crawl_id)
    .bind(ids)
    .fetch_all(pool)
    .await?;
    Ok(stored(rows))
}

/// The URLs in this crawl that count as key pages: the start page (depth 0), the 20 pages with
/// the most inlinks (ties by URL, as `codoseo_diff::key_pages` ranks them) and the starred ones.
pub async fn key_page_urls(
    pool: &PgPool,
    crawl_id: Uuid,
    starred: &[u64],
) -> Result<HashSet<String>, sqlx::Error> {
    let hashes: Vec<i64> = starred.iter().map(|h| crate::hash::to_db(*h)).collect();
    let urls: Vec<String> = sqlx::query_scalar(
        "SELECT url FROM pages WHERE crawl_id = $1 AND ( \
           depth = 0 OR url_hash = ANY($2) OR url IN ( \
             SELECT url FROM pages WHERE crawl_id = $1 ORDER BY inlinks DESC, url LIMIT 20))",
    )
    .bind(crawl_id)
    .bind(hashes)
    .fetch_all(pool)
    .await?;
    Ok(urls.into_iter().collect())
}

/// One delivery: these changes go to this channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub channel_id: Uuid,
    pub change_ids: Vec<i64>,
}

/// Queues one `send_alert` delivery job per route and marks the routed changes `alerted_at`,
/// in one transaction: a retry of the planning job finds nothing left to route, and the jobs and
/// the marks never disagree. Changes that no route names stay unalerted, for the digest.
pub async fn enqueue_routes(
    pool: &PgPool,
    crawl_id: Uuid,
    routes: &[Route],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mut routed: Vec<i64> = Vec::new();
    for route in routes {
        sqlx::query("INSERT INTO jobs (kind, payload) VALUES ('send_alert', $1)")
            .bind(serde_json::json!({
                "crawl_id": crawl_id,
                "channel_id": route.channel_id,
                "change_ids": route.change_ids,
            }))
            .execute(&mut *tx)
            .await?;
        routed.extend(&route.change_ids);
    }
    sqlx::query("UPDATE changes SET alerted_at = now() WHERE crawl_id = $1 AND id = ANY($2) AND alerted_at IS NULL")
        .bind(crawl_id)
        .bind(routed)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Queues the "couldn't reach your site" delivery to each channel.
pub async fn enqueue_unreachable(
    pool: &PgPool,
    crawl_id: Uuid,
    channels: &[Uuid],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for channel_id in channels {
        sqlx::query("INSERT INTO jobs (kind, payload) VALUES ('send_alert', $1)")
            .bind(serde_json::json!({
                "crawl_id": crawl_id,
                "channel_id": channel_id,
                "unreachable": true,
            }))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}
