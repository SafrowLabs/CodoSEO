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

/// How many fresh audits one IP may start (spec section 8). Audits that join a running one or
/// reuse a cached report cost nothing and aren't counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub per_hour: i64,
    pub per_day: i64,
}

impl Limits {
    /// 3 an hour, 10 a day.
    pub const DEFAULT: Limits = Limits {
        per_hour: 3,
        per_day: 10,
    };
    /// No limit, for tests of everything else.
    pub const NONE: Limits = Limits {
        per_hour: i64::MAX,
        per_day: i64::MAX,
    };
}

/// Which limit turned a visitor away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitWindow {
    Hour,
    Day,
}

/// Who asked for an audit: a visitor on the website or an agent over the no-key MCP tier.
/// Stored in `crawls.source`; the agent daily budget counts the second kind.
///
/// `crawls.source` (its comment in migration 0001 predates the agent tier and can't be edited
/// now) holds: `'web'` and `'agent'` for quick audits, as below; `'agent'` also for the first
/// crawl of a site added by `start_monitoring` (`sites::create_checked`); `'audit'` for the
/// first crawl of a site added from a website audit (`quick::claim`); NULL otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Web,
    Agent,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Web => "web",
            Source::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StartRequest<'a> {
    /// The lowercase host the audit is about (`www.example.com` and `example.com` differ).
    pub domain: &'a str,
    pub start_url: &'a str,
    /// SHA-256 of the claim token the visitor's cookie holds.
    pub claim_hash: &'a [u8],
    /// The visitor's IP hash (daily salt), kept on the crawl for the per-IP limits.
    pub ip_hash: Option<&'a [u8]>,
    /// The same address hashed with yesterday's salt. The limits count rolling windows of up to
    /// 24 hours, which span two salts, so both are counted; only `ip_hash` is stored.
    pub previous_ip_hash: Option<&'a [u8]>,
    /// Per-IP limits; they only apply when there is an `ip_hash`.
    pub limits: Limits,
    pub source: Source,
    /// The most fresh audits agents may start in any 24 hours, over all of them, and an eighth
    /// of it (see [`hourly_share`]) in any hour. Only applies to `Source::Agent` requests;
    /// `None` is no budget. Cached and joined audits never count.
    pub agent_daily_budget: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartOutcome {
    /// A new site and quick crawl were created.
    Started { crawl_id: Uuid },
    /// A finished audit of this domain is less than 24 h old: show that report.
    Cached { crawl_id: Uuid },
    /// An audit of this domain is queued or running: watch that one.
    Joined { crawl_id: Uuid },
    /// The agent budget for the last 24 hours, or its hourly share, is spent. Nothing was
    /// started.
    AgentBudgetReached {
        /// Seconds until the oldest agent audit in the spent window ages out.
        retry_after_secs: i64,
    },
    /// This IP has used up its audits for the hour or the day. Nothing was started.
    Limited {
        window: LimitWindow,
        /// Seconds until the oldest audit in the window ages out.
        retry_after_secs: i64,
    },
}

/// The most fresh agent audits in any hour, for a daily budget: an eighth of it (at least 1), so
/// a client that fakes a hosted connector's `User-Agent` can't spend the day's budget in
/// minutes.
pub fn hourly_share(daily: i64) -> i64 {
    (daily / 8).max(1)
}

/// `example.com` for both `example.com` and `www.example.com`: the two spellings are one site
/// as far as reusing an audit goes.
fn bare_domain(domain: &str) -> &str {
    domain.strip_prefix("www.").unwrap_or(domain)
}

