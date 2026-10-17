//! `/s/{site}/audit`: the site audit, the landing screen for a site. From the top: seven score
//! cards (Overall, SEO, GEO, indexable share, AI products, response time, open issues), then
//! side by side the ranked fix list ([`audit_fix`]) and a column with the score trend
//! ([`audit_trend`]) and the AI bots table ([`audit_ai`]), then response codes, response time,
//! crawl depth and issues by area. All of it comes from the latest finished crawl, the stored
//! summaries and change counts of the ones before it, and the latest AI access report with the
//! open incidents; per page it reads only the response-time buckets and a few example URLs.
//!
//! Before the first crawl finishes it shows that crawl's live progress instead, refreshed from
//! `/s/{site}/audit/live`. When the newest crawl failed after an earlier one finished, a banner
//! says so above the earlier results.

use std::collections::HashMap;

use askama::Template;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use codoseo_checks::{CHECKS, Category, Scope, def};
use codoseo_core::check::{CheckId, Severity};
use codoseo_core::crawl::CrawlLimits;
use codoseo_core::output::StopReason;
use codoseo_core::plan::{Plan, PlanLimits};
use codoseo_core::report::CrawlSummary;
use codoseo_geo::Intent;
use codoseo_geo::report::AccessReport;
use codoseo_store::crawls::{self, Crawl, CrawlStatus, LiveFilter, StoredSummary};
use codoseo_store::explorer;
use codoseo_store::geo::{self, Incident};
use codoseo_store::reports::{self, ChangeCounts, ResponseBuckets};
use codoseo_store::sites::{self, Site};
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::{CurrentUser, load_site};
use crate::error::AppError;
use crate::fmt;
use crate::layout::{Screen, Shell};
use crate::live::LiveCrawl;
use crate::live_audit::{self, LiveAudit, LiveInput};
use crate::render::{Hx, html};
use crate::routes::audit_ai::{BotsTable, bots_table};
use crate::routes::audit_fix::{AiFixInput, FixInput, FixList, fix_list, known_counts};
use crate::routes::audit_trend::{PolicyPin, Trend, TrendPoint, trend};
use crate::score_bars::ScoreBar;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/s/{site}/audit", get(page))
        .route("/s/{site}/audit/live", get(live))
        .route("/s/{site}/audit/watch", get(watch))
        .route("/s/{site}/icon", get(icon))
}

/// The site's icon, as its newest crawl found it.
async fn icon(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id(&site)?).await?;
    let (content_type, bytes) = sites::icon(&state.pool, site.id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(crate::render::icon_response(content_type, bytes))
}

/// How long after a crawl finished the live view still shows it, with its final scores, before
/// it hands over to the results.
const WATCH_AFTER_DONE: time::Duration = time::Duration::minutes(2);

/// The site ID from the path; anything that isn't a UUID is a 404, like a missing site.
pub fn site_id(raw: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(raw).map_err(|_| AppError::NotFound)
}

/// Finished crawls the score trend covers.
pub const TREND_CRAWLS: i64 = 14;
/// Example URLs loaded per failing SEO check.
const EXAMPLE_URLS: i64 = 5;

#[derive(Template)]
#[template(path = "audit/index.html")]
pub struct AuditPage {
    pub shell: Shell,
    pub body: AuditBody,
}

/// Everything inside `#main`: one of the four states. Also the response to an htmx GET of
/// the audit URL (the empty states reload it when a crawl is queued).
#[derive(Template)]
#[template(path = "audit/body.html")]
pub struct AuditBody {
    pub base: String,
    pub domain: String,
    /// The latest finished crawl's audit.
    pub done: Option<DoneAudit>,
    /// The first crawl's progress (no `done`), or the running-crawl banner slot (with `done`).
    pub live: Option<AuditLive>,
    /// The live audit, which takes the whole body: the first crawl, or `/audit/watch`.
    pub watch: Option<LiveAudit>,
    /// Why the latest crawl failed, when nothing has finished yet.
    pub failure: Option<String>,
    /// The public demo: sample data and no actions (no Run crawl, export or links into a site).
    pub demo: bool,
}

impl DoneAudit {
    /// The overall, SEO and GEO cards as bars.
    pub fn bars(&self) -> Vec<ScoreBar> {
        self.cards
            .iter()
            .filter(|c| is_score(c))
            .map(|c| {
                // The toned phrase (`▲ 3`) is the change; the rest reads as detail.
                let (toned, plain): (Vec<&Part>, Vec<&Part>) =
                    c.change.iter().partition(|p| !p.class.is_empty());
                let detail: Vec<&str> = plain
                    .iter()
                    .chain(&c.detail.iter().collect::<Vec<_>>())
                    .map(|p| p.text.as_str())
                    .collect();
                (c, toned, detail)
            })
            .map(|(c, toned, detail)| ScoreBar {
                change_class: toned.first().map_or("", |p| p.class),
                change: toned
                    .iter()
                    .map(|p| p.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                detail: detail.join(" · "),
                help: c.help.then_some(if c.key == "geo" {
                    "help-geo"
                } else {
                    "help-overall"
                }),
                ..ScoreBar::new(c.key, c.count.parse().ok())
            })
            .collect()
    }

    /// The cards beside the bars.
    pub fn side_cards(&self) -> impl Iterator<Item = &ScoreCard> {
        self.cards.iter().filter(|c| !is_score(c))
    }
}

impl AuditBody {
    /// The empty and failed states reload themselves when a crawl is queued.
    pub fn reloads_on_queue(&self) -> bool {
        self.done.is_none() && self.live.is_none()
    }

