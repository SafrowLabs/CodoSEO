//! The no-signup audit. A visitor's audit is a site with no account (and a hashed claim token)
//! plus a `quick` crawl in the top priority lane, so the worker and the result readers treat it
//! like any other crawl. The public report is keyed by the crawl id; unlocking it with an email
//! attaches the site to the new account and queues the 500-page first crawl.

use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::crawl_queue::CrawlTrigger;
use crate::crawls::{self, Crawl};
use crate::sites::{COLUMNS, Site, SiteRow};

/// A finished quick audit of a domain is reused for this long.
pub const REUSE_WINDOW: &str = "24 hours";

#[derive(Debug, Clone)]
pub struct StartRequest<'a> {
    /// The lowercase host the audit is about (`www.example.com` and `example.com` differ).
    pub domain: &'a str,
    pub start_url: &'a str,
    /// SHA-256 of the claim token the visitor's cookie holds.
    pub claim_hash: &'a [u8],
    /// The visitor's IP hash (daily salt), kept on the crawl for the per-IP limits.
    pub ip_hash: Option<&'a [u8]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartOutcome {
    /// A new site and quick crawl were created.
    Started { crawl_id: Uuid },
    /// A finished audit of this domain is less than 24 h old: show that report.
    Cached { crawl_id: Uuid },
    /// An audit of this domain is queued or running: watch that one.
    Joined { crawl_id: Uuid },
}

