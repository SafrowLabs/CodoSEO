//! `/s/{site}/crawls`: the crawl history and Run crawl, which enforces the plan's per-site
//! manual allowance and the one-crawl-at-a-time rule; and `/s/{site}/status`, the sidebar
//! crawler card's poll, which announces a crawl that just finished.

use std::fmt::Write as _;

use askama::Template;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use codoseo_core::output::StopReason;
use codoseo_core::plan::{ManualAllowance, Plan, PlanLimits};
use codoseo_store::crawl_queue::CrawlTrigger;
use codoseo_store::crawls::{Crawl, CrawlStatus, ManualOutcome, ManualWindow};
use codoseo_store::sites::Site;
use serde::Deserialize;
use serde_json::json;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::auth::{CurrentUser, load_site};
use crate::error::AppError;
use crate::fmt;
use crate::layout::{CrawlerView, Screen, Shell, crawler_for};
use crate::render::{Hx, html};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/s/{site}/crawls", get(history).post(run))
        .route("/s/{site}/status", get(status))
}

/// Priority lane for a manual crawl on a paid plan (spec section 10).
pub const PAID_MANUAL_PRIORITY: i16 = 2;
/// Priority lane for a manual crawl on Free (spec section 10).
pub const FREE_MANUAL_PRIORITY: i16 = 4;

/// How many crawls the history shows.
const HISTORY_LIMIT: i64 = 100;

/// Grid columns shared by the history header and rows.
const COLS: &str = "52px minmax(190px, 2fr) minmax(96px, 1fr) 76px 84px 84px 92px 16px";

/// The queue lane for a manual crawl on `plan`.
pub fn manual_priority(plan: Plan) -> i16 {
    match plan {
        Plan::Free => FREE_MANUAL_PRIORITY,
        Plan::Pro | Plan::Agency | Plan::SelfHosted => PAID_MANUAL_PRIORITY,
    }
}

/// One row of the history.
pub struct CrawlRow {
    /// `a` for a finished crawl (it links to the audit), `div` otherwise.
    pub tag: &'static str,
    pub href: Option<String>,
    /// `#48`
    pub number: String,
    /// `queued`, `running`, `done` or `failed`: the dot and badge tone.
    pub state: &'static str,
    pub badge_class: &'static str,
    pub label: &'static str,
    /// Next to the badge: pages so far, the failure reason, why a crawl stopped early.
    pub detail: Option<String>,
    /// The full text behind `detail`, as a native tooltip.
    pub detail_title: Option<String>,
    pub detail_class: &'static str,
    pub trigger: &'static str,
    /// `86` and its tone (`c-ok`, `c-warn`, `c-err`).
    pub health: Option<(String, &'static str)>,
    pub pages: String,
    pub duration: String,
    /// `2h ago`
    pub when: String,
    /// `Finished Oct 3, 14:05 UTC`
    pub when_full: String,
}

#[derive(Template)]
#[template(path = "crawls/index.html")]
pub struct CrawlsPage {
    pub shell: Shell,
    pub base: String,
    pub total: String,
    pub allowance: Option<String>,
    pub list: CrawlList,
}

/// The history card. It refreshes itself when a crawl is queued or finishes, and polls every
/// 3 s while a crawl is queued or running.
#[derive(Template)]
#[template(path = "crawls/list.html")]
pub struct CrawlList {
    pub base: String,
    pub domain: String,
    pub cols: &'static str,
    pub rows: Vec<CrawlRow>,
    pub live: bool,
    /// Set on a fragment response: the page-head chip and allowance line, swapped out of band.
    pub oob: Option<(String, Option<String>)>,
}

#[derive(Template)]
#[template(path = "partials/crawler_partial.html")]
pub struct CrawlerPartial {
    pub crawler: CrawlerView,
}

/// `/s/{site}/…` with something that isn't a site ID looks like any other missing page.
fn parse_site_id(raw: &str) -> Result<Uuid, AppError> {
    raw.parse().map_err(|_| AppError::NotFound)
}

async fn history(
    State(state): State<AppState>,
    user: CurrentUser,
    hx: Hx,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, parse_site_id(&site)?).await?;
    let crawls = codoseo_store::crawls::history(&state.pool, site.id, HISTORY_LIMIT).await?;
    let now = OffsetDateTime::now_utc();
    let base = format!("/s/{}", site.id);
    // `number` counts every crawl ever queued, so the newest one's is the total.
    let total = match crawls.first().map_or(0, |c| c.number) {
        1 => "1 crawl".to_owned(),
        n => format!("{} crawls", fmt::thousands(n)),
    };
    let allowance = allowance_note(&state, &site, user.account.plan, now).await?;
    let mut list = CrawlList {
        base: base.clone(),
        domain: site.domain.clone(),
        cols: COLS,
        live: crawls.iter().any(Crawl::is_active),
        rows: crawls.iter().map(|c| row(&base, c, now)).collect(),
        oob: None,
    };
    if hx.partial() {
        list.oob = Some((total, allowance));
        return Ok(html(&list)?.into_response());
    }
    let shell = Shell::load(&state, &user, Some(&site), Screen::Crawls).await?;
    Ok(html(&CrawlsPage {
        shell,
        base,
        total,
        allowance,
        list,
    })?
    .into_response())
}