    /// The score card labels, for the skeleton strip while the first crawl runs.
    pub fn kpi_labels(&self) -> [&'static str; 7] {
        CARD_LABELS
    }
}

const CARD_LABELS: [&str; 7] = [
    "Overall score",
    "SEO score",
    "GEO score",
    "Indexable",
    "AI products",
    "Avg response",
    "Open issues",
];

fn is_score(c: &ScoreCard) -> bool {
    matches!(c.key, "overall" | "seo" | "geo")
}

pub struct DoneAudit {
    /// `page limit reached`, when the crawl stopped early.
    pub stop_chip: Option<&'static str>,
    /// `Crawl #14 · finished 2h ago · 4m 12s · 3.1 URL/s · next crawl Oct 20, 06:00 UTC`
    pub meta: String,
    /// When the crawl finished, in full.
    pub finished: String,
    pub number: i64,
    /// A newer crawl failed: the banner over these results.
    pub failed: Option<FailedBanner>,
    /// Every score card; overall, SEO and GEO show as bars (`bars()`).
    pub cards: Vec<ScoreCard>,
    pub fixes: FixList,
    pub trend: Trend,
    /// None before the site's first AI access report.
    pub bots: Option<BotsTable>,
    pub codes: Vec<Segment>,
    /// `1,284 URLs`
    pub pages_label: String,
    pub depth: Vec<Bar>,
    pub times: Vec<HBar>,
    /// `Average 182 ms`
    pub times_aside: String,
    pub areas: Vec<CategoryBar>,
}

pub struct FailedBanner {
    pub number: i64,
    pub ago: String,
    pub reason: String,
}

/// One phrase of a card's line: it never breaks inside.
pub struct Part {
    pub text: String,
    /// Tone class, or empty.
    pub class: &'static str,
}

fn part(text: impl Into<String>, class: &'static str) -> Part {
    Part {
        text: text.into(),
        class,
    }
}

pub struct ScoreCard {
    /// `overall`, `seo`, `geo`, `indexable`, `ai`, `response`, `issues`.
    pub key: &'static str,
    pub label: &'static str,
    /// A tag before the label: `(class, text)`.
    pub tag: Option<(&'static str, &'static str)>,
    pub value: String,
    /// The number for the count-up; empty for "—".
    pub count: String,
    pub unit: String,
    /// The change since the previous crawl.
    pub change: Vec<Part>,
    /// One more line of detail.
    pub detail: Vec<Part>,
    /// Status dots, on the AI products card.
    pub dots: Vec<&'static str>,
    pub href: Option<String>,
    /// `is-strong`, `is-err`, `is-ok` or empty.
    pub tone: &'static str,
    /// Has a "?" explaining it.
    pub help: bool,
}

pub struct CategoryBar {
    pub label: &'static str,
    pub n: u32,
    pub count: String,
    pub width: u32,
    pub ai: bool,
}

/// A response-code class: one stackbar segment and one row.
pub struct Segment {
    pub label: &'static str,
    pub class: &'static str,
    pub n: u32,
    pub count: String,
    pub href: Option<String>,
}

/// One vertical bar.
pub struct Bar {
    pub label: String,
    pub value: String,
    pub height: u32,
    pub class: &'static str,
    pub tip: String,
}

/// One horizontal bar.
pub struct HBar {
    pub label: &'static str,
    pub value: String,
    pub width: u32,
    pub class: &'static str,
}

/// The live crawl line: the first-crawl progress block, or the slim banner over a finished
/// audit. Polls itself every 2 s while a crawl is queued or running.
#[derive(Template)]
#[template(path = "audit/live.html")]
pub struct AuditLive {
    pub base: String,
    pub banner: bool,
    /// A crawl is queued or running.
    pub active: bool,
    /// `queued` or `running`, for the mascot.
    pub state: &'static str,
    pub line: String,
    pub sub: String,
    /// The counters and newest pages of the running first crawl (never on the banner).
    pub panel: Option<LiveCrawl>,
}

impl AuditLive {
    /// `seen` is the newest page the poller already shows (see [`LiveCrawl`]).
    fn new(site: &Site, banner: bool, active: Option<&Crawl>, seen: Option<u32>) -> AuditLive {
        let base = format!("/s/{}", site.id);
        let Some(c) = active else {
            return AuditLive {
                base,
                banner,
                active: false,
                state: "idle",
                line: "Crawl finished".to_owned(),
                sub: "Loading the audit…".to_owned(),
                panel: None,
            };
        };
        let running = c.status == CrawlStatus::Running;
        let panel = LiveCrawl::for_crawl(c, seen, OffsetDateTime::now_utc());
        let (line, sub) = match (banner, running) {
            (true, true) => (
                match &panel {
                    Some(p) => format!("Crawl #{} running · {}", c.number, p.summary()),
                    None => format!("Crawl #{} running", c.number),
                },
                String::new(),
            ),
            (true, false) => (format!("Crawl #{} queued", c.number), String::new()),
            // The counters and pages are in the panel below the line.
            (false, true) => (
                format!("Crawling {}", site.domain),
                "Scores and fixes appear here the moment the crawl finishes.".to_owned(),
            ),
            (false, false) => (
                format!("Starting a crawl of {}", site.domain),
                format!(
                    "Queued {}. It starts as soon as a crawler is free.",
                    fmt::ago(c.queued_at)
                ),
            ),
        };
        AuditLive {
            base,
            banner,
            active: true,
            state: if running { "running" } else { "queued" },
            line,
            sub,
            panel: panel.filter(|_| !banner),
        }
    }
}

async fn page(
    State(state): State<AppState>,
    user: CurrentUser,
    hx: Hx,
    Path(site): Path<String>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id(&site)?).await?;
    let body = load_body(&state, &site, user.account.plan).await?;
    if hx.partial() {
        return Ok(html(&body)?.into_response());
    }
    let shell = Shell::load(&state, &user, Some(&site), Screen::Audit).await?;
    Ok(html(&AuditPage { shell, body })?.into_response())
}