/// Starts an audit unless the domain was audited in the last 24 h or is being audited now.
/// `www.example.com` and `example.com` count as one domain for that; a new audit keeps the host
/// that was asked for.
///
/// An advisory lock on the domain makes two submits at the same moment agree: the second one
/// waits, then finds the first one's crawl and joins it.
pub async fn start(pool: &PgPool, req: &StartRequest<'_>) -> Result<StartOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let bare = bare_domain(req.domain);
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('codoseo.quick.start:' || $1))")
        .bind(bare)
        .execute(&mut *tx)
        .await?;

    let spellings = [bare.to_owned(), format!("www.{bare}")];
    let existing: Option<(Uuid, String)> = sqlx::query_as(&format!(
        "SELECT id, status::text FROM crawls \
         WHERE trigger = 'quick' AND domain = ANY($1) AND status IN ('queued', 'running', 'done') \
           AND created_at > now() - interval '{REUSE_WINDOW}' \
         ORDER BY created_at DESC LIMIT 1"
    ))
    .bind(&spellings[..])
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

    if let (Source::Agent, Some(budget)) = (req.source, req.agent_daily_budget) {
        // Taken after the domain lock and before any IP lock (always in that order, so two
        // starts can't wait on each other): every fresh agent audit passes through here one at
        // a time, so no number of concurrent starts gets past the budget.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('codoseo.quick.agent'))")
            .execute(&mut *tx)
            .await?;
        let (day, hour, day_retry, hour_retry): (i64, i64, Option<f64>, Option<f64>) =
            sqlx::query_as(
                "SELECT count(*), \
                        count(*) FILTER (WHERE created_at > now() - interval '1 hour'), \
                        EXTRACT(EPOCH FROM min(created_at) + interval '1 day' - now())::float8, \
                        EXTRACT(EPOCH FROM min(created_at) FILTER (WHERE created_at > now() - interval '1 hour') \
                                           + interval '1 hour' - now())::float8 \
                 FROM crawls \
                 WHERE trigger = 'quick' AND source = 'agent' \
                   AND created_at > now() - interval '1 day'",
            )
            .fetch_one(&mut *tx)
            .await?;
        let secs = |s: Option<f64>| s.map_or(1, |s| s.ceil().max(1.0) as i64);
        if day >= budget {
            return Ok(StartOutcome::AgentBudgetReached {
                retry_after_secs: secs(day_retry),
            });
        }
        if hour >= hourly_share(budget) {
            return Ok(StartOutcome::AgentBudgetReached {
                retry_after_secs: secs(hour_retry),
            });
        }
    }

    if let Some(ip) = req.ip_hash {
        // Always taken after the domain lock, so two submits from one IP for different domains
        // can't both pass the counts.
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtext('codoseo.quick.ip:' || encode($1, 'hex')))",
        )
        .bind(ip)
        .execute(&mut *tx)
        .await?;
        let hashes: Vec<Vec<u8>> = std::iter::once(ip)
            .chain(req.previous_ip_hash)
            .map(<[u8]>::to_vec)
            .collect();
        let (hour, day, hour_retry, day_retry): (i64, i64, Option<f64>, Option<f64>) =
            sqlx::query_as(
                "SELECT count(*) FILTER (WHERE created_at > now() - interval '1 hour'), \
                        count(*), \
                        EXTRACT(EPOCH FROM min(created_at) FILTER (WHERE created_at > now() - interval '1 hour') \
                                           + interval '1 hour' - now())::float8, \
                        EXTRACT(EPOCH FROM min(created_at) + interval '1 day' - now())::float8 \
                 FROM crawls \
                 WHERE trigger = 'quick' AND requester_ip_hash = ANY($1) \
                   AND created_at > now() - interval '1 day'",
            )
            .bind(&hashes)
            .fetch_one(&mut *tx)
            .await?;
        let secs = |s: Option<f64>| s.map_or(1, |s| s.ceil().max(1.0) as i64);
        if day >= req.limits.per_day {
            return Ok(StartOutcome::Limited {
                window: LimitWindow::Day,
                retry_after_secs: secs(day_retry),
            });
        }
        if hour >= req.limits.per_hour {
            return Ok(StartOutcome::Limited {
                window: LimitWindow::Hour,
                retry_after_secs: secs(hour_retry),
            });
        }
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
         VALUES ($1, $2, $3, 0, $5, $4) RETURNING id",
    )
    .bind(site_id)
    .bind(req.domain)
    .bind(CrawlTrigger::Quick)
    .bind(req.ip_hash)
    .bind(req.source.as_str())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(StartOutcome::Started { crawl_id })
}

/// Where a quick audit stands, in one cheap read: its status and, once it is crawling, the
/// pages done so far. `None` when there is no quick audit with this id. For polling.
pub async fn status(
    pool: &PgPool,
    crawl_id: Uuid,
) -> Result<Option<(crawls::CrawlStatus, Option<u32>)>, sqlx::Error> {
    let row: Option<(crawls::CrawlStatus, Option<serde_json::Value>)> =
        sqlx::query_as("SELECT status, progress FROM crawls WHERE id = $1 AND trigger = 'quick'")
            .bind(crawl_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(status, progress)| {
        let pages_done = progress
            .and_then(|p| serde_json::from_value::<codoseo_core::output::Progress>(p).ok())
            .map(|p| p.pages_done);
        (status, pages_done)
    }))
}

