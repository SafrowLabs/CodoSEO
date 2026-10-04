//! Sites owned by an account. Every lookup takes the account ID, so one account can never read
//! another's site: a miss is simply `None`, which the web side turns into a 404.

use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::crawl_queue::CrawlTrigger;
use crate::hash;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub id: Uuid,
    pub account_id: Option<Uuid>,
    pub domain: String,
    pub start_url: String,
    pub schedule: Option<String>,
    pub monitoring_active: bool,
    /// Starred key pages, as URL hashes.
    pub key_pages: Vec<u64>,
    pub created_at: OffsetDateTime,
}

#[derive(FromRow)]
struct SiteRow {
    id: Uuid,
    account_id: Option<Uuid>,
    domain: String,
    start_url: String,
    schedule: Option<String>,
    monitoring_active: bool,
    key_pages: Vec<i64>,
    created_at: OffsetDateTime,
}

impl From<SiteRow> for Site {
    fn from(r: SiteRow) -> Site {
        Site {
            id: r.id,
            account_id: r.account_id,
            domain: r.domain,
            start_url: r.start_url,
            schedule: r.schedule,
            monitoring_active: r.monitoring_active,
            key_pages: r.key_pages.into_iter().map(hash::from_db).collect(),
            created_at: r.created_at,
        }
    }
}

const COLUMNS: &str =
    "id, account_id, domain, start_url, schedule, monitoring_active, key_pages, created_at";

/// The account's sites, oldest first (the first one is the default after login).
pub async fn list_for_account(pool: &PgPool, account_id: Uuid) -> Result<Vec<Site>, sqlx::Error> {
    let rows: Vec<SiteRow> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM sites WHERE account_id = $1 ORDER BY created_at, id"
    ))
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Site::from).collect())
}

/// One of the account's sites, or `None` when it doesn't exist or belongs to someone else.
pub async fn get_for_account(
    pool: &PgPool,
    account_id: Uuid,
    site_id: Uuid,
) -> Result<Option<Site>, sqlx::Error> {
    let row: Option<SiteRow> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM sites WHERE id = $1 AND account_id = $2"
    ))
    .bind(site_id)
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Site::from))
}

pub async fn count_for_account(pool: &PgPool, account_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM sites WHERE account_id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await
}

/// Adds a site. `schedule` is `weekly`, `daily` or `None`.
pub async fn create(
    pool: &PgPool,
    account_id: Uuid,
    domain: &str,
    start_url: &str,
    schedule: Option<&str>,
) -> Result<Site, sqlx::Error> {
    let row: SiteRow = sqlx::query_as(&format!(
        "INSERT INTO sites (account_id, domain, start_url, schedule) VALUES ($1, $2, $3, $4) \
         RETURNING {COLUMNS}"
    ))
    .bind(account_id)
    .bind(domain)
    .bind(start_url)
    .bind(schedule)
    .fetch_one(pool)
    .await?;
    Ok(row.into())
}

/// What [`create_checked`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateOutcome {
    /// The site was added and its first crawl queued.
    Created(Site),
    /// The account already has `max_sites` sites.
    LimitReached,
    /// The account already has a site on this domain.
    Duplicate,
}

/// Adds a site and queues its `first` crawl at `first_priority`, in one transaction, unless
/// the account already has `max_sites` sites (`None` means no limit) or a site on `domain`.
///
/// The account row is locked (`FOR UPDATE`) around the checks and the inserts, so two submits
/// at once can't go past the limit or add the same domain twice.
pub async fn create_checked(
    pool: &PgPool,
    account_id: Uuid,
    domain: &str,
    start_url: &str,
    schedule: Option<&str>,
    max_sites: Option<i64>,
    first_priority: i16,
) -> Result<CreateOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM accounts WHERE id = $1 FOR UPDATE")
        .bind(account_id)
        .fetch_one(&mut *tx)
        .await?;

    let (count, duplicate): (i64, Option<bool>) =
        sqlx::query_as("SELECT count(*), bool_or(domain = $2) FROM sites WHERE account_id = $1")
            .bind(account_id)
            .bind(domain)
            .fetch_one(&mut *tx)
            .await?;
    if max_sites.is_some_and(|max| count >= max) {
        return Ok(CreateOutcome::LimitReached);
    }
    if duplicate == Some(true) {
        return Ok(CreateOutcome::Duplicate);
    }

    let row: SiteRow = sqlx::query_as(&format!(
        "INSERT INTO sites (account_id, domain, start_url, schedule) VALUES ($1, $2, $3, $4) \
         RETURNING {COLUMNS}"
    ))
    .bind(account_id)
    .bind(domain)
    .bind(start_url)
    .bind(schedule)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO crawls (site_id, domain, trigger, priority) VALUES ($1, $2, $3, $4)")
        .bind(row.id)
        .bind(&row.domain)
        .bind(CrawlTrigger::First)
        .bind(first_priority)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(CreateOutcome::Created(row.into()))
}

/// Stars or unstars a key page. Returns whether the page is starred afterwards.
pub async fn toggle_key_page(
    pool: &PgPool,
    account_id: Uuid,
    site_id: Uuid,
    url_hash: u64,
) -> Result<Option<bool>, sqlx::Error> {
    let starred: Option<bool> = sqlx::query_scalar(
        "UPDATE sites SET key_pages = CASE \
             WHEN $3 = ANY(key_pages) THEN array_remove(key_pages, $3) \
             ELSE array_append(key_pages, $3) END \
         WHERE id = $1 AND account_id = $2 \
         RETURNING $3 = ANY(key_pages)",
    )
    .bind(site_id)
    .bind(account_id)
    .bind(hash::to_db(url_hash))
    .fetch_optional(pool)
    .await?;
    Ok(starred)
}
