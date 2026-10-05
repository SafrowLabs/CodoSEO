//! The scheduler: once a minute it enqueues what is due (scheduled crawls, Monday digests, the
//! daily cleanup, inactivity emails) and records a heartbeat. It only enqueues; the crawl
//! worker and the job runner do the work. Every decision takes `now` as an argument, so tests
//! drive time directly.

use std::time::Duration;

use codoseo_core::plan::{Plan, PlanLimits, Schedule};
use codoseo_store::jobs::JobQueue;
use codoseo_store::plans;
use codoseo_store::schedule::{self, DueSite};
use codoseo_web::auth::session;
use codoseo_web::metrics;
use codoseo_web::{Config, Mode};
use jiff::civil::{Time, Weekday};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

const WEEK_SECS: i64 = 7 * 24 * 3600;
/// 1970-01-05 00:00 UTC, the first Monday after the epoch.
const FIRST_MONDAY_SECS: i64 = 4 * 24 * 3600;

/// How far into the week (from Monday 00:00 UTC) this site's weekly crawl runs: a hash of the
/// site ID, so sites spread evenly over all 7 days.
pub fn weekly_offset(site_id: Uuid) -> Duration {
    let hash = xxhash_rust::xxh3::xxh3_64(site_id.as_bytes());
    Duration::from_secs(hash % WEEK_SECS as u64)
}

/// The next weekly slot strictly after `after`: Monday 00:00 UTC plus the site's offset.
pub fn next_weekly(site_id: Uuid, after: Timestamp) -> Timestamp {
    let offset = weekly_offset(site_id).as_secs() as i64;
    let week_start = (after.as_second() - FIRST_MONDAY_SECS).div_euclid(WEEK_SECS) * WEEK_SECS
        + FIRST_MONDAY_SECS;
    let mut slot = week_start + offset;
    if Timestamp::from_second(slot).is_ok_and(|t| t <= after) {
        slot += WEEK_SECS;
    }
    Timestamp::from_second(slot).unwrap_or(Timestamp::MAX)
}

/// The local hour (0-23) a daily crawl runs at when the site has no `scheduled_hour`. Hashed
/// with a different seed than the weekly offset, so the two don't line up.
pub fn default_hour(site_id: Uuid) -> u8 {
    (xxhash_rust::xxh3::xxh3_64_with_seed(site_id.as_bytes(), 0x686f_7572) % 24) as u8
}

/// The next local `hour:00` in `tz` strictly after `after`.
pub fn next_daily(hour: u8, tz: &TimeZone, after: Timestamp) -> Timestamp {
    let at = Time::new(hour.min(23) as i8, 0, 0, 0).unwrap_or(Time::midnight());
    next_daily_at(at, tz, after)
}

/// The next local `at` time in `tz` strictly after `after`. A time that doesn't exist that day
/// (the hour skipped by a spring-forward change) resolves forward, so 02:30 becomes 03:30.
pub fn next_daily_at(at: Time, tz: &TimeZone, after: Timestamp) -> Timestamp {
    let today = after.to_zoned(tz.clone()).date();
    // Today's slot may already have passed; tomorrow's always lies ahead. The extra day covers
    // a slot that a zone change moved back before `after`.
    for days in 0..=2 {
        let Ok(date) = today.checked_add(jiff::Span::new().days(days)) else {
            break;
        };
        let Ok(zoned) = tz.to_ambiguous_zoned(date.to_datetime(at)).compatible() else {
            continue;
        };
        if zoned.timestamp() > after {
            return zoned.timestamp();
        }
    }
    after + SignedDuration::from_hours(24)
}

/// An IANA zone name from `accounts.timezone`; anything unknown is UTC.
pub fn parse_tz(name: &str) -> TimeZone {
    TimeZone::get(name.trim()).unwrap_or(TimeZone::UTC)
}

/// Whether it is Monday 08:00 or later in `tz` and no digest has gone out since this Monday's
/// 08:00 local.
pub fn next_digest_due(tz: &TimeZone, last_digest_at: Option<Timestamp>, now: Timestamp) -> bool {
    let local = now.to_zoned(tz.clone());
    if local.weekday() != Weekday::Monday || local.hour() < 8 {
        return false;
    }
    let Ok(start) = tz
        .to_ambiguous_zoned(local.date().at(8, 0, 0, 0))
        .compatible()
    else {
        return false;
    };
    last_digest_at.is_none_or(|last| last < start.timestamp())
}