/// Where a waiting audit is in line: 1 means next. `None` when it isn't queued (running,
/// finished, failed) or isn't a quick audit.
pub async fn queue_position(pool: &PgPool, crawl_id: Uuid) -> Result<Option<i64>, sqlx::Error> {
    let position: Option<Option<i64>> = sqlx::query_scalar(
        "SELECT CASE WHEN c.status = 'queued' THEN ( \
                  SELECT count(*) FROM crawls q \
                  WHERE q.trigger = 'quick' AND q.status = 'queued' \
                    AND (q.queued_at, q.id) <= (c.queued_at, c.id)) END \
         FROM crawls c WHERE c.id = $1 AND c.trigger = 'quick'",
    )
    .bind(crawl_id)
    .fetch_optional(pool)
    .await?;
    Ok(position.flatten())
}

/// How many no-signup audits are waiting for a crawler.
pub async fn queue_depth(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM crawls WHERE trigger = 'quick' AND status = 'queued'")
        .fetch_one(pool)
        .await
}

/// How many sign-in emails an audit's unlock form may trigger, per audit and per recipient, per
/// hour. The form mails an arbitrary address, so the recipient cap is what stops it being used
/// to flood someone's inbox through many different audits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnlockCaps {
    pub per_audit: i64,
    pub per_address: i64,
}

impl UnlockCaps {
    pub const DEFAULT: UnlockCaps = UnlockCaps {
        per_audit: 3,
        per_address: 3,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockSlot {
    /// The token was stored: send the email.
    Created,
    AuditCapReached,
    AddressCapReached,
}

/// Stores the magic-link token for an unlock email unless a cap is reached. The counts and the
/// insert happen under advisory locks, so concurrent requests can't all pass the count before
/// any of them inserts. `canonical` (the address's deduplication key) must also be in `payload`,
/// which is what later counts read.
pub async fn create_unlock_token(
    pool: &PgPool,
    crawl_id: Uuid,
    canonical: &str,
    token_hash: &[u8],
    payload: serde_json::Value,
    ttl: time::Duration,
    caps: UnlockCaps,
) -> Result<UnlockSlot, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // Always address first, then audit, so two requests can't wait on each other.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('codoseo.unlock.address:' || $1))")
        .bind(canonical)
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('codoseo.unlock.audit:' || $1))")
        .bind(crawl_id.to_string())
        .execute(&mut *tx)
        .await?;
    let (audit, address): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE payload->>'audit' = $1), \
                count(*) FILTER (WHERE payload->>'canonical' = $2) \
         FROM login_tokens \
         WHERE purpose = 'magic_link' AND created_at > now() - interval '1 hour'",
    )
    .bind(crawl_id.to_string())
    .bind(canonical)
    .fetch_one(&mut *tx)
    .await?;
    if audit >= caps.per_audit {
        return Ok(UnlockSlot::AuditCapReached);
    }
    if address >= caps.per_address {
        return Ok(UnlockSlot::AddressCapReached);
    }
    sqlx::query(
        "INSERT INTO login_tokens (purpose, token_hash, payload, expires_at) \
         VALUES ('magic_link', $1, $2, now() + make_interval(secs => $3))",
    )
    .bind(token_hash)
    .bind(payload)
    .bind(ttl.as_seconds_f64())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(UnlockSlot::Created)
}

/// How many start-monitoring emails the no-key MCP tool may trigger. The tool mails an address
/// an agent chose, so the caps are what stops it flooding someone's inbox, and the daily total
/// bounds what a stolen or misused connector can send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitoringCaps {
    /// Per recipient (canonical address), per hour.
    pub per_address: i64,
    /// Per recipient, per 24 hours: the hourly cap alone would let someone mail an address
    /// three times an hour all day.
    pub per_address_day: i64,
    /// Per client address (hash), per hour. Only applies when the caller has an `ip_hash`.
    pub per_ip: i64,
    /// Over everyone, per 24 hours.
    pub per_day: i64,
    /// Over everyone, per hour, so the daily budget can't be drained in minutes.
    pub per_hour: i64,
}