/// The whole audit page, for an action that returns to it (AI access's "Mark intended" from
/// the fix list).
pub(crate) async fn render_page(
    state: &AppState,
    user: &CurrentUser,
    site: &Site,
) -> Result<Html<String>, AppError> {
    let body = load_body(state, site, user.account.plan).await?;
    let shell = Shell::load(state, user, Some(site), Screen::Audit).await?;
    html(&AuditPage { shell, body })
}

#[derive(Deserialize)]
struct LiveQuery {
    view: Option<String>,
    /// The newest page the poller shows.
    seen: Option<u32>,
}

/// The live line on its own, polled every 2 s (`?view=banner` for the slim banner).
async fn live(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
    Query(q): Query<LiveQuery>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id(&site)?).await?;
    let active = crawls::active(&state.pool, site.id).await?;
    let banner = q.view.as_deref() == Some("banner");
    Ok(html(&AuditLive::new(&site, banner, active.as_ref(), q.seen))?.into_response())
}

#[derive(Deserialize)]
struct WatchQuery {
    /// The row filter: `all`, `issues`, `ai` or `errors`.
    f: Option<String>,
}

/// The live audit on its own screen, for a crawl over a finished audit; polled every 2 s while
/// the crawl runs. With nothing running or just finished, the results.
async fn watch(
    State(state): State<AppState>,
    user: CurrentUser,
    hx: Hx,
    Path(site): Path<String>,
    Query(q): Query<WatchQuery>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id(&site)?).await?;
    let filter = live_audit::filter_from(q.f.as_deref());
    let Some(view) = live_view(&state, &site, user.account.plan, filter).await? else {
        return Ok(Redirect::to(&format!("/s/{}/audit", site.id)).into_response());
    };
    if hx.partial() {
        return Ok(html(&view)?.into_response());
    }
    let body = AuditBody {
        base: format!("/s/{}", site.id),
        domain: site.domain.clone(),
        done: None,
        live: None,
        watch: Some(view),
        failure: None,
        demo: false,
    };
    let shell = Shell::load(&state, &user, Some(&site), Screen::Audit).await?;
    Ok(html(&AuditPage { shell, body })?.into_response())
}

/// The live audit of the site's running crawl, or of the one that finished in the last
/// [`WATCH_AFTER_DONE`]; `None` otherwise.
async fn live_view(
    state: &AppState,
    site: &Site,
    plan: Plan,
    filter: LiveFilter,
) -> Result<Option<LiveAudit>, AppError> {
    let pool = &state.pool;
    let now = OffsetDateTime::now_utc();
    let crawl = match crawls::active(pool, site.id).await? {
        Some(c) => c,
        None => match crawls::history(pool, site.id, 1).await?.into_iter().next() {
            Some(c)
                if c.status == CrawlStatus::Done
                    && c.finished_at.is_some_and(|f| now - f < WATCH_AFTER_DONE) =>
            {
                c
            }
            _ => return Ok(None),
        },
    };
    let rows = crawls::newest_live_rows(pool, crawl.id, filter, live_audit::ROWS_SHOWN).await?;
    let previous = if crawl.status == CrawlStatus::Done {
        crawls::previous_done(pool, site.id, crawl.id).await?
    } else {
        crawls::latest_done(pool, site.id).await?
    };
    let max_pages = PlanLimits::for_plan(plan)
        .max_pages
        .unwrap_or(CrawlLimits::default().max_pages);
    let icon = sites::icon_version(pool, site.id)
        .await?
        .map(|v| format!("/s/{}/icon?v={v}", site.id));
    Ok(Some(LiveAudit::build(LiveInput {
        base: format!("/s/{}", site.id),
        domain: site.domain.clone(),
        icon,
        crawl: &crawl,
        rows: &rows,
        filter,
        previous: previous.as_ref(),
        max_pages,
        now,
    })))
}

async fn load_body(state: &AppState, site: &Site, plan: Plan) -> Result<AuditBody, AppError> {
    let pool = &state.pool;
    let latest = crawls::latest_done(pool, site.id).await?;
    let active = crawls::active(pool, site.id).await?;
    let newest = crawls::history(pool, site.id, 1).await?.into_iter().next();
    let mut body = AuditBody {
        base: format!("/s/{}", site.id),
        domain: site.domain.clone(),
        done: None,
        live: None,
        watch: None,
        failure: None,
        demo: false,
    };
    match latest {
        Some(crawl) => {
            // The newest crawl failed after this one finished.
            let failed =
                newest.filter(|c| c.status == CrawlStatus::Failed && c.number > crawl.number);
            body.done = Some(done_audit(state, site, &crawl, failed.as_ref()).await?);
            body.live = Some(AuditLive::new(site, true, active.as_ref(), None));
        }
        None if active.is_some() => {
            body.watch = live_view(state, site, plan, LiveFilter::All).await?;
        }
        None => {
            body.failure = newest
                .filter(|c| c.status == CrawlStatus::Failed)
                .map(|c| failure_reason(&c));
        }
    }
    Ok(body)
}

fn failure_reason(c: &Crawl) -> String {
    c.failure_reason
        .clone()
        .unwrap_or_else(|| "The crawl stopped unexpectedly.".to_owned())
}

/// The AI side of the audit: the latest access report, the owner's intent and the open
/// incidents. None before the site's first report.
pub(crate) struct AiData {
    /// The site's start URL, which the remedies are written for.
    pub origin: String,
    pub report: AccessReport,
    pub intent: Intent,
    pub open: Vec<Incident>,
}

/// The policy changes behind the trend's policy pins: the history rows that judged one of
/// `crawls` again, by crawl, oldest first.
async fn policy_pins(
    pool: &sqlx::PgPool,
    crawls: &[Uuid],
) -> Result<HashMap<Uuid, Vec<PolicyPin>>, AppError> {
    let rows = codoseo_store::policy::history_for_crawls(pool, crawls).await?;
    let mut out: HashMap<Uuid, Vec<PolicyPin>> = HashMap::new();
    for r in rows {
        let Some(crawl) = r.crawl_id else { continue };
        out.entry(crawl).or_default().push(PolicyPin {
            who: crate::routes::policy::who(&r),
            health: (r.health_before, r.health_after),
            geo: (r.geo_before, r.geo_after),
        });
    }
    Ok(out)
}