/// Run crawl. The header button posts here with htmx (`hx-swap="none"`): success is a `204`
/// whose `HX-Trigger` shows a toast and tells the page a crawl was queued.
async fn run(
    State(state): State<AppState>,
    user: CurrentUser,
    hx: Hx,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, parse_site_id(&site)?).await?;
    let plan = user.account.plan;
    let allowance = PlanLimits::for_plan(plan).manual_crawls;
    let outcome = codoseo_store::crawls::enqueue_manual_checked(
        &state.pool,
        site.id,
        &site.domain,
        manual_priority(plan),
        ManualWindow::for_allowance(allowance),
    )
    .await?;
    let number = match outcome {
        ManualOutcome::Queued { number, .. } => number,
        ManualOutcome::Busy(status) => {
            let what = if status == CrawlStatus::Running {
                "running"
            } else {
                "queued"
            };
            return Err(AppError::Conflict(format!(
                "A crawl is already {what} for this site."
            )));
        }
        ManualOutcome::LimitReached { frees_at } => {
            return Err(AppError::Limit(limit_message(
                plan,
                allowance,
                frees_at - OffsetDateTime::now_utc(),
            )));
        }
    };
    if !hx.partial() {
        return Ok(Redirect::to(&format!("/s/{}/crawls", site.id)).into_response());
    }
    let trigger = json!({
        "toast": { "kind": "ok", "message": format!("Crawl #{number} queued") },
        "crawlQueued": true,
    });
    Ok((StatusCode::NO_CONTENT, [hx_trigger(&trigger)]).into_response())
}

#[derive(Deserialize)]
struct StatusQuery {
    /// The card's state when it last rendered.
    was: Option<String>,
}

/// The sidebar crawler card, polled every 2 s while a crawl is queued or running. When the
/// card was active and the crawl is now over, the response also carries `crawlFinished` (the
/// page reloads in place) and a toast saying how it went.
async fn status(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, parse_site_id(&site)?).await?;
    let crawler = crawler_for(&state, &site, user.account.plan).await?;
    let was_active = matches!(q.was.as_deref(), Some("queued" | "running"));
    let now_active = matches!(crawler.state, "queued" | "running");
    let mut res = html(&CrawlerPartial { crawler })?.into_response();
    if was_active && !now_active {
        let latest = codoseo_store::crawls::history(&state.pool, site.id, 1).await?;
        if let Some(c) = latest.first() {
            let trigger = json!({
                "crawlFinished": true,
                "toast": finished_toast(c),
            });
            let (name, value) = hx_trigger(&trigger);
            res.headers_mut().insert(name, value);
        }
    }
    Ok(res)
}

/// An `HX-Trigger` header carrying several events at once (`render::toast` carries only one).
fn hx_trigger(events: &serde_json::Value) -> (HeaderName, HeaderValue) {
    (
        HeaderName::from_static("hx-trigger"),
        HeaderValue::from_str(&ascii_json(events))
            .unwrap_or_else(|_| HeaderValue::from_static("{}")),
    )
}

/// JSON with every non-ASCII character written as a JSON escape (`·` as U+00B7's). Browsers
/// read header values as Latin-1, so raw UTF-8 in a toast would arrive garbled.
fn ascii_json(v: &serde_json::Value) -> String {
    let raw = v.to_string();
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                let _ = write!(out, "\\u{unit:04x}");
            }
        }
    }
    out
}

/// The toast for a crawl that just ended: `Crawl #49 finished · health 86/100`, or
/// `Crawl #49 failed: <reason>`.
fn finished_toast(c: &Crawl) -> serde_json::Value {
    if c.status == CrawlStatus::Failed {
        let reason = c.failure_reason.as_deref().unwrap_or("unknown error");
        return json!({ "kind": "error", "message": format!("Crawl #{} failed: {reason}", c.number) });
    }
    let message = match c.health_score {
        Some(h) => format!("Crawl #{} finished · health {h}/100", c.number),
        None => format!("Crawl #{} finished", c.number),
    };
    json!({ "kind": "ok", "message": message })
}