/// Starts an audit unless the domain was audited in the last 24 h or is being audited now.
///
/// An advisory lock on the domain makes two submits at the same moment agree: the second one
/// waits, then finds the first one's crawl and joins it.
pub async fn start(pool: &PgPool, req: &StartRequest<'_>) -> Result<StartOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('codoseo.quick.start:' || $1))")
        .bind(req.domain)
        .execute(&mut *tx)
        .await?;

    let existing: Option<(Uuid, String)> = sqlx::query_as(&format!(
        "SELECT id, status::text FROM crawls \
         WHERE trigger = 'quick' AND domain = $1 AND status IN ('queued', 'running', 'done') \
           AND created_at > now() - interval '{REUSE_WINDOW}' \
         ORDER BY created_at DESC LIMIT 1"
    ))
    .bind(req.domain)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((crawl_id, status)) = existing {
        tx.commit().await?;
        return Ok(if status == "done" {
            StartOutcome::Cached { crawl_id }
        } else {
            StartOutcome::Joined { crawl_id }
        });
    }

    let site_id: Uuid = sqlx::query_scalar(
        "INSERT INTO sites (domain, start_url, claim_token_hash) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(req.domain)
    .bind(req.start_url)
    .bind(req.claim_hash)
    .fetch_one(&mut *tx)
    .await?;
    let crawl_id: Uuid = sqlx::query_scalar(
        "INSERT INTO crawls (site_id, domain, trigger, priority, source, requester_ip_hash) \
         VALUES ($1, $2, $3, 0, 'web', $4) RETURNING id",
    )
    .bind(site_id)
    .bind(req.domain)
    .bind(CrawlTrigger::Quick)
    .bind(req.ip_hash)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(StartOutcome::Started { crawl_id })
}

/// A quick audit as the public report page reads it.
#[derive(Debug, Clone)]
pub struct Audit {
    pub site_id: Uuid,
    pub domain: String,
    pub start_url: String,
    /// The site already belongs to an account.
    pub claimed: bool,
    pub crawl: Crawl,
}

/// The quick audit with this crawl id. Anything else (an ordinary crawl, an unknown or purged
/// id) is `None`, so the report URL can only ever show a no-signup audit.
pub async fn get(pool: &PgPool, crawl_id: Uuid) -> Result<Option<Audit>, sqlx::Error> {
    #[derive(FromRow)]
    struct Head {
        site_id: Uuid,
        domain: String,
        start_url: String,
        claimed: bool,
    }
    let head: Option<Head> = sqlx::query_as(
        "SELECT s.id AS site_id, s.domain, s.start_url, s.account_id IS NOT NULL AS claimed \
         FROM crawls c JOIN sites s ON s.id = c.site_id \
         WHERE c.id = $1 AND c.trigger = 'quick'",
    )
    .bind(crawl_id)
    .fetch_optional(pool)
    .await?;
    let Some(head) = head else { return Ok(None) };
    let Some(crawl) = crawls::get(pool, head.site_id, crawl_id).await? else {
        return Ok(None);
    };
    Ok(Some(Audit {
        site_id: head.site_id,
        domain: head.domain,
        start_url: head.start_url,
        claimed: head.claimed,
        crawl,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// The visitor's own audit site now belongs to the account; its first crawl is queued.
    Attached(Site),
    /// A fresh site for the domain was created (no matching claim cookie); first crawl queued.
    Created(Site),
    /// The account already has a site on this domain; nothing was queued.
    Existing(Site),
    /// The account is at its plan's site limit; nothing was queued. Holds its oldest site.
    LimitReached(Option<Site>),
    /// No quick audit has this id.
    NotFound,
}

/// Gives the account the audited site and queues its `first` crawl at `first_priority`.
///
/// The account row is locked around the checks and writes, so two unlocks at once can't pass
/// the site limit. With the visitor's claim cookie (`claim_hash`) and a still-unclaimed site,
/// the audit's own site is attached, which keeps the quick crawl in the account's history;
/// otherwise (another browser, an audit someone else already claimed) a fresh site is made.
pub async fn claim(
    pool: &PgPool,
    account_id: Uuid,
    crawl_id: Uuid,
    claim_hash: Option<&[u8]>,
    max_sites: Option<i64>,
    first_priority: i16,
    schedule: Option<&str>,
) -> Result<ClaimOutcome, sqlx::Error> {
    #[derive(FromRow)]
    struct Quick {
        site_id: Uuid,
        domain: String,
        start_url: String,
        account_id: Option<Uuid>,
        claim_token_hash: Option<Vec<u8>>,
    }

    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM accounts WHERE id = $1 FOR UPDATE")
        .bind(account_id)
        .fetch_one(&mut *tx)
        .await?;

    let quick: Option<Quick> = sqlx::query_as(
        "SELECT s.id AS site_id, s.domain, s.start_url, s.account_id, s.claim_token_hash \
         FROM crawls c JOIN sites s ON s.id = c.site_id \
         WHERE c.id = $1 AND c.trigger = 'quick' FOR UPDATE OF s",
    )
    .bind(crawl_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(quick) = quick else {
        return Ok(ClaimOutcome::NotFound);
    };

    let existing: Option<SiteRow> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM sites WHERE account_id = $1 AND domain = $2 ORDER BY created_at LIMIT 1"
    ))
    .bind(account_id)
    .bind(&quick.domain)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(site) = existing {
        return Ok(ClaimOutcome::Existing(site.into()));
    }

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sites WHERE account_id = $1")
        .bind(account_id)
        .fetch_one(&mut *tx)
        .await?;
    if max_sites.is_some_and(|max| count >= max) {
        let oldest: Option<SiteRow> = sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM sites WHERE account_id = $1 ORDER BY created_at, id LIMIT 1"
        ))
        .bind(account_id)
        .fetch_optional(&mut *tx)
        .await?;
        return Ok(ClaimOutcome::LimitReached(oldest.map(Site::from)));
    }

    let own_audit = quick.account_id.is_none()
        && matches!((claim_hash, quick.claim_token_hash.as_deref()), (Some(a), Some(b)) if a == b);
    let (row, attached): (SiteRow, bool) = if own_audit {
        let row = sqlx::query_as(&format!(
            "UPDATE sites SET account_id = $2, claim_token_hash = NULL, schedule = $3 \
             WHERE id = $1 RETURNING {COLUMNS}"
        ))
        .bind(quick.site_id)
        .bind(account_id)
        .bind(schedule)
        .fetch_one(&mut *tx)
        .await?;
        (row, true)
    } else {
        let row = sqlx::query_as(&format!(
            "INSERT INTO sites (account_id, domain, start_url, schedule) VALUES ($1, $2, $3, $4) \
             RETURNING {COLUMNS}"
        ))
        .bind(account_id)
        .bind(&quick.domain)
        .bind(&quick.start_url)
        .bind(schedule)
        .fetch_one(&mut *tx)
        .await?;
        (row, false)
    };
    sqlx::query("INSERT INTO crawls (site_id, domain, trigger, priority) VALUES ($1, $2, $3, $4)")
        .bind(row.id)
        .bind(&row.domain)
        .bind(CrawlTrigger::First)
        .bind(first_priority)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let site: Site = row.into();
    Ok(if attached {
        ClaimOutcome::Attached(site)
    } else {
        ClaimOutcome::Created(site)
    })
}