async fn done_audit(
    state: &AppState,
    site: &Site,
    crawl: &Crawl,
    failed: Option<&Crawl>,
) -> Result<DoneAudit, AppError> {
    let pool = &state.pool;
    // Oldest first, ending with `crawl` (both are the latest finished crawl, chosen alike).
    let recent = crawls::recent_done(pool, site.id, TREND_CRAWLS).await?;
    let times = reports::response_time_buckets(pool, crawl.id).await?;
    let next = sites::next_crawl_at(pool, site.id).await?;
    let ids: Vec<Uuid> = recent.iter().map(|c| c.id).collect();
    let changes = reports::change_kind_counts_by_crawl(pool, &ids).await?;
    let policy_pins = policy_pins(pool, &ids).await?;
    let failing: Vec<CheckId> = crawl
        .summary()
        .map(|s| known_counts(&s).into_iter().map(|c| c.0).collect())
        .unwrap_or_default();
    let examples = explorer::example_urls(pool, crawl.id, &failing, EXAMPLE_URLS).await?;
    let ai = match geo::latest_report(pool, site.id).await? {
        Some(latest) => Some(AiData {
            origin: site.start_url.clone(),
            report: latest.report,
            intent: geo::get_intent(pool, site.id).await?,
            open: geo::open_incidents(pool, site.id).await?,
        }),
        None => None,
    };
    Ok(done_view(DoneInput {
        base: format!("/s/{}", site.id),
        demo: false,
        crawl,
        recent: &recent,
        changes: &changes,
        policy_pins: &policy_pins,
        thresholds: codoseo_store::policy::get(pool, site.id)
            .await?
            .thresholds(),
        times: &times,
        next,
        failed,
        examples: &examples,
        ai,
        now: OffsetDateTime::now_utc(),
    }))
}

/// What a finished audit is drawn from: the database loads it ([`done_audit`]); the public demo
/// builds it from a fixture.
pub(crate) struct DoneInput<'a> {
    /// `/s/{site}`, which the screen's links hang off.
    pub base: String,
    pub demo: bool,
    /// The latest finished crawl.
    pub crawl: &'a Crawl,
    /// Up to [`TREND_CRAWLS`] finished crawls, oldest first, ending with `crawl`.
    pub recent: &'a [Crawl],
    /// Change counts of the `recent` crawls, by crawl.
    pub changes: &'a HashMap<Uuid, ChangeCounts>,
    /// The policy changes that judged the `recent` crawls again, by crawl, oldest first.
    pub policy_pins: &'a HashMap<Uuid, Vec<PolicyPin>>,
    /// The site policy's thresholds, which the check titles name.
    pub thresholds: codoseo_checks::Thresholds,
    pub times: &'a ResponseBuckets,
    /// The next scheduled crawl.
    pub next: Option<OffsetDateTime>,
    /// A newer crawl that failed.
    pub failed: Option<&'a Crawl>,
    /// Example URLs per failing SEO check.
    pub examples: &'a HashMap<CheckId, Vec<String>>,
    pub ai: Option<AiData>,
    pub now: OffsetDateTime,
}

pub(crate) fn done_view(input: DoneInput<'_>) -> DoneAudit {
    let DoneInput {
        base,
        demo,
        crawl,
        recent,
        changes,
        policy_pins,
        thresholds,
        times,
        next,
        failed,
        examples,
        ai,
        now,
    } = input;
    let stored = crawl.summary();
    let summary = stored
        .as_ref()
        .map(|s| s.report_summary.clone())
        .unwrap_or_default();
    let previous = recent
        .iter()
        .rev()
        .find(|c| c.id != crawl.id && c.finished_at <= crawl.finished_at);
    let prev_stored = previous.and_then(Crawl::summary);

    let stop_chip = stored.as_ref().and_then(|s| match s.stop_reason {
        StopReason::PageLimit => Some("page limit reached"),
        StopReason::TimeLimit => Some("time limit reached"),
        _ => None,
    });
    let mut meta = vec![format!("Crawl #{}", crawl.number)];
    if let Some(at) = crawl.finished_at {
        meta.push(format!("finished {}", fmt::ago_from(at, now)));
    }
    meta.push(format!("{} URLs", fmt::thousands(summary.pages)));
    if let Some(d) = crawl.duration() {
        meta.push(fmt::duration(d));
        meta.push(rate(summary.pages, d));
    }
    meta.push(match next {
        Some(at) => format!("next crawl {}", fmt::until_from(at, now)),
        None => "no crawl scheduled".to_owned(),
    });

    let seo_total = u32::try_from(crawl.checks_total.unwrap_or(0)).unwrap_or(0);
    let seo_passed = u32::try_from(crawl.checks_passed.unwrap_or(0)).unwrap_or(0);
    let bots = ai.as_ref().map(|a| bots_table(&a.report, &a.intent));
    let fixes = fix_list(FixInput {
        base: &base,
        demo,
        crawl_id: crawl.id,
        current: stored.as_ref(),
        previous: prev_stored.as_ref(),
        seo_passed,
        seo_total,
        examples,
        thresholds,
        ai: ai.as_ref().map(|a| AiFixInput {
            origin: &a.origin,
            report: &a.report,
            intent: &a.intent,
            open: &a.open,
            now,
        }),
    });

    let points: Vec<TrendPoint> = recent
        .iter()
        .filter_map(|c| {
            Some(TrendPoint {
                number: c.number,
                at: c.finished_at?,
                seo: c.health_score,
                geo: c.geo_score,
                overall: c.overall_score,
                changes: changes.get(&c.id).cloned().unwrap_or_default(),
                policy: policy_pins.get(&c.id).cloned().unwrap_or_default(),
            })
        })
        .collect();

    let mut areas = stored.as_ref().map(category_bars).unwrap_or_default();
    // The open AI issues are an area of their own.
    if fixes.ai > 0 {
        areas.push(CategoryBar {
            label: "AI access",
            n: fixes.ai as u32,
            count: fmt::thousands(fixes.ai as u32),
            width: 0,
            ai: true,
        });
        scale_bars(&mut areas);
    }

    let cards = cards(CardInput {
        base: &base,
        demo,
        crawl,
        previous,
        summary: &summary,
        prev_summary: prev_stored.as_ref().map(|s| &s.report_summary),
        times,
        bots: bots.as_ref(),
        fixes: &fixes,
    });

    DoneAudit {
        stop_chip,
        meta: meta.join(" · "),
        finished: crawl.finished_at.map(fmt::datetime).unwrap_or_default(),
        number: crawl.number,
        failed: failed.map(|f| FailedBanner {
            number: f.number,
            ago: f
                .finished_at
                .map_or_else(String::new, |at| fmt::ago_from(at, now)),
            reason: failure_reason(f),
        }),
        cards,
        fixes,
        trend: trend(&points, next, now),
        bots,
        codes: segments(&base, demo, &summary),
        pages_label: format!("{} URLs", fmt::thousands(summary.pages)),
        depth: depth_bars(&summary),
        times: time_bars(times),
        times_aside: format!("Average {} ms", fmt::thousands(summary.avg_response_ms)),
        areas,
    }
}