impl MonitoringCaps {
    /// 3 per address (5 a day) and 3 per client address an hour; `per_day` is the configured
    /// daily cap, of which an eighth (at least 1) may go in any one hour.
    pub fn with_daily(per_day: i64) -> MonitoringCaps {
        MonitoringCaps {
            per_address: 3,
            per_address_day: 5,
            per_ip: 3,
            per_day,
            per_hour: hourly_share(per_day),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitoringSlot {
    /// The token was stored: send the email.
    Created,
    AddressCapReached,
    /// The address has had its emails for the last 24 hours.
    AddressDayCapReached,
    IpCapReached,
    DailyCapReached,
    HourlyCapReached,
}

/// Who asked for a start-monitoring email, as far as the per-IP cap is concerned: today's hash
/// (stored in the token's payload) and yesterday's (also counted, since an hour window can
/// span the salt change at midnight UTC).
#[derive(Debug, Clone, Copy)]
pub struct Requester<'a> {
    pub ip_hash: &'a [u8],
    pub previous_ip_hash: Option<&'a [u8]>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Stores a start-monitoring token (`payload` holds `email`, `canonical`, `start_url` and
/// `domain`) unless a cap is reached. Counts and insert happen under advisory locks, always
/// taken global first, then address, then client, so concurrent requests can't all pass the
/// counts before any of them inserts. With a `requester` the token's payload also carries its
/// IP hash (`ip`), which later counts read.
pub async fn create_monitoring_token(
    pool: &PgPool,
    canonical: &str,
    requester: Option<Requester<'_>>,
    token_hash: &[u8],
    mut payload: serde_json::Value,
    ttl: time::Duration,
    caps: MonitoringCaps,
) -> Result<MonitoringSlot, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('codoseo.monitor.global'))")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('codoseo.monitor.address:' || $1))")
        .bind(canonical)
        .execute(&mut *tx)
        .await?;
    let ips: Vec<String> = requester
        .iter()
        .flat_map(|r| std::iter::once(r.ip_hash).chain(r.previous_ip_hash))
        .map(hex)
        .collect();
    if let Some(r) = requester {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('codoseo.monitor.ip:' || $1))")
            .bind(hex(r.ip_hash))
            .execute(&mut *tx)
            .await?;
        payload["ip"] = serde_json::json!(hex(r.ip_hash));
    }
    let (day, hour, address_day, address, ip): (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT count(*), \
                count(*) FILTER (WHERE created_at > now() - interval '1 hour'), \
                count(*) FILTER (WHERE payload->>'canonical' = $1), \
                count(*) FILTER (WHERE created_at > now() - interval '1 hour' \
                                   AND payload->>'canonical' = $1), \
                count(*) FILTER (WHERE created_at > now() - interval '1 hour' \
                                   AND payload->>'ip' = ANY($2)) \
         FROM login_tokens \
         WHERE purpose = 'start_monitoring' AND created_at > now() - interval '1 day'",
    )
    .bind(canonical)
    .bind(&ips)
    .fetch_one(&mut *tx)
    .await?;
    if day >= caps.per_day {
        return Ok(MonitoringSlot::DailyCapReached);
    }
    if hour >= caps.per_hour {
        return Ok(MonitoringSlot::HourlyCapReached);
    }
    if address >= caps.per_address {
        return Ok(MonitoringSlot::AddressCapReached);
    }
    if address_day >= caps.per_address_day {
        return Ok(MonitoringSlot::AddressDayCapReached);
    }
    if requester.is_some() && ip >= caps.per_ip {
        return Ok(MonitoringSlot::IpCapReached);
    }
    sqlx::query(
        "INSERT INTO login_tokens (purpose, token_hash, payload, expires_at) \
         VALUES ('start_monitoring', $1, $2, now() + make_interval(secs => $3))",
    )
    .bind(token_hash)
    .bind(payload)
    .bind(ttl.as_seconds_f64())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(MonitoringSlot::Created)
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
/// the site limit. With one of the visitor's claim tokens (`claim_hashes`, since a visitor can
/// have audited several sites), a still-unclaimed site and a quick crawl that has ended, the
/// audit's own site is attached, which keeps the quick crawl in the account's history;
/// otherwise (another browser, an audit someone else already claimed, a crawl still running,
/// which would otherwise be governed by the account's larger limits) a fresh site is made.
/// The first crawl is marked `source = 'audit'`, so the funnel counts only these.
pub async fn claim(
    pool: &PgPool,
    account_id: Uuid,
    crawl_id: Uuid,
    claim_hashes: &[Vec<u8>],
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
        status: String,
    }

    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM accounts WHERE id = $1 FOR UPDATE")
        .bind(account_id)
        .fetch_one(&mut *tx)
        .await?;

    let quick: Option<Quick> = sqlx::query_as(
        "SELECT s.id AS site_id, s.domain, s.start_url, s.account_id, s.claim_token_hash, \
                c.status::text AS status \
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

    let ended = matches!(quick.status.as_str(), "done" | "failed");
    let own_audit = quick.account_id.is_none()
        && ended
        && quick
            .claim_token_hash
            .as_deref()
            .is_some_and(|stored| claim_hashes.iter().any(|h| h.as_slice() == stored));
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
    sqlx::query(
        "INSERT INTO crawls (site_id, domain, trigger, priority, source) VALUES ($1, $2, $3, $4, 'audit')",
    )
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