/// How often the scheduler loop ticks.
pub const TICK_INTERVAL: Duration = Duration::from_secs(60);
/// A `running` job claimed longer ago than this belongs to a worker that died.
const STALE_JOB_AFTER: Duration = Duration::from_secs(600);
/// Due sites claimed per transaction.
const DUE_BATCH: i64 = 100;
/// Free-plan crawls queue behind paid ones: scheduled lanes 3 (paid) and 5 (free).
const PAID_SCHEDULED_PRIORITY: i16 = 3;
const FREE_SCHEDULED_PRIORITY: i16 = 5;

/// Everything a tick needs besides the time.
#[derive(Clone)]
pub struct SchedulerContext {
    pub pool: PgPool,
    /// Inactivity pausing only applies to the cloud.
    pub mode: Mode,
    /// For the link in the "Keep monitoring?" email.
    pub base_url: Url,
    /// `SCHEDULER_HEARTBEAT_URL`: pinged with a GET after each tick (a dead-man's switch).
    pub heartbeat_url: Option<Url>,
    pub http: reqwest::Client,
}

impl SchedulerContext {
    /// The context for a process configured by `config`; `SCHEDULER_HEARTBEAT_URL` is read from
    /// the environment (an invalid value is logged and ignored).
    pub fn from_config(pool: PgPool, config: &Config) -> SchedulerContext {
        let heartbeat_url = std::env::var("SCHEDULER_HEARTBEAT_URL")
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .and_then(|v| match Url::parse(&v) {
                Ok(url) => Some(url),
                Err(e) => {
                    tracing::warn!(error = %e, "SCHEDULER_HEARTBEAT_URL is not a URL; ignoring it");
                    None
                }
            });
        SchedulerContext {
            pool,
            mode: config.mode,
            base_url: config.base_url.clone(),
            heartbeat_url,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .user_agent(concat!("CodoSEO/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("http client builds"),
        }
    }
}

/// `CODOSEO_SCHEDULER=off` turns the scheduler off (for a second web container).
pub fn enabled() -> bool {
    enabled_by(std::env::var("CODOSEO_SCHEDULER").ok().as_deref())
}

fn enabled_by(value: Option<&str>) -> bool {
    !value.is_some_and(|v| v.trim().eq_ignore_ascii_case("off"))
}

/// Starts [`scheduler_loop`] for a web process, unless `CODOSEO_SCHEDULER=off`.
pub fn spawn(
    pool: &PgPool,
    config: &Config,
    shutdown: &CancellationToken,
) -> Option<tokio::task::JoinHandle<()>> {
    if !enabled() {
        tracing::info!("CODOSEO_SCHEDULER=off: this process does not run the scheduler");
        return None;
    }
    let ctx = SchedulerContext::from_config(pool.clone(), config);
    Some(tokio::spawn(scheduler_loop(ctx, shutdown.clone())))
}

/// Ticks every [`TICK_INTERVAL`] until `shutdown` is cancelled. A slow tick delays the next one
/// instead of piling up.
pub async fn scheduler_loop(ctx: SchedulerContext, shutdown: CancellationToken) {
    let mut interval = tokio::time::interval(TICK_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = shutdown.cancelled() => return,
        }
        let report = tick(&ctx, Timestamp::now()).await;
        tracing::debug!(?report, "scheduler tick");
    }
}

/// What one tick did, for logs and tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    /// Stuck `running` jobs put back in the queue.
    pub jobs_requeued: u64,
    /// `schedule` crawls queued.
    pub crawls_queued: u64,
    /// Sites given their first slot without a crawl.
    pub sites_initialised: u64,
    /// Accounts moved to Free because their plan ran out.
    pub downgraded: u64,
    pub digests_queued: u64,
    pub cleanup_queued: bool,
    /// "Keep monitoring?" emails queued.
    pub warned: u64,
    /// Warnings whose email failed for good, cleared so a new one goes out.
    pub warnings_reset: u64,
    pub paused: u64,
    pub active_events: u64,
    /// Steps that failed, by name. The other steps still ran.
    pub failures: Vec<(&'static str, String)>,
}

/// Runs every scheduler step once at `now`. A failing step is recorded and logged; the rest
/// still run, and the heartbeat is only written (and pinged) if the database took it.
pub async fn tick(ctx: &SchedulerContext, now: Timestamp) -> TickReport {
    let mut report = TickReport::default();
    let at = to_odt(now);

    match JobQueue::new(ctx.pool.clone())
        .requeue_stale(STALE_JOB_AFTER)
        .await
    {
        Ok(n) => report.jobs_requeued = n,
        Err(e) => report.fail("requeue_stale_jobs", e),
    }
    match schedule_due_sites(&ctx.pool, now).await {
        Ok((queued, initialised)) => {
            report.crawls_queued = queued;
            report.sites_initialised = initialised;
        }
        Err(e) => report.fail("schedule_sites", e),
    }
    match plans::downgrade_expired(&ctx.pool, at).await {
        Ok(n) => report.downgraded = n,
        Err(e) => report.fail("downgrade_expired", e),
    }
    match queue_digests(&ctx.pool, now).await {
        Ok(n) => report.digests_queued = n,
        Err(e) => report.fail("digests", e),
    }
    let today = now.to_zoned(TimeZone::UTC).date().to_string();
    match schedule::enqueue_daily_cleanup(&ctx.pool, &today, at).await {
        Ok(queued) => report.cleanup_queued = queued,
        Err(e) => report.fail("cleanup", e),
    }
    if ctx.mode == Mode::Cloud {
        match schedule::reset_failed_warnings(&ctx.pool).await {
            Ok(n) => report.warnings_reset = n,
            Err(e) => report.fail("reset_failed_warnings", e),
        }
        match warn_inactive(ctx, at).await {
            Ok(n) => report.warned = n,
            Err(e) => report.fail("inactivity_warnings", e),
        }
        match schedule::pause_unresponsive(&ctx.pool, at).await {
            Ok(n) => report.paused = n,
            Err(e) => report.fail("inactivity_pause", e),
        }
    }
    match schedule::record_active_after_4_weeks(&ctx.pool, at).await {
        Ok(n) => report.active_events = n,
        Err(e) => report.fail("active_after_4_weeks", e),
    }
    match schedule::record_heartbeat(&ctx.pool, at).await {
        Ok(()) => ping_heartbeat(ctx).await,
        Err(e) => report.fail("heartbeat", e),
    }
    metrics::scheduler_tick(unix_now());
    report
}

impl TickReport {
    fn fail(&mut self, step: &'static str, error: impl std::fmt::Display) {
        tracing::warn!(step, %error, "scheduler step failed");
        self.failures.push((step, error.to_string()));
    }
}

/// Now as Unix seconds, for the last-tick gauge.
fn unix_now() -> f64 {
    Timestamp::now().as_millisecond() as f64 / 1000.0
}

fn to_odt(ts: Timestamp) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(ts.as_nanosecond())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

fn to_timestamp(t: OffsetDateTime) -> Timestamp {
    Timestamp::from_nanosecond(t.unix_timestamp_nanos()).unwrap_or(Timestamp::UNIX_EPOCH)
}

/// The priority lane of a scheduled crawl. Self-hosted counts as paid.
pub fn crawl_priority(plan: Plan) -> i16 {
    match plan {
        Plan::Free => FREE_SCHEDULED_PRIORITY,
        Plan::Pro | Plan::Agency | Plan::SelfHosted => PAID_SCHEDULED_PRIORITY,
    }
}

/// The schedule a site really runs at: what it asked for, capped by what its plan allows.
fn effective_schedule(site: &DueSite) -> Schedule {
    let allowed = PlanLimits::for_plan(site.plan)
        .fastest_schedule
        .unwrap_or(Schedule::Weekly);
    match (site.schedule, allowed) {
        (Some(Schedule::Daily), Schedule::Daily) => Schedule::Daily,
        _ => Schedule::Weekly,
    }
}

/// The slot after `now` for a site on `schedule`. Sites are never back-filled: a site that was
/// due three weeks ago gets one crawl now and the next slot ahead of `now`.
fn next_slot(site: &DueSite, schedule: Schedule, now: Timestamp) -> Timestamp {
    match schedule {
        Schedule::Weekly => next_weekly(site.id, now),
        Schedule::Daily => {
            let hour = site.scheduled_hour.unwrap_or_else(|| default_hour(site.id));
            next_daily(hour, &parse_tz(&site.timezone), now)
        }
    }
}

/// Queues a `schedule` crawl for each due site that has none waiting, and moves every due site
/// to its next slot. Returns (crawls queued, sites given their first slot).
async fn schedule_due_sites(pool: &PgPool, now: Timestamp) -> Result<(u64, u64), sqlx::Error> {
    let at = to_odt(now);
    let (mut queued, mut initialised) = (0, 0);
    loop {
        let mut batch = schedule::claim_due_sites(pool, at, DUE_BATCH).await?;
        let sites = std::mem::take(&mut batch.sites);
        for site in &sites {
            let next = to_odt(next_slot(site, effective_schedule(site), now));
            // A site that has never had a slot just had its first crawl; it only needs one.
            let priority = site.next_crawl_at.map(|_| crawl_priority(site.plan));
            if priority.is_none() {
                initialised += 1;
            }
            if batch.advance(site, priority, next).await? {
                queued += 1;
            }
        }
        batch.commit().await?;
        if (sites.len() as i64) < DUE_BATCH {
            return Ok((queued, initialised));
        }
    }
}

/// Queues `send_digest` for each account whose Monday 08:00 (local) has come and gone without
/// one. Returns how many were queued.
async fn queue_digests(pool: &PgPool, now: Timestamp) -> Result<u64, sqlx::Error> {
    let at = to_odt(now);
    let mut queued = 0;
    for candidate in schedule::digest_candidates(pool, at).await? {
        let tz = parse_tz(&candidate.timezone);
        let last = candidate.last_digest_at.map(to_timestamp);
        if next_digest_due(&tz, last, now) && schedule::enqueue_digest(pool, &candidate, at).await?
        {
            queued += 1;
        }
    }
    Ok(queued)
}

/// Emails the "Keep monitoring?" link to Free accounts that have been silent for 30 days.
async fn warn_inactive(ctx: &SchedulerContext, now: OffsetDateTime) -> Result<u64, sqlx::Error> {
    let mut warned = 0;
    for account in schedule::accounts_to_warn(&ctx.pool, now, DUE_BATCH).await? {
        let token = session::random_token();
        let mut link = ctx.base_url.clone();
        link.set_path(&format!("/monitoring/resume/{token}"));
        let email = serde_json::json!({
            "to": account.email,
            "subject": "Keep monitoring your site on CodoSEO?",
            "text": format!(
                "We haven't seen you on CodoSEO for 30 days.\n\n\
                 Do you want us to keep monitoring your site? Open this link and we will:\n\n\
                 {link}\n\n\
                 If we don't hear from you within 7 days we'll pause monitoring. Nothing is \
                 deleted, and signing in again turns it back on.\n\n\
                 The link works once and expires in 7 days."
            ),
        });
        if schedule::send_keep_monitoring(&ctx.pool, account.id, now, &session::hash(&token), email)
            .await?
        {
            warned += 1;
        }
    }
    Ok(warned)
}

/// Pings `SCHEDULER_HEARTBEAT_URL`, if set. A failure is logged and nothing more.
async fn ping_heartbeat(ctx: &SchedulerContext) {
    let Some(url) = &ctx.heartbeat_url else {
        return;
    };
    match ctx.http.get(url.clone()).send().await {
        Ok(res) if res.status().is_success() => {}
        Ok(res) => {
            tracing::warn!(status = %res.status(), "scheduler heartbeat URL answered with an error")
        }
        Err(e) => tracing::warn!(error = %e, "could not reach the scheduler heartbeat URL"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_off_disables_the_scheduler() {
        assert!(enabled_by(None));
        assert!(enabled_by(Some("on")));
        assert!(enabled_by(Some("")));
        assert!(!enabled_by(Some("off")));
        assert!(!enabled_by(Some(" OFF ")));
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    /// A fixed pseudo-random sequence of IDs, so the spread test is the same on every run.
    fn ids(n: u64) -> impl Iterator<Item = Uuid> {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        (0..n).map(move |_| {
            let mut next = || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            };
            Uuid::from_u64_pair(next(), next())
        })
    }

    #[test]
    fn weekly_offsets_spread_evenly_over_the_seven_days() {
        let mut days = [0u32; 7];
        for id in ids(10_000) {
            let offset = weekly_offset(id).as_secs();
            assert!(offset < WEEK_SECS as u64);
            days[(offset / 86_400) as usize] += 1;
        }
        let expected = 10_000.0 / 7.0;
        for (day, count) in days.iter().enumerate() {
            let off = (f64::from(*count) - expected).abs() / expected;
            assert!(
                off < 0.05,
                "day {day} has {count} of 10000 sites ({off:.3} off)"
            );
        }
    }

    #[test]
    fn the_offset_of_a_site_never_changes() {
        let id = Uuid::from_u128(42);
        assert_eq!(weekly_offset(id), weekly_offset(id));
    }

    #[test]
    fn next_weekly_is_strictly_after_and_a_week_apart() {
        let id = Uuid::from_u128(7);
        let offset = weekly_offset(id).as_secs() as i64;
        let after = ts("2026-10-05T12:00:00Z"); // a Monday
        let first = next_weekly(id, after);
        assert!(first > after);
        assert!(first <= after + SignedDuration::from_secs(WEEK_SECS));
        // Monday 00:00 UTC plus the offset.
        assert_eq!(
            (first.as_second() - FIRST_MONDAY_SECS - offset) % WEEK_SECS,
            0
        );
        let second = next_weekly(id, first);
        assert_eq!(second.as_second() - first.as_second(), WEEK_SECS);
        // Exactly on a slot: the next one is a week later, not the same instant.
        assert_eq!(next_weekly(id, first), second);
        // One second before a slot: that slot.
        assert_eq!(next_weekly(id, first - SignedDuration::from_secs(1)), first);
    }

    #[test]
    fn next_weekly_stays_in_the_same_week_until_its_offset_has_passed() {
        // A site whose slot is late in the week: asked on the Sunday before it, it answers with
        // that Sunday, and asked just after it, with the following week's.
        let id = ids(1000)
            .find(|id| weekly_offset(*id).as_secs() > 6 * 86_400 + 3_600)
            .unwrap();
        let monday = ts("2026-10-05T00:00:00Z");
        let slot = monday + SignedDuration::from_secs(weekly_offset(id).as_secs() as i64);
        assert_eq!(next_weekly(id, ts("2026-10-11T00:30:00Z")), slot);
        assert_eq!(
            next_weekly(id, slot),
            slot + SignedDuration::from_secs(WEEK_SECS)
        );
    }

    #[test]
    fn default_hours_are_valid_and_spread() {
        let mut hours = [0u32; 24];
        for id in ids(2400) {
            let h = default_hour(id);
            assert!(h < 24);
            hours[h as usize] += 1;
        }
        assert!(hours.iter().all(|c| *c > 50), "{hours:?}");
    }

    #[test]
    fn a_daily_crawl_stays_at_six_local_across_both_dst_changes() {
        let berlin = parse_tz("Europe/Berlin");
        // Spring forward: Sunday 2026-03-29 (CET +1 to CEST +2). 06:00 local moves from 05:00Z
        // to 04:00Z.
        let sat = next_daily(6, &berlin, ts("2026-03-27T12:00:00Z"));
        assert_eq!(sat, ts("2026-03-28T05:00:00Z")); // 06:00 CET
        let sun = next_daily(6, &berlin, sat);
        assert_eq!(sun, ts("2026-03-29T04:00:00Z")); // 06:00 CEST
        assert_eq!(next_daily(6, &berlin, sun), ts("2026-03-30T04:00:00Z"));
        // Fall back: Sunday 2026-10-25 (CEST +2 to CET +1). 06:00 local moves from 04:00Z to
        // 05:00Z.
        let sat = next_daily(6, &berlin, ts("2026-10-23T12:00:00Z"));
        assert_eq!(sat, ts("2026-10-24T04:00:00Z")); // 06:00 CEST
        let sun = next_daily(6, &berlin, sat);
        assert_eq!(sun, ts("2026-10-25T05:00:00Z")); // 06:00 CET
        assert_eq!(next_daily(6, &berlin, sun), ts("2026-10-26T05:00:00Z"));
        for slot in [sat, sun] {
            assert_eq!(slot.to_zoned(berlin.clone()).hour(), 6);
        }
    }

    #[test]
    fn next_daily_is_strictly_after() {
        let utc = TimeZone::UTC;
        let slot = ts("2026-10-05T06:00:00Z");
        assert_eq!(next_daily(6, &utc, slot), ts("2026-10-06T06:00:00Z"));
        assert_eq!(
            next_daily(6, &utc, slot - SignedDuration::from_secs(1)),
            slot
        );
    }

    #[test]
    fn a_time_skipped_by_spring_forward_runs_at_the_next_valid_instant() {
        let berlin = parse_tz("Europe/Berlin");
        // 02:30 does not exist on 2026-03-29 in Berlin; it resolves to 03:30 CEST.
        let at = Time::new(2, 30, 0, 0).unwrap();
        let slot = next_daily_at(at, &berlin, ts("2026-03-28T12:00:00Z"));
        assert_eq!(slot, ts("2026-03-29T01:30:00Z")); // 03:30 CEST
        assert_eq!(slot.to_zoned(berlin.clone()).hour(), 3);
        // The hour-only form lands on 03:00 that day (02:00 is skipped too), never skipped.
        let slot = next_daily(2, &berlin, ts("2026-03-28T12:00:00Z"));
        assert_eq!(slot, ts("2026-03-29T01:00:00Z"));
        // The day after it is 02:30 local again.
        let slot = ts("2026-03-29T01:30:00Z");
        let next = next_daily_at(at, &berlin, slot);
        assert_eq!(next, ts("2026-03-30T00:30:00Z"));
    }

    #[test]
    fn an_unknown_time_zone_is_utc() {
        assert_eq!(parse_tz("Mars/Olympus"), TimeZone::UTC);
        assert_eq!(parse_tz(""), TimeZone::UTC);
        assert_eq!(parse_tz("Asia/Kolkata").iana_name(), Some("Asia/Kolkata"));
        let slot = next_daily(6, &parse_tz("Mars/Olympus"), ts("2026-10-05T07:00:00Z"));
        assert_eq!(slot, ts("2026-10-06T06:00:00Z"));
    }

    #[test]
    fn the_digest_is_due_on_monday_from_eight_local_until_one_goes_out() {
        let kolkata = parse_tz("Asia/Kolkata"); // +05:30
        // Monday 2026-10-05 08:05 IST = 02:35 UTC.
        let monday_0805 = ts("2026-10-05T02:35:00Z");
        assert!(next_digest_due(&kolkata, None, monday_0805));
        // Not yet 08:00 local.
        assert!(!next_digest_due(&kolkata, None, ts("2026-10-05T02:25:00Z")));
        // Sent at 08:05: not due again at 09:05, nor later that Monday.
        assert!(!next_digest_due(
            &kolkata,
            Some(monday_0805),
            ts("2026-10-05T03:35:00Z")
        ));
        assert!(!next_digest_due(
            &kolkata,
            Some(monday_0805),
            ts("2026-10-05T18:00:00Z")
        ));
        // Tuesday is never a digest day, even with no digest sent.
        assert!(!next_digest_due(&kolkata, None, ts("2026-10-06T10:00:00Z")));
        // Last week's digest does not count.
        assert!(next_digest_due(
            &kolkata,
            Some(ts("2026-09-28T02:40:00Z")),
            monday_0805
        ));
        // West of UTC: 08:00 PDT is 15:00 UTC.
        let la = parse_tz("America/Los_Angeles");
        assert!(next_digest_due(&la, None, ts("2026-10-05T15:00:00Z"))); // 08:00 PDT
        assert!(!next_digest_due(&la, None, ts("2026-10-05T14:59:00Z")));
    }
}