/// `3.1 URL/s`
fn rate(pages: u32, d: time::Duration) -> String {
    let secs = d.as_seconds_f64();
    match f64::from(pages) / secs {
        _ if secs < 0.5 => "— URL/s".to_owned(),
        r if r < 0.1 => "< 0.1 URL/s".to_owned(),
        r => format!("{r:.1} URL/s"),
    }
}

struct CardInput<'a> {
    base: &'a str,
    demo: bool,
    crawl: &'a Crawl,
    previous: Option<&'a Crawl>,
    summary: &'a CrawlSummary,
    prev_summary: Option<&'a CrawlSummary>,
    times: &'a ResponseBuckets,
    bots: Option<&'a BotsTable>,
    fixes: &'a FixList,
}

/// The change line of a score: against the previous crawl's, when both have one.
fn score_change(
    cur: Option<i16>,
    previous: Option<&Crawl>,
    of: fn(&Crawl) -> Option<i16>,
) -> Vec<Part> {
    let Some(prev) = previous else {
        return vec![part("First crawl", "")];
    };
    match (cur, of(prev)) {
        (None, _) => vec![part("Not scored yet", "")],
        (Some(_), None) => vec![part("No earlier score", "")],
        (Some(c), Some(p)) => change_parts(i64::from(c) - i64::from(p), "", true),
    }
}

/// `▲ 3 since last crawl` (up is good when `up_good`), or `No change`.
fn change_parts(d: i64, unit: &str, up_good: bool) -> Vec<Part> {
    let tone = |good: bool| if good { "up" } else { "down" };
    match d {
        0 => vec![part("No change", "")],
        d if d > 0 => vec![
            part(format!("▲ {}{unit}", fmt::thousands(d)), tone(up_good)),
            part("since last crawl", ""),
        ],
        d => vec![
            part(format!("▼ {}{unit}", fmt::thousands(-d)), tone(!up_good)),
            part("since last crawl", ""),
        ],
    }
}

fn number_card(
    key: &'static str,
    label: &'static str,
    value: Option<i16>,
    unit: &str,
) -> ScoreCard {
    ScoreCard {
        key,
        label,
        tag: None,
        value: value.map_or_else(|| "—".to_owned(), |v| v.to_string()),
        count: value.map(|v| v.to_string()).unwrap_or_default(),
        unit: if value.is_some() {
            unit.to_owned()
        } else {
            String::new()
        },
        change: Vec::new(),
        detail: Vec::new(),
        dots: Vec::new(),
        href: None,
        tone: "",
        help: false,
    }
}

fn checks_line(passed: impl Into<i64>, total: impl Into<i64>) -> Vec<Part> {
    vec![part(
        format!(
            "{} of {} checks pass",
            fmt::thousands(passed),
            fmt::thousands(total)
        ),
        "",
    )]
}