fn plan_name(plan: Plan) -> &'static str {
    match plan {
        Plan::Free => "Free",
        Plan::Pro => "Pro",
        Plan::Agency => "Agency",
        Plan::SelfHosted => "self-hosted",
    }
}

/// `1 manual crawl a week`, `3 manual crawls a day`; `None` when unlimited.
fn allowance_phrase(allowance: ManualAllowance) -> Option<String> {
    let (n, per) = match allowance {
        ManualAllowance::PerWeek(n) => (n, "week"),
        ManualAllowance::PerDay(n) => (n, "day"),
        ManualAllowance::Unlimited => return None,
    };
    let s = if n == 1 { "" } else { "s" };
    Some(format!("{n} manual crawl{s} a {per}"))
}

/// The refusal when the allowance is used up, e.g. "Your Free plan includes 1 manual crawl a
/// week. The next one is available in 3 days."
fn limit_message(plan: Plan, allowance: ManualAllowance, wait: Duration) -> String {
    let phrase = allowance_phrase(allowance).unwrap_or_else(|| "manual crawls".to_owned());
    format!(
        "Your {} plan includes {phrase}. The next one is available {}.",
        plan_name(plan),
        until(wait)
    )
}

/// `in a minute`, `in 12 minutes`, `in 5 hours`, `in 3 days` (rounded up below a day, to the
/// nearest day above).
fn until(wait: Duration) -> String {
    let minutes = (wait.whole_seconds().max(0) + 59) / 60;
    if minutes <= 1 {
        return "in a minute".to_owned();
    }
    if minutes < 60 {
        return format!("in {minutes} minutes");
    }
    let hours = (minutes + 59) / 60;
    if hours < 24 {
        let s = if hours == 1 { "" } else { "s" };
        return format!("in {hours} hour{s}");
    }
    let days = (hours + 12) / 24;
    let s = if days == 1 { "" } else { "s" };
    format!("in {days} day{s}")
}

/// The line under the page title on plans with a manual allowance: `Free plan: 1 manual crawl
/// a week · next one available in 3 days`.
async fn allowance_note(
    state: &AppState,
    site: &Site,
    plan: Plan,
    now: OffsetDateTime,
) -> Result<Option<String>, AppError> {
    let allowance = PlanLimits::for_plan(plan).manual_crawls;
    let (Some(window), Some(phrase)) = (
        ManualWindow::for_allowance(allowance),
        allowance_phrase(allowance),
    ) else {
        return Ok(None);
    };
    let since = now - window.window;
    let used = codoseo_store::crawls::manual_count_since(&state.pool, site.id, since).await?;
    let when = if used < window.max {
        "available now".to_owned()
    } else {
        let oldest = codoseo_store::crawls::oldest_manual_since(&state.pool, site.id, since)
            .await?
            .unwrap_or(now);
        format!("next one available {}", until(oldest + window.window - now))
    };
    Ok(Some(format!("{} plan: {phrase} · {when}", plan_name(plan))))
}

fn trigger_label(t: CrawlTrigger) -> &'static str {
    match t {
        CrawlTrigger::First => "First crawl",
        CrawlTrigger::Manual => "Manual",
        CrawlTrigger::Schedule => "Scheduled",
        CrawlTrigger::Quick => "Quick audit",
    }
}

fn health_tone(score: i16) -> &'static str {
    match score {
        s if s >= 90 => "c-ok",
        s if s >= 70 => "c-warn",
        _ => "c-err",
    }
}

/// Why a finished crawl stopped short of the whole site, if it did.
fn stop_note(stop: &StopReason) -> Option<String> {
    match stop {
        StopReason::Completed => None,
        StopReason::PageLimit => Some("page limit reached".to_owned()),
        StopReason::TimeLimit => Some("time limit reached".to_owned()),
        StopReason::RobotsBlocked => Some("blocked by robots.txt".to_owned()),
        StopReason::Unreachable(r) | StopReason::Blocked(r) => Some(r.clone()),
    }
}

