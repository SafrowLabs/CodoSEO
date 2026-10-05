//! The weekly digest job. The scheduler queues `send_digest { account_id }` at Monday 08:00 in
//! the account's time zone; this builds one email covering every monitored site of the account
//! that has a finished crawl, and sends it. An account with nothing to report gets no email.
//!
//! The changes listed are all of the site's last seven days, whether or not an instant alert
//! was also routed for them: `changes.alerted_at` records routing, not delivery, so the digest
//! never uses it to decide what to leave out (and never writes it).

use codoseo_core::check::CheckId;
use codoseo_notify::AlertItem;
use codoseo_notify::digest::{DigestView, SeverityCounts, SiteDigest};
use codoseo_store::digest::{self, SiteWeek};
use codoseo_store::{reports, sites};
use codoseo_web::rankorg;
use jiff::Span;
use jiff::tz::TimeZone;
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::jobs::JobContext;
use crate::scheduler::parse_tz;

/// How many top pages a RankOrg link carries (the same as the explorer's link).
const RANKORG_PAGES: i64 = 10;

#[derive(Deserialize)]
struct DigestPayload {
    account_id: Uuid,
}

fn db(e: impl std::fmt::Display) -> String {
    format!("database error: {e}")
}

/// The `send_digest` job handler.
pub async fn send_digest(ctx: &JobContext, payload: &serde_json::Value) -> Result<(), String> {
    let p: DigestPayload = serde_json::from_value(payload.clone())
        .map_err(|e| format!("send_digest payload is invalid: {e}"))?;
    send_digest_at(ctx, p.account_id, OffsetDateTime::now_utc())
        .await
        .map(|_| ())
}

/// Builds and sends the digest for the week ending `now`. Returns whether an email went out.
pub async fn send_digest_at(
    ctx: &JobContext,
    account_id: Uuid,
    now: OffsetDateTime,
) -> Result<bool, String> {
    let Some(view) = build_digest(ctx, account_id, now).await? else {
        return Ok(false);
    };
    let email = view
        .render()
        .map_err(|e| format!("could not render the digest: {e}"))?;
    ctx.mailer.send(email).await.map_err(|e| e.to_string())?;
    Ok(true)
}

/// The digest for the account's monitored sites, or `None` when there is nothing to send: the
/// account is gone or paused, or no monitored site has a finished crawl.
pub async fn build_digest(
    ctx: &JobContext,
    account_id: Uuid,
    now: OffsetDateTime,
) -> Result<Option<DigestView>, String> {
    let pool = &ctx.pool;
    let Some(account) = digest::account(pool, account_id).await.map_err(db)? else {
        return Ok(None);
    };
    if account.paused {
        return Ok(None);
    }
    let tz = parse_tz(&account.timezone);

    let mut digests = Vec::new();
    for site in sites::list_for_account(pool, account_id)
        .await
        .map_err(db)?
        .into_iter()
        .filter(|s| s.monitoring_active)
    {
        let Some(week) = digest::site_week(pool, site.id, now).await.map_err(db)? else {
            continue;
        };
        digests.push(site_digest(ctx, &week).await?);
    }
    if digests.is_empty() {
        return Ok(None);
    }
    Ok(Some(DigestView {
        account_email: account.email,
        week_label: week_label(&tz, now),
        sites: digests,
        settings_url: ctx
            .base_url
            .join("settings/alerts")
            .map_err(|e| format!("bad base URL: {e}"))?
            .to_string(),
    }))
}

async fn site_digest(ctx: &JobContext, week: &SiteWeek) -> Result<SiteDigest, String> {
    let dashboard_url = ctx
        .base_url
        .join(&format!("s/{}/audit", week.site_id))
        .map_err(|e| format!("bad base URL: {e}"))?
        .to_string();
    let rankorg_url = match &ctx.rankorg_url {
        Some(base) => {
            let pages =
                reports::top_pages_by_inlinks(&ctx.pool, week.latest.crawl_id, RANKORG_PAGES)
                    .await
                    .map_err(db)?;
            Some(rankorg::link(base, &week.domain, &pages, "digest").to_string())
        }
        None => None,
    };
    Ok(SiteDigest {
        domain: week.domain.clone(),
        score: u8::try_from(week.latest.score.clamp(0, 100)).unwrap_or(0),
        score_delta: week.score_delta(),
        checks_passed: u32::try_from(week.latest.checks_passed).unwrap_or(0),
        checks_total: u32::try_from(week.latest.checks_total).unwrap_or(0),
        new_issues: titled(&week.new_issues),
        resolved_issues: titled(&week.resolved_issues),
        changes_by_severity: SeverityCounts {
            critical: week.changes.critical,
            warning: week.changes.warning,
            notice: week.changes.notice,
        },
        top_changes: week
            .top_changes
            .iter()
            .map(|c| AlertItem {
                severity: c.severity,
                kind: c.kind.slug().to_owned(),
                kind_label: c.kind.label().to_owned(),
                url: c.url.clone(),
                before: c.before.clone(),
                after: c.after.clone(),
            })
            .collect(),
        dashboard_url,
        rankorg_url,
    })
}

/// `(title, pages)` for each known check, most severe first, then most pages, then by title.
/// Slugs this version doesn't know (a newer version wrote them) are left out.
fn titled(issues: &[(String, u32)]) -> Vec<(String, u32)> {
    let mut known: Vec<_> = issues
        .iter()
        .filter_map(|(slug, pages)| {
            let def = codoseo_checks::def(CheckId::from_slug(slug)?);
            Some((def.severity, def.title, *pages))
        })
        .collect();
    known.sort_by(|a, b| a.0.cmp(&b.0).then(b.2.cmp(&a.2)).then(a.1.cmp(b.1)));
    known
        .into_iter()
        .map(|(_, title, pages)| (title.to_owned(), pages))
        .collect()
}

/// "Sep 28 to Oct 5, 2026": the seven days ending on `now`'s date in the account's zone. The
/// year shows on both ends when the week crosses New Year.
pub fn week_label(tz: &TimeZone, now: OffsetDateTime) -> String {
    let end = jiff::Timestamp::from_second(now.unix_timestamp())
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .to_zoned(tz.clone())
        .date();
    let start = end.checked_sub(Span::new().days(7)).unwrap_or(end);
    if start.year() == end.year() {
        format!(
            "{} to {}",
            start.strftime("%b %-d"),
            end.strftime("%b %-d, %Y")
        )
    } else {
        format!(
            "{} to {}",
            start.strftime("%b %-d, %Y"),
            end.strftime("%b %-d, %Y")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn the_week_label_names_both_years_across_new_year() {
        let utc = TimeZone::UTC;
        assert_eq!(
            week_label(&utc, datetime!(2026-10-05 08:00 UTC)),
            "Sep 28 to Oct 5, 2026"
        );
        assert_eq!(
            week_label(&utc, datetime!(2027-01-04 08:00 UTC)),
            "Dec 28, 2026 to Jan 4, 2027"
        );
    }

    #[test]
    fn known_checks_sort_by_severity_then_pages_and_unknown_ones_are_dropped() {
        let got = titled(&[
            ("title_too_long".into(), 9),
            ("http_5xx".into(), 2),
            ("from_the_future".into(), 100),
            ("http_4xx".into(), 5),
        ]);
        assert_eq!(
            got,
            [
                ("Page returns a 4xx error".to_owned(), 5),
                ("Page returns a 5xx error".to_owned(), 2),
                ("Title is over 60 characters".to_owned(), 9),
            ]
        );
    }
}