fn cards(k: CardInput<'_>) -> Vec<ScoreCard> {
    let CardInput {
        base,
        demo,
        crawl,
        previous,
        summary: s,
        prev_summary,
        times,
        bots,
        fixes,
    } = k;
    let seo_passed = i64::from(crawl.checks_passed.unwrap_or(0));
    let seo_total = i64::from(crawl.checks_total.unwrap_or(0));
    let geo_counts = crawl
        .geo_checks_passed
        .zip(crawl.geo_checks_applicable)
        .map(|(p, a)| (i64::from(p), i64::from(a)));

    let mut overall = number_card("overall", CARD_LABELS[0], crawl.overall_score, "/100");
    overall.change = score_change(crawl.overall_score, previous, |c| c.overall_score);
    overall.detail = match (crawl.overall_score, geo_counts) {
        (Some(_), Some((p, a))) => checks_line(seo_passed + p, seo_total + a),
        _ => vec![part("Needs a GEO score", "")],
    };
    overall.tone = "is-strong";
    overall.help = true;

    let mut seo = number_card("seo", "score", crawl.health_score, "/100");
    seo.tag = Some(("t-seo", "SEO"));
    seo.change = score_change(crawl.health_score, previous, |c| c.health_score);
    seo.detail = checks_line(seo_passed, seo_total);
    seo.href = Some("#fix".to_owned());

    let mut geo = number_card("geo", "score", crawl.geo_score, "/100");
    geo.tag = Some(("t-ai", "GEO"));
    geo.change = score_change(crawl.geo_score, previous, |c| c.geo_score);
    geo.detail = match geo_counts {
        Some((p, a)) => checks_line(p, a),
        None => vec![part("Comes with the next AI check", "")],
    };
    geo.help = true;

    let pct = fmt::pct1(u64::from(s.indexable), u64::from(s.pages));
    let mut indexable = ScoreCard {
        key: "indexable",
        label: CARD_LABELS[3],
        tag: None,
        value: pct.clone(),
        count: pct,
        unit: "%".to_owned(),
        change: match prev_summary {
            None => vec![part("First crawl", "")],
            Some(p) => {
                let d = i64::from(s.indexable) - i64::from(p.indexable);
                match d {
                    0 => vec![part("No change", "")],
                    d if d > 0 => vec![part(
                        format!(
                            "▲ {} page{}",
                            fmt::thousands(d),
                            if d == 1 { "" } else { "s" }
                        ),
                        "up",
                    )],
                    d => vec![part(
                        format!(
                            "▼ {} page{}",
                            fmt::thousands(-d),
                            if d == -1 { "" } else { "s" }
                        ),
                        "down",
                    )],
                }
            }
        },
        detail: vec![part(
            format!(
                "{} of {} URLs",
                fmt::thousands(s.indexable),
                fmt::thousands(s.pages)
            ),
            "",
        )],
        dots: Vec::new(),
        href: (!demo).then(|| format!("{base}/explorer?filter=nx")),
        tone: "",
        help: false,
    };
    if s.pages == 0 {
        indexable.value = "—".to_owned();
        indexable.count.clear();
        indexable.unit.clear();
    }

    let ai = match bots {
        Some(b) if !b.unknown => ScoreCard {
            key: "ai",
            label: "products",
            tag: Some(("t-ai", "AI")),
            value: b.reach.to_string(),
            count: b.reach.to_string(),
            unit: format!(" / {}", b.of),
            change: if b.blocked + b.partly == 0 {
                vec![part("All can reach you", "c-ok")]
            } else {
                let mut v = Vec::new();
                if b.blocked > 0 {
                    v.push(part(format!("{} blocked", b.blocked), "c-err"));
                }
                if b.partly > 0 {
                    v.push(part(format!("{} partly", b.partly), "c-warn"));
                }
                v
            },
            detail: if demo {
                Vec::new()
            } else {
                vec![part("AI access →", "c-link")]
            },
            dots: b.dots(),
            href: (!demo).then(|| format!("{base}/ai-access")),
            tone: "",
            help: false,
        },
        Some(_) => ScoreCard {
            change: vec![part("robots.txt unreadable", "c-warn")],
            detail: vec![part("AI access →", "c-link")],
            href: (!demo).then(|| format!("{base}/ai-access")),
            tag: Some(("t-ai", "AI")),
            ..number_card("ai", "products", None, "")
        },
        None => ScoreCard {
            change: vec![part("No AI check yet", "")],
            detail: vec![part("Comes with the next crawl", "")],
            tag: Some(("t-ai", "AI")),
            ..number_card("ai", "products", None, "")
        },
    };

    let avg = s.avg_response_ms;
    let response = ScoreCard {
        key: "response",
        label: CARD_LABELS[5],
        tag: None,
        value: fmt::thousands(avg),
        count: avg.to_string(),
        unit: " ms".to_owned(),
        // Faster is better: a drop is the good colour.
        change: match prev_summary {
            None => vec![part("First crawl", "")],
            Some(p) => change_parts(i64::from(avg) - i64::from(p.avg_response_ms), " ms", false),
        },
        detail: vec![part(
            match times.very_slow {
                0 => "None over 1 s".to_owned(),
                n => format!("{} over 1 s", fmt::pages(n as u32)),
            },
            "",
        )],
        dots: Vec::new(),
        href: None,
        tone: "",
        help: false,
    };

    let issues = if fixes.is_clear() {
        ScoreCard {
            key: "issues",
            label: CARD_LABELS[6],
            tag: None,
            value: "0".to_owned(),
            count: "0".to_owned(),
            unit: String::new(),
            change: vec![part("Nothing to fix", "c-ok")],
            detail: checks_line(fixes.checks_passed, fixes.checks_total),
            dots: Vec::new(),
            href: None,
            tone: "is-ok",
            help: false,
        }
    } else {
        let mut change = Vec::new();
        for (sev, name, class) in [
            (Severity::Critical, "critical", "c-err"),
            (Severity::Warning, "warning", ""),
            (Severity::Notice, "notice", ""),
        ] {
            let n = fixes.count(sev);
            if n > 0 {
                change.push(part(format!("{n} {name}"), class));
            }
        }
        ScoreCard {
            key: "issues",
            label: CARD_LABELS[6],
            tag: None,
            value: fixes.total.to_string(),
            count: fixes.total.to_string(),
            unit: String::new(),
            change,
            detail: vec![part("See the fix list ↓", "c-link")],
            dots: Vec::new(),
            href: Some("#fix".to_owned()),
            tone: if fixes.critical > 0 {
                "is-err"
            } else {
                "is-warn"
            },
            help: false,
        }
    };

    vec![overall, seo, geo, indexable, ai, response, issues]
}

fn category_label(c: Category) -> &'static str {
    match c {
        Category::Response => "Response",
        Category::Indexability => "Indexability",
        Category::Links => "Links",
        Category::OnPage => "On-page",
        Category::Content => "Content",
        Category::Technical => "Technical",
        Category::Sitemap => "Sitemap",
        Category::Social => "Social",
        Category::Schema => "Schema",
        Category::Policy => "Policy",
    }
}

/// What one failing check weighs in the charts: its pages, or 1 for a site-wide fact.
fn weight(id: CheckId, n: u32) -> u32 {
    if def(id).scope == Scope::SiteWide {
        1
    } else {
        n
    }
}