fn row(base: &str, c: &Crawl, now: OffsetDateTime) -> CrawlRow {
    let progress = c.progress();
    let summary = c.summary();
    let dash = || "—".to_owned();
    let pages_done = progress.as_ref().map(|p| p.pages_done);

    let (state, badge_class, label, detail, detail_class) = match c.status {
        CrawlStatus::Queued => {
            // A first attempt that failed is requeued 15 minutes out with its reason kept.
            let retry = c.failure_reason.is_some() && c.queued_at > now;
            let detail = retry.then(|| format!("retry {}", until(c.queued_at - now)));
            ("queued", "t-warn", "Queued", detail, "faint")
        }
        CrawlStatus::Running => {
            let done = pages_done.unwrap_or(0);
            let s = if done == 1 { "" } else { "s" };
            let detail = Some(format!("{} page{s} so far", fmt::thousands(done)));
            ("running", "t-muted", "Crawling", detail, "faint")
        }
        CrawlStatus::Done => {
            let detail = summary.as_ref().and_then(|s| stop_note(&s.stop_reason));
            ("done", "t-ok", "Done", detail, "faint")
        }
        CrawlStatus::Failed => {
            let detail = Some(
                c.failure_reason
                    .clone()
                    .unwrap_or_else(|| "unknown error".to_owned()),
            );
            ("failed", "t-err", "Failed", detail, "c-err")
        }
    };
    let detail_title = match c.status {
        CrawlStatus::Queued if detail.is_some() => c
            .failure_reason
            .as_ref()
            .map(|r| format!("The last attempt failed: {r}")),
        _ => detail.clone(),
    };

    let pages = match (c.status, &summary, pages_done) {
        (CrawlStatus::Done, Some(s), _) => fmt::thousands(s.report_summary.pages),
        (CrawlStatus::Running | CrawlStatus::Failed, _, Some(n)) => fmt::thousands(n),
        _ => dash(),
    };
    let duration = match c.status {
        CrawlStatus::Running => progress
            .as_ref()
            .map(|p| fmt::millis(p.elapsed_ms))
            .or_else(|| c.started_at.map(|s| fmt::duration(now - s)))
            .unwrap_or_else(dash),
        _ => c.duration().map(fmt::duration).unwrap_or_else(dash),
    };
    let (verb, at) = match (c.finished_at, c.started_at) {
        (Some(f), _) => ("Finished", f),
        (None, Some(s)) => ("Started", s),
        (None, None) => ("Queued", c.queued_at),
    };
    let href = (c.status == CrawlStatus::Done).then(|| format!("{base}/audit"));

    CrawlRow {
        tag: if href.is_some() { "a" } else { "div" },
        href,
        number: format!("#{}", c.number),
        state,
        badge_class,
        label,
        detail,
        detail_title,
        detail_class,
        trigger: trigger_label(c.trigger),
        health: c
            .health_score
            .filter(|_| c.status == CrawlStatus::Done)
            .map(|h| (h.to_string(), health_tone(h))),
        pages,
        duration,
        when: fmt::ago(at),
        when_full: format!("{verb} {}", fmt::datetime(at)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_lanes() {
        assert_eq!(manual_priority(Plan::Free), 4);
        assert_eq!(manual_priority(Plan::Pro), 2);
        assert_eq!(manual_priority(Plan::Agency), 2);
        assert_eq!(manual_priority(Plan::SelfHosted), 2);
    }

    #[test]
    fn waits() {
        assert_eq!(until(Duration::seconds(-5)), "in a minute");
        assert_eq!(until(Duration::seconds(50)), "in a minute");
        assert_eq!(until(Duration::minutes(12)), "in 12 minutes");
        assert_eq!(
            until(Duration::minutes(59) + Duration::seconds(59)),
            "in 1 hour"
        );
        assert_eq!(
            until(Duration::hours(4) + Duration::minutes(10)),
            "in 5 hours"
        );
        assert_eq!(until(Duration::hours(25)), "in 1 day");
        assert_eq!(until(Duration::days(3) - Duration::seconds(1)), "in 3 days");
        assert_eq!(until(Duration::days(6) + Duration::hours(23)), "in 7 days");
    }

    #[test]
    fn limit_messages() {
        assert_eq!(
            limit_message(
                Plan::Free,
                ManualAllowance::PerWeek(1),
                Duration::days(3) - Duration::seconds(1)
            ),
            "Your Free plan includes 1 manual crawl a week. The next one is available in 3 days."
        );
        assert_eq!(
            limit_message(Plan::Pro, ManualAllowance::PerDay(2), Duration::minutes(30)),
            "Your Pro plan includes 2 manual crawls a day. The next one is available in 30 minutes."
        );
    }

    #[test]
    fn trigger_header_is_ascii_json() {
        let events = json!({ "toast": { "message": "Crawl #2 finished · health 86/100 🦉" } });
        let (_, value) = hx_trigger(&events);
        let text = value.to_str().expect("visible ASCII only");
        assert!(text.is_ascii() && text.contains("u00b7"), "{text}");
        let back: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(back, events);
    }

    #[test]
    fn health_tones() {
        assert_eq!(health_tone(90), "c-ok");
        assert_eq!(health_tone(89), "c-warn");
        assert_eq!(health_tone(70), "c-warn");
        assert_eq!(health_tone(69), "c-err");
    }
}
