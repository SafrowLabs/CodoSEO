//! What the scheduler reads and writes: sites that are due, digests, the daily cleanup,
//! inactivity warnings, the funnel's "active after 4 weeks" event and the heartbeat.
//!
//! Time is always an argument (`now`), never `now()` in SQL, so tests can move the clock. The
//! functions that must not run twice at once (two web containers) claim their work with a row
//! lock or a guarded update.

use codoseo_core::plan::{Plan, Schedule};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::accounts::parse_plan;

/// A monitored site whose time has come, with what the scheduler needs to pick its next slot.
#[derive(Debug, Clone)]
pub struct DueSite {
    pub id: Uuid,
    pub domain: String,
    pub plan: Plan,
    /// The schedule the site asked for, before the plan's limit is applied.
    pub schedule: Option<Schedule>,
    /// Local hour of a daily crawl, `None` for the site's default hour.
    pub scheduled_hour: Option<u8>,
    /// The owner's IANA zone name.
    pub timezone: String,
    /// `None` for a site that has never been given a slot.
    pub next_crawl_at: Option<OffsetDateTime>,
}

/// Due sites locked by an open transaction. Other schedulers skip them until [`commit`]
/// (or a drop, which rolls back).
///
/// [`commit`]: DueBatch::commit
pub struct DueBatch {
    tx: Transaction<'static, Postgres>,
    pub sites: Vec<DueSite>,
}

/// Locks up to `limit` monitored sites of unpaused accounts whose `next_crawl_at` is at or
/// before `now` (or was never set), with `FOR UPDATE SKIP LOCKED`.
pub async fn claim_due_sites(
    pool: &PgPool,
    now: OffsetDateTime,
    limit: i64,
) -> Result<DueBatch, sqlx::Error> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        domain: String,
        plan: String,
        schedule: Option<String>,
        scheduled_hour: Option<i16>,
        timezone: String,
        next_crawl_at: Option<OffsetDateTime>,
    }
    let mut tx = pool.begin().await?;
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT s.id, s.domain, a.plan::text AS plan, s.schedule, s.scheduled_hour, \
                a.timezone, s.next_crawl_at \
         FROM sites s JOIN accounts a ON a.id = s.account_id \
         WHERE s.monitoring_active AND s.account_id IS NOT NULL AND s.schedule IS NOT NULL \
           AND NOT a.paused \
           AND (s.next_crawl_at IS NULL OR s.next_crawl_at <= $1) \
         ORDER BY s.next_crawl_at NULLS FIRST, s.id \
         LIMIT $2 \
         FOR UPDATE OF s SKIP LOCKED",
    )
    .bind(now)
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;
    let sites = rows
        .into_iter()
        .map(|r| DueSite {
            id: r.id,
            domain: r.domain,
            plan: parse_plan(&r.plan),
            schedule: r.schedule.as_deref().and_then(parse_schedule),
            scheduled_hour: r
                .scheduled_hour
                .and_then(|h| u8::try_from(h).ok())
                .filter(|h| *h < 24),
            timezone: r.timezone,
            next_crawl_at: r.next_crawl_at,
        })
        .collect();
    Ok(DueBatch { tx, sites })
}

fn parse_schedule(slug: &str) -> Option<Schedule> {
    serde_json::from_value(serde_json::Value::String(slug.to_owned())).ok()
}

impl DueBatch {
    /// Sets the site's `next_crawl_at` and, with a `crawl_priority`, queues a `schedule` crawl
    /// first, unless the site already has one queued or running. Returns whether a crawl was
    /// queued.
    pub async fn advance(
        &mut self,
        site: &DueSite,
        crawl_priority: Option<i16>,
        next_crawl_at: OffsetDateTime,
    ) -> Result<bool, sqlx::Error> {
        let mut queued = false;
        if let Some(priority) = crawl_priority {
            queued = sqlx::query(
                "INSERT INTO crawls (site_id, domain, trigger, priority) \
                 SELECT $1, $2, 'schedule', $3 \
                 WHERE NOT EXISTS ( \
                   SELECT 1 FROM crawls WHERE site_id = $1 AND status IN ('queued', 'running'))",
            )
            .bind(site.id)
            .bind(&site.domain)
            .bind(priority)
            .execute(&mut *self.tx)
            .await?
            .rows_affected()
                == 1;
        }
        sqlx::query("UPDATE sites SET next_crawl_at = $2 WHERE id = $1")
            .bind(site.id)
            .bind(next_crawl_at)
            .execute(&mut *self.tx)
            .await?;
        Ok(queued)
    }

    pub async fn commit(self) -> Result<(), sqlx::Error> {
        self.tx.commit().await
    }
}

/// An account that may be owed this week's digest.
#[derive(Debug, Clone, FromRow)]
pub struct DigestCandidate {
    pub id: Uuid,
    pub timezone: String,
    pub last_digest_at: Option<OffsetDateTime>,
}