/// This crawl's failing pages per check category, the largest first.
fn category_bars(s: &StoredSummary) -> Vec<CategoryBar> {
    let mut by: Vec<(Category, u32)> = Vec::new();
    for c in CHECKS.iter().map(|d| d.category) {
        if !by.iter().any(|(k, _)| *k == c) {
            by.push((c, 0));
        }
    }
    for (id, n) in known_counts(s) {
        if let Some(entry) = by.iter_mut().find(|(k, _)| *k == def(id).category) {
            entry.1 += weight(id, n);
        }
    }
    by.retain(|(_, n)| *n > 0);
    let mut bars: Vec<CategoryBar> = by
        .into_iter()
        .map(|(c, n)| CategoryBar {
            label: category_label(c),
            n,
            count: fmt::thousands(n),
            width: 0,
            ai: false,
        })
        .collect();
    scale_bars(&mut bars);
    bars
}

/// The largest first, each as a share of it.
fn scale_bars(bars: &mut [CategoryBar]) {
    bars.sort_by_key(|b| std::cmp::Reverse(b.n));
    let max = bars.first().map_or(1, |b| b.n.max(1));
    for b in bars {
        b.width = (u64::from(b.n) * 100 / u64::from(max)).max(3) as u32;
    }
}

fn segments(base: &str, demo: bool, s: &CrawlSummary) -> Vec<Segment> {
    let st = &s.status;
    [
        ("2xx OK", "bg-ok", st.ok, Some("s2")),
        ("3xx Redirect", "bg-blue", st.redirect, Some("s3")),
        ("4xx Not found", "bg-warn", st.client_error, Some("s4")),
        ("5xx Server error", "bg-err", st.server_error, Some("s5")),
        ("No response", "bg-ghost", st.failed, Some("s0")),
        ("Blocked", "bg-ink", st.blocked, None),
    ]
    .into_iter()
    .filter(|(label, _, n, _)| *n > 0 || label.starts_with(['2', '3', '4', '5']))
    .map(|(label, class, n, filter)| Segment {
        label,
        class,
        n,
        count: fmt::thousands(n),
        href: filter
            .filter(|_| !demo && n > 0)
            .map(|f| format!("{base}/explorer?filter={f}")),
    })
    .collect()
}