/// Unpaused accounts with at least one finished crawl whose last digest is older than five
/// days (or missing). The caller applies the Monday-08:00-local rule per time zone.
pub async fn digest_candidates(
    pool: &PgPool,
    now: OffsetDateTime,
) -> Result<Vec<DigestCandidate>, sqlx::Error> {
    sqlx::query_as(
        "SELECT a.id, a.timezone, a.last_digest_at FROM accounts a \
         WHERE NOT a.paused \
           AND (a.last_digest_at IS NULL OR a.last_digest_at < $1) \
           AND EXISTS (SELECT 1 FROM sites s JOIN crawls c ON c.site_id = s.id \
                       WHERE s.account_id = a.id AND c.status = 'done') \
         ORDER BY a.created_at, a.id",
    )
    .bind(now - Duration::days(5))
    .fetch_all(pool)
    .await
}

/// Stamps `last_digest_at = now` and enqueues `send_digest { account_id }` in one transaction,
/// but only if the account's `last_digest_at` is still what `candidate` saw, so two schedulers
/// can't both send. Returns whether this call did.
pub async fn enqueue_digest(
    pool: &PgPool,
    candidate: &DigestCandidate,
    now: OffsetDateTime,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let stamped = sqlx::query(
        "UPDATE accounts SET last_digest_at = $2 \
         WHERE id = $1 AND last_digest_at IS NOT DISTINCT FROM $3",
    )
    .bind(candidate.id)
    .bind(now)
    .bind(candidate.last_digest_at)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if stamped != 1 {
        return Ok(false);
    }
    insert_job(
        &mut tx,
        "send_digest",
        serde_json::json!({ "account_id": candidate.id }),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Enqueues the `cleanup` job if none was enqueued for `today` (a `YYYY-MM-DD` UTC date) yet,
/// guarded by the `last_cleanup_on` instance setting. Returns whether this call did.
pub async fn enqueue_daily_cleanup(
    pool: &PgPool,
    today: &str,
    now: OffsetDateTime,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // Inserts the first day and moves a later day in. The `WHERE` makes a repeat of `today`, or an
    // earlier day from a scheduler with a slow clock, a no-op, so the day never flips back.
    // (ISO dates compare correctly as text.)
    let claimed = sqlx::query(
        "INSERT INTO instance_settings (key, value, updated_at) VALUES ('last_cleanup_on', $1, $2) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at \
         WHERE instance_settings.value::text < EXCLUDED.value::text",
    )
    .bind(serde_json::Value::String(today.to_owned()))
    .bind(now)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if claimed != 1 {
        return Ok(false);
    }
    insert_job(&mut tx, "cleanup", serde_json::json!({})).await?;
    tx.commit().await?;
    Ok(true)
}

async fn insert_job(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    payload: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO jobs (kind, payload) VALUES ($1::job_kind, $2)")
        .bind(kind)
        .bind(payload)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// A Free account with no sign-in or email click for this long gets the "Keep monitoring?"
/// email.
pub const INACTIVE_AFTER: Duration = Duration::days(30);
/// How long after the warning an account that stayed silent is paused.
pub const WARNING_GRACE: Duration = Duration::days(7);
/// How long a "Keep monitoring?" link works.
pub const RESUME_LINK_TTL: Duration = Duration::days(7);

/// The last time the person was seen: the latest sign-in, email click, or visit with a session
/// (`sessions.last_seen_at`, refreshed by the web app at most every 5 minutes), failing all of
/// those the day they signed up.
const LAST_ACTIVITY: &str = "GREATEST(a.last_login_at, a.last_email_click_at, a.created_at, \
     (SELECT max(se.last_seen_at) FROM sessions se WHERE se.account_id = a.id))";

/// The same without the sessions lookup: cheap, so it narrows the accounts first.
const LAST_LOGIN_OR_CLICK: &str = "GREATEST(a.last_login_at, a.last_email_click_at, a.created_at)";

/// What happened to the account's latest "Keep monitoring?" email job. The job payload carries
/// `keep_monitoring_for`, the account id.
const WARNING_JOB_STATUS: &str = "(SELECT j.status::text FROM jobs j \
     WHERE j.kind = 'send_email' AND j.payload->>'keep_monitoring_for' = a.id::text \
     ORDER BY j.created_at DESC LIMIT 1)";

/// Whether a Free account is due its warning at `$now`: inactive for [`INACTIVE_AFTER`], with
/// something to monitor, and not warned since it was last active.
fn due_for_warning(now: &str) -> String {
    format!(
        "a.plan = 'free' AND NOT a.paused \
         AND {LAST_LOGIN_OR_CLICK} < {now} - interval '30 days' \
         AND {LAST_ACTIVITY} < {now} - interval '30 days' \
         AND (a.keep_monitoring_sent_at IS NULL OR a.keep_monitoring_sent_at < {LAST_ACTIVITY}) \
         AND EXISTS (SELECT 1 FROM sites s WHERE s.account_id = a.id AND s.monitoring_active)"
    )
}

/// A Free account that should be asked whether to keep monitoring.
#[derive(Debug, Clone, FromRow)]
pub struct InactiveAccount {
    pub id: Uuid,
    pub email: String,
}

/// Free accounts due the "Keep monitoring?" email at `now`, oldest first.
pub async fn accounts_to_warn(
    pool: &PgPool,
    now: OffsetDateTime,
    limit: i64,
) -> Result<Vec<InactiveAccount>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT a.id, a.email FROM accounts a WHERE {} ORDER BY a.created_at, a.id LIMIT $2",
        due_for_warning("$1")
    ))
    .bind(now)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Marks the account warned (`keep_monitoring_sent_at = now`), stores its `resume_monitoring`
/// token and enqueues the `send_email` job with `email`, in one transaction, if the account is
/// still due. Returns whether it did.
pub async fn send_keep_monitoring(
    pool: &PgPool,
    account_id: Uuid,
    now: OffsetDateTime,
    token_hash: &[u8],
    email: serde_json::Value,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let marked = sqlx::query(&format!(
        "UPDATE accounts a SET keep_monitoring_sent_at = $2 WHERE a.id = $1 AND {}",
        due_for_warning("$2")
    ))
    .bind(account_id)
    .bind(now)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if marked != 1 {
        return Ok(false);
    }
    crate::auth::create_token(
        &mut *tx,
        crate::auth::TokenPurpose::ResumeMonitoring,
        token_hash,
        Some(account_id),
        None,
        RESUME_LINK_TTL,
    )
    .await?;
    let mut email = email;
    email["keep_monitoring_for"] = serde_json::json!(account_id);
    insert_job(&mut tx, "send_email", email).await?;
    tx.commit().await?;
    Ok(true)
}

/// Clears `keep_monitoring_sent_at` for accounts whose latest warning email failed for good
/// (the job ran out of attempts), so the next tick warns them again. A paused account is
/// skipped: there is nothing left to warn it about. Returns how many.
pub async fn reset_failed_warnings(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let done = sqlx::query(&format!(
        "UPDATE accounts a SET keep_monitoring_sent_at = NULL \
         WHERE a.plan = 'free' AND NOT a.paused AND a.keep_monitoring_sent_at IS NOT NULL \
           AND {WARNING_JOB_STATUS} = 'failed'"
    ))
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

/// Pauses Free accounts that were warned at least [`WARNING_GRACE`] ago, whose warning email was
/// delivered, and who haven't been seen since. A warning still queued or retrying does not count
/// (a mail outage must not pause anyone). Returns how many were paused.
pub async fn pause_unresponsive(pool: &PgPool, now: OffsetDateTime) -> Result<u64, sqlx::Error> {
    let done = sqlx::query(&format!(
        "UPDATE accounts a SET paused = true \
         WHERE a.plan = 'free' AND NOT a.paused \
           AND a.keep_monitoring_sent_at <= $1 - interval '7 days' \
           AND {LAST_ACTIVITY} <= a.keep_monitoring_sent_at \
           AND {WARNING_JOB_STATUS} = 'done'"
    ))
    .bind(now)
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

/// Records the funnel event `active_after_4_weeks`, once per account: it signed up at least 28
/// days before `now`, isn't paused, and had a crawl finish in the 7 days before `now`. Returns
/// how many events it wrote.
pub async fn record_active_after_4_weeks(
    pool: &PgPool,
    now: OffsetDateTime,
) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // Without a unique key on the event, this keeps two schedulers from both writing it.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('codoseo.scheduler.active_after_4_weeks'))")
        .execute(&mut *tx)
        .await?;
    let done = sqlx::query(
        "INSERT INTO events (account_id, site_id, kind) \
         SELECT DISTINCT ON (a.id) a.id, s.id, 'active_after_4_weeks' \
         FROM accounts a \
           JOIN sites s ON s.account_id = a.id \
           JOIN crawls c ON c.site_id = s.id \
         WHERE a.created_at <= $1 - interval '28 days' AND NOT a.paused \
           AND c.status = 'done' AND c.finished_at > $1 - interval '7 days' \
           AND NOT EXISTS (SELECT 1 FROM events e \
                           WHERE e.account_id = a.id AND e.kind = 'active_after_4_weeks') \
         ORDER BY a.id, c.finished_at DESC",
    )
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(done.rows_affected())
}

const HEARTBEAT_KEY: &str = "scheduler_heartbeat";

/// Records that the scheduler ran at `now`.
pub async fn record_heartbeat(pool: &PgPool, now: OffsetDateTime) -> Result<(), sqlx::Error> {
    let stamp = now
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    sqlx::query(
        "INSERT INTO instance_settings (key, value, updated_at) VALUES ($1, $2, $3) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at",
    )
    .bind(HEARTBEAT_KEY)
    .bind(serde_json::Value::String(stamp))
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// When the scheduler last ran, if it ever did (for the admin page).
pub async fn last_heartbeat(pool: &PgPool) -> Result<Option<OffsetDateTime>, sqlx::Error> {
    sqlx::query_scalar("SELECT updated_at FROM instance_settings WHERE key = $1")
        .bind(HEARTBEAT_KEY)
        .fetch_optional(pool)
        .await
}