/// Pages per click depth. Trailing empty buckets are dropped, but 0–3 always show; the last
/// bucket is 10 and deeper. Pages found only in the sitemap get their own bar.
fn depth_bars(s: &CrawlSummary) -> Vec<Bar> {
    let last = s.depth.iter().rposition(|&n| n > 0).map_or(0, |i| i + 1);
    let len = last.max(4);
    let mut buckets: Vec<(String, u32, &'static str, String)> = (0..len)
        .map(|i| {
            let n = s.depth.get(i).copied().unwrap_or(0);
            let label = if i >= 10 {
                "10+".to_owned()
            } else {
                i.to_string()
            };
            let class = if i >= 5 { "bg-warn" } else { "bg-blue" };
            let tip = match i {
                0 => "the homepage".to_owned(),
                1 => "1 click from the homepage".to_owned(),
                i if i >= 10 => "10 or more clicks".to_owned(),
                i => format!("{i} clicks from the homepage"),
            };
            (label, n, class, tip)
        })
        .collect();
    if s.no_depth > 0 {
        buckets.push((
            "map".to_owned(),
            s.no_depth,
            "bg-ghost",
            "found only in the sitemap".to_owned(),
        ));
    }
    let max = buckets.iter().map(|b| b.1).max().unwrap_or(0).max(1);
    buckets
        .into_iter()
        .map(|(label, n, class, tip)| Bar {
            label,
            value: fmt::thousands(n),
            height: if n == 0 { 0 } else { (n * 100 / max).max(2) },
            class,
            tip,
        })
        .collect()
}

fn time_bars(t: &ResponseBuckets) -> Vec<HBar> {
    let total = t.total().max(1);
    [
        ("Under 200 ms", t.fast, "bg-ok"),
        ("200–500 ms", t.ok, "bg-ok"),
        ("500 ms–1 s", t.slow, "bg-warn"),
        ("Over 1 s", t.very_slow, "bg-err"),
    ]
    .into_iter()
    .map(|(label, n, class)| HBar {
        label,
        value: fmt::thousands(n),
        width: u32::try_from(n * 100 / total).unwrap_or(100),
        class,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_checks::BASE_CHECKS;
    use codoseo_core::report::StatusCounts;
    use codoseo_store::crawl_queue::CrawlTrigger;
    use time::Duration;

    fn stored(counts: Vec<(&str, u32)>, pages: u32) -> StoredSummary {
        StoredSummary {
            stop_reason: StopReason::Completed,
            report_summary: CrawlSummary {
                pages,
                indexable: pages,
                status: StatusCounts::default(),
                ..Default::default()
            },
            counts: counts.into_iter().map(|(s, n)| (s.to_owned(), n)).collect(),
            excluded: Default::default(),
            geo: None,
        }
    }

    fn crawl(number: i64, now: OffsetDateTime, s: &StoredSummary, geo: Option<i16>) -> Crawl {
        let finished = now - Duration::days(14 - number);
        Crawl {
            id: Uuid::from_u128(number as u128),
            site_id: Uuid::nil(),
            number,
            status: CrawlStatus::Done,
            trigger: CrawlTrigger::Schedule,
            queued_at: finished - Duration::minutes(2),
            started_at: Some(finished - Duration::minutes(1)),
            finished_at: Some(finished),
            health_score: Some(80),
            checks_passed: Some((BASE_CHECKS - s.counts.len()) as i16),
            checks_total: Some(BASE_CHECKS as i16),
            geo_score: geo,
            geo_checks_passed: geo.map(|_| 9),
            geo_checks_applicable: geo.map(|_| 11),
            overall_score: geo.map(|g| (0.6 * 80.0 + 0.4 * f64::from(g)).round() as i16),
            progress: None,
            summary: Some(serde_json::json!({
                "stop_reason": s.stop_reason,
                "report_summary": s.report_summary,
                "counts": s.counts,
            })),
            failure_reason: None,
        }
    }

    fn view(recent: &[Crawl], failed: Option<&Crawl>) -> DoneAudit {
        let now = OffsetDateTime::now_utc();
        done_view(DoneInput {
            base: "/s/x".to_owned(),
            demo: false,
            crawl: recent.last().unwrap(),
            recent,
            changes: &HashMap::new(),
            policy_pins: &HashMap::new(),
            thresholds: codoseo_checks::Thresholds::DEFAULT,
            times: &ResponseBuckets::default(),
            next: None,
            failed,
            examples: &HashMap::new(),
            ai: None,
            now,
        })
    }

    fn card<'a>(d: &'a DoneAudit, key: &str) -> &'a ScoreCard {
        d.cards.iter().find(|c| c.key == key).unwrap()
    }

    fn text(parts: &[Part]) -> String {
        parts
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn seven_cards_and_old_crawls_without_geo_read_as_a_dash() {
        let now = OffsetDateTime::now_utc();
        let s = stored(vec![("http_4xx", 2)], 10);
        let one = [crawl(1, now, &s, None)];
        let d = view(&one, None);
        let keys: Vec<&str> = d.cards.iter().map(|c| c.key).collect();
        assert_eq!(
            keys,
            [
                "overall",
                "seo",
                "geo",
                "indexable",
                "ai",
                "response",
                "issues"
            ]
        );
        assert_eq!(card(&d, "overall").value, "—");
        assert_eq!(card(&d, "geo").value, "—");
        assert_eq!(text(&card(&d, "seo").change), "First crawl");
        assert_eq!(text(&card(&d, "issues").change), "1 critical");
        assert!(d.trend.waiting.is_some(), "one crawl: no chart");
        assert!(d.bots.is_none());

        // A second crawl with GEO: the overall card counts both kinds of checks.
        let two = [crawl(1, now, &s, None), crawl(2, now, &s, Some(70))];
        let d = view(&two, None);
        assert_eq!(card(&d, "overall").value, "76");
        assert_eq!(text(&card(&d, "overall").change), "No earlier score");
        assert_eq!(
            text(&card(&d, "overall").detail),
            format!(
                "{} of {} checks pass",
                BASE_CHECKS - 1 + 9,
                BASE_CHECKS + 11
            )
        );
        assert_eq!(text(&card(&d, "seo").change), "No change");
        assert!(d.trend.waiting.is_none());
    }

    #[test]
    fn a_clean_crawl_is_all_clear_and_a_failed_newer_crawl_shows_a_banner() {
        let now = OffsetDateTime::now_utc();
        let s = stored(vec![], 10);
        let ok = [crawl(1, now, &s, Some(90))];
        let mut failed = crawl(2, now, &s, None);
        failed.status = CrawlStatus::Failed;
        failed.failure_reason = Some("503 on every page".to_owned());
        let d = view(&ok, Some(&failed));
        let issues = card(&d, "issues");
        assert_eq!(issues.value, "0");
        assert_eq!(issues.tone, "is-ok");
        assert!(d.fixes.is_clear());
        let b = d.failed.unwrap();
        assert_eq!(b.number, 2);
        assert_eq!(b.reason, "503 on every page");
    }

    #[test]
    fn changes_are_signed_and_coloured_by_what_is_good() {
        assert_eq!(text(&change_parts(3, "", true)), "▲ 3 since last crawl");
        assert_eq!(change_parts(3, "", true)[0].class, "up");
        assert_eq!(change_parts(40, " ms", false)[0].class, "down");
        assert_eq!(
            text(&change_parts(-40, " ms", false)),
            "▼ 40 ms since last crawl"
        );
        assert_eq!(text(&change_parts(0, "", true)), "No change");
    }

    #[test]
    fn categories_sum_their_checks_and_hide_the_clean_ones() {
        let s = stored(
            vec![
                ("title_missing", 3),
                ("description_missing", 9),
                ("http_4xx", 2),
            ],
            100,
        );
        let bars = category_bars(&s);
        assert_eq!(bars[0].label, "On-page");
        assert_eq!(bars[0].count, "12");
        assert_eq!(bars[0].width, 100);
        assert_eq!(bars[1].label, "Response");
        assert_eq!(bars.len(), 2);
        assert!(category_bars(&stored(vec![], 1)).is_empty());
    }

    #[test]
    fn depth_keeps_zero_to_three_and_marks_deep_pages() {
        let s = CrawlSummary {
            depth: vec![1, 5, 0, 0, 0, 0],
            ..Default::default()
        };
        let bars = depth_bars(&s);
        assert_eq!(
            bars.iter().map(|b| b.label.as_str()).collect::<Vec<_>>(),
            ["0", "1", "2", "3"]
        );
        assert_eq!(bars[1].height, 100);
        assert_eq!(bars[0].height, 20);

        let mut depth = vec![0; 11];
        depth[0] = 1;
        depth[6] = 2;
        depth[10] = 4;
        let bars = depth_bars(&CrawlSummary {
            depth,
            no_depth: 3,
            ..Default::default()
        });
        assert_eq!(bars.len(), 12);
        assert_eq!(bars[10].label, "10+");
        assert_eq!(bars[6].class, "bg-warn");
        assert_eq!(bars[4].class, "bg-blue");
        assert_eq!(bars[11].value, "3");
    }

    #[test]
    fn response_times_are_shares_of_the_crawl() {
        let bars = time_bars(&ResponseBuckets {
            fast: 3,
            ok: 1,
            slow: 0,
            very_slow: 0,
        });
        assert_eq!(bars[0].width, 75);
        assert_eq!(bars[1].width, 25);
        assert_eq!(bars[3].class, "bg-err");
        assert_eq!(time_bars(&ResponseBuckets::default())[0].width, 0);
    }
}
