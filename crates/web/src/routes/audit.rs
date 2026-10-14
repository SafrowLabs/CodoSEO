//! `/s/{site}/audit`: the site audit, the landing screen for a site. From the top: the AI & GEO
//! section (access and checks, never part of the score), the headline KPIs, the health score
//! over the last crawls with the fixes that would raise it most, issues by severity over time
//! and by category, then what changed, response codes, response time and crawl depth. All of
//! it comes from the latest finished crawl, the stored summaries of the ones before it and the
//! latest AI access report; nothing here reads per-page rows except the response-time buckets.
//! Before the first crawl finishes it shows that crawl's live progress instead, refreshed from
//! `/s/{site}/audit/live`.

use askama::Template;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use codoseo_checks::{CHECKS, Category, Scope, def, points_regained};
use codoseo_core::change::ChangeKind;
use codoseo_core::check::{CheckId, Severity};
use codoseo_core::output::StopReason;
use codoseo_core::report::CrawlSummary;
use codoseo_store::crawls::{self, Crawl, CrawlStatus, StoredSummary};
use codoseo_store::geo;
use codoseo_store::reports::{self, ChangeCounts, ResponseBuckets};
use codoseo_store::sites::{self, Site};
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ai_incident;
use crate::auth::{CurrentUser, load_site};
use crate::error::AppError;
use crate::fmt;
use crate::layout::{Screen, Shell};
use crate::live::LiveCrawl;
use crate::render::{Hx, html};
use crate::routes::audit_ai::{
    self, GEO_CHECKS, GeoChecks, GeoSection, IncidentLink, ProbeSetting,
};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/s/{site}/audit", get(page))
        .route("/s/{site}/audit/live", get(live))
}

/// The site ID from the path; anything that isn't a UUID is a 404, like a missing site.
pub fn site_id(raw: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(raw).map_err(|_| AppError::NotFound)
}

/// Finished crawls the health score trend covers.
const TREND_CRAWLS: i64 = 14;
/// Finished crawls the issues-by-severity chart covers.
const SEVERITY_CRAWLS: usize = 8;
/// Fixes listed before "Show all".
const FIXES_SHOWN: usize = 5;
/// Change kinds listed under "Changed since last crawl".
const CHANGES_SHOWN: usize = 6;

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
    /// Why the latest crawl failed, when nothing has finished yet.
    pub failure: Option<String>,
    /// The public demo: sample data and no actions (no Run crawl, export or links into a site).
    pub demo: bool,
}

impl AuditBody {
    /// The empty and failed states reload themselves when a crawl is queued.
    pub fn reloads_on_queue(&self) -> bool {
        self.done.is_none() && self.live.is_none()
    }

    /// The KPI labels, for the skeleton strip while the first crawl runs.
    pub fn kpi_labels(&self) -> [&'static str; 6] {
        KPI_LABELS
    }
}

const KPI_LABELS: [&str; 6] = [
    "Health score",
    "URLs crawled",
    "Indexable",
    "Avg response",
    "Failing checks",
    "AI access",
];

pub struct DoneAudit {
    /// `38 / 44 SEO checks passed`
    pub seo_chip: Chip,
    /// `4 / 7 AI & GEO checks passed`; none before the site's first AI access report.
    pub geo_chip: Option<Chip>,
    /// `page limit reached`, when the crawl stopped early.
    pub stop_chip: Option<&'static str>,
    /// `Crawl #14 · finished 2h ago · 4m 12s · 3.1 URL/s · next crawl Oct 20, 06:00 UTC`
    pub meta: String,
    /// When the crawl finished, in full.
    pub finished: String,
    pub geo: GeoBlock,
    pub kpis: Vec<Kpi>,
    pub trend: Trend,
    /// The fixes worth most, then the rest behind "Show all".
    pub fixes: Vec<FixRow>,
    pub more_fixes: Vec<FixRow>,
    pub severity: Vec<SeverityBar>,
    /// `last 8 crawls`
    pub severity_aside: String,
    pub categories: Vec<CategoryBar>,
    pub changes: ChangesCard,
    pub codes: Vec<Segment>,
    /// `1,284 URLs`
    pub pages_label: String,
    pub depth: Vec<Bar>,
    pub times: Vec<HBar>,
    /// `avg 182 ms`
    pub times_aside: String,
}

pub struct Chip {
    pub text: String,
    /// Badge tone class.
    pub class: &'static str,
    pub tip: String,
}

/// The AI & GEO section: the report's view, or why there is none yet, and the incidents.
pub struct GeoBlock {
    pub section: Option<GeoSection>,
    /// Shown instead of the section before the first report.
    pub empty: &'static str,
    /// The incidents panel; none before the first report.
    pub panel: Option<AiPanel>,
}

/// The incidents panel in the AI & GEO section.
pub struct AiPanel {
    /// Nothing open, accepted or snoozed.
    pub clear: bool,
    /// Open, accepted and snoozed, each with its counts by severity.
    pub rows: Vec<AiPanelRow>,
    /// `Weakest evidence: B`, over the open incidents.
    pub weakest: Option<String>,
    /// `2 fixes being verified`
    pub verifying: Option<String>,
    /// The first open incidents by name, each linked to its card (Fix, Recheck).
    pub links: Vec<IncidentLink>,
    /// `and 2 more`
    pub more: Option<String>,
}

pub struct AiPanelRow {
    pub label: &'static str,
    pub total: u32,
    /// `(badge class, text)`, most severe first, only the severities that have any.
    pub chips: Vec<(&'static str, String)>,
}

pub struct Kpi {
    pub label: &'static str,
    /// Swatch background class.
    pub swatch: &'static str,
    pub parts: Vec<KpiPart>,
    pub delta: String,
    /// `up`, `down` or empty.
    pub tone: &'static str,
}

/// One number of a KPI value plus its unit.
pub struct KpiPart {
    /// The raw number for the count-up (`1284`, `90.2`); empty for a value that isn't one.
    pub count: String,
    /// The same number, formatted (`1,284`).
    pub text: String,
    pub unit: String,
}

/// The health score over the last crawls, as an inline SVG drawn in a 640×180 box.
pub struct Trend {
    /// Fewer than two scored crawls: the chart waits, and says why.
    pub empty: Option<String>,
    /// `polyline` points.
    pub points: String,
    /// The filled area under the line.
    pub area: String,
    /// What the chart shows, for screen readers.
    pub label: String,
    /// `last 14 crawls · 71 → 87`
    pub aside: String,
    /// Horizontal grid lines: `(y in the box, value, top in %)`.
    pub grid: Vec<(f64, i64, f64)>,
    /// Crawl labels under the chart: `(#n, left in %)`.
    pub axis: Vec<(String, f64)>,
    /// The latest point, `(left %, top %, score)`, marked with a dot.
    pub last: (f64, f64, i16),
    /// `Best gain: crawl #12 (+3)`
    pub note: Option<String>,
}

/// One failing check with what fixing it gives back.
pub struct FixRow {
    pub severity: &'static str,
    pub severity_label: &'static str,
    pub title: &'static str,
    /// Explorer link; `None` for site-wide checks.
    pub href: Option<String>,
    /// `14 pages`, or `site-wide`.
    pub pages: String,
    pub category: &'static str,
    /// `+6.2`
    pub gain: String,
}

/// One crawl's failing pages by severity, stacked.
pub struct SeverityBar {
    pub label: String,
    /// Segment heights in % of the tallest bar.
    pub critical: u32,
    pub warning: u32,
    pub notice: u32,
    pub total: String,
    pub tip: String,
}

pub struct CategoryBar {
    pub label: &'static str,
    pub count: String,
    pub width: u32,
}

pub struct ChangesCard {
    pub href: String,
    pub rows: Vec<ChangeLine>,
    /// Shown instead of the rows.
    pub empty: Option<String>,
}

pub struct ChangeLine {
    pub label: &'static str,
    pub count: String,
    /// Dot colour class.
    pub dot: &'static str,
}

/// A response-code class: one stackbar segment and one legend entry.
pub struct Segment {
    pub label: &'static str,
    pub class: &'static str,
    pub n: u32,
    pub count: String,
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
                "Your audit appears here the moment the crawl finishes.".to_owned(),
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
    let probes_installed = state.config.probe_urls_for(user.account.plan) > 0;
    let body = load_body(&state, &site, probes_installed).await?;
    if hx.partial() {
        return Ok(html(&body)?.into_response());
    }
    let shell = Shell::load(&state, &user, Some(&site), Screen::Audit).await?;
    Ok(html(&AuditPage { shell, body })?.into_response())
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

async fn load_body(
    state: &AppState,
    site: &Site,
    probes_installed: bool,
) -> Result<AuditBody, AppError> {
    let pool = &state.pool;
    let latest = crawls::latest_done(pool, site.id).await?;
    let active = crawls::active(pool, site.id).await?;
    let mut body = AuditBody {
        base: format!("/s/{}", site.id),
        domain: site.domain.clone(),
        done: None,
        live: None,
        failure: None,
        demo: false,
    };
    match latest {
        Some(crawl) => {
            body.done = Some(done_audit(state, site, &crawl, probes_installed).await?);
            body.live = Some(AuditLive::new(site, true, active.as_ref(), None));
        }
        None if active.is_some() => {
            body.live = Some(AuditLive::new(site, false, active.as_ref(), None));
        }
        None => {
            let last = crawls::history(pool, site.id, 1).await?;
            body.failure = last
                .into_iter()
                .find(|c| c.status == CrawlStatus::Failed)
                .map(|c| {
                    c.failure_reason
                        .unwrap_or_else(|| "The crawl stopped unexpectedly.".to_owned())
                });
        }
    }
    Ok(body)
}

/// What the AI & GEO parts of the screen read, loaded once.
pub(crate) struct GeoData {
    checks: Option<GeoChecks>,
    block: GeoBlock,
    /// Open incidents asking for attention, and how many fixes are being verified.
    open: Option<(u32, u32, u32)>,
}

const GEO_EMPTY: &str = "Each crawl checks robots.txt for every known AI bot, what your pages hold in their HTML and which answer engines may quote them. The first report appears with the next crawl.";

async fn geo_data(
    pool: &sqlx::PgPool,
    site: &Site,
    probes: ProbeSetting,
) -> Result<GeoData, AppError> {
    let base = format!("/s/{}", site.id);
    let Some(latest) = geo::latest_report(pool, site.id).await? else {
        return Ok(GeoData {
            checks: None,
            block: GeoBlock {
                section: None,
                empty: GEO_EMPTY,
                panel: None,
            },
            open: None,
        });
    };
    let intent = geo::get_intent(pool, site.id).await?;
    let open = geo::open_incidents(pool, site.id).await?;
    let rechecks = geo::latest_rechecks(pool, site.id).await?;
    Ok(geo_view(
        &base,
        &latest.report,
        &intent,
        &open,
        &rechecks,
        probes,
        time::OffsetDateTime::now_utc(),
    ))
}

/// The AI & GEO parts of the screen from an access report and the site's incidents, without the
/// database (the public demo draws it from a fixture).
pub(crate) fn geo_view(
    base: &str,
    report: &codoseo_geo::report::AccessReport,
    intent: &codoseo_geo::Intent,
    open: &[geo::Incident],
    rechecks: &std::collections::HashMap<i64, geo::Recheck>,
    probes: ProbeSetting,
    now: time::OffsetDateTime,
) -> GeoData {
    let counts = ai_incident::panel_counts(open, rechecks, now);
    let mut panel = ai_panel_view(&counts);
    panel.links = audit_ai::incident_links(base, open, now);
    let asking = counts.open.total();
    panel.more = (asking as usize > panel.links.len())
        .then(|| format!("and {} more", asking as usize - panel.links.len()));
    GeoData {
        checks: Some(GeoChecks::of(report, intent)),
        block: GeoBlock {
            section: Some(audit_ai::section(report, intent, probes)),
            empty: "",
            panel: Some(panel),
        },
        open: Some((asking, counts.open.critical, counts.verifying)),
    }
}

async fn done_audit(
    state: &AppState,
    site: &Site,
    crawl: &Crawl,
    probes_installed: bool,
) -> Result<DoneAudit, AppError> {
    let pool = &state.pool;
    // Oldest first, ending with `crawl` (both are the latest finished crawl, chosen alike).
    let recent = crawls::recent_done(pool, site.id, TREND_CRAWLS).await?;
    let changes = reports::change_kind_counts(pool, crawl.id).await?;
    let times = reports::response_time_buckets(pool, crawl.id).await?;
    let next = sites::next_crawl_at(pool, site.id).await?;
    let geo = geo_data(
        pool,
        site,
        ProbeSetting {
            site_on: site.ai_probes,
            installed: probes_installed,
        },
    )
    .await?;
    Ok(done_view(DoneInput {
        base: format!("/s/{}", site.id),
        crawl,
        recent: &recent,
        changes: &changes,
        times: &times,
        next,
        geo,
    }))
}

/// What a finished audit is drawn from: the database loads it ([`done_audit`]); the public demo
/// builds it from a fixture.
pub(crate) struct DoneInput<'a> {
    /// `/s/{site}`, which the screen's links hang off.
    pub base: String,
    /// The latest finished crawl.
    pub crawl: &'a Crawl,
    /// Up to [`TREND_CRAWLS`] finished crawls, oldest first, ending with `crawl`.
    pub recent: &'a [Crawl],
    pub changes: &'a ChangeCounts,
    pub times: &'a ResponseBuckets,
    /// The next scheduled crawl.
    pub next: Option<time::OffsetDateTime>,
    pub geo: GeoData,
}

pub(crate) fn done_view(input: DoneInput<'_>) -> DoneAudit {
    let DoneInput {
        base,
        crawl,
        recent,
        changes,
        times,
        next,
        geo,
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

    let total = i64::from(crawl.checks_total.unwrap_or(0));
    let passed = i64::from(crawl.checks_passed.unwrap_or(0));
    let critical_failing = stored.as_ref().is_some_and(|s| {
        s.counts.iter().any(|(slug, _)| {
            CheckId::from_slug(slug).is_some_and(|id| def(id).severity == Severity::Critical)
        })
    });
    let seo_chip = Chip {
        text: format!("{passed} / {total} SEO checks passed"),
        class: if passed == total {
            "t-ok"
        } else if critical_failing {
            "t-err"
        } else {
            "t-warn"
        },
        tip: "The checks behind the health score".to_owned(),
    };
    let geo_chip = geo.checks.map(|g| Chip {
        text: format!("{} / {GEO_CHECKS} AI & GEO checks passed", g.passed),
        class: if g.failing > 0 {
            "t-warn"
        } else if g.not_checked > 0 {
            "t-muted"
        } else {
            "t-ok"
        },
        tip: match g.not_checked {
            0 => "AI access checks: not part of the health score".to_owned(),
            n => format!(
                "AI access checks, not part of the health score. {n} couldn't be checked on this crawl"
            ),
        },
    });
    let stop_chip = stored.as_ref().and_then(|s| match s.stop_reason {
        StopReason::PageLimit => Some("page limit reached"),
        StopReason::TimeLimit => Some("time limit reached"),
        _ => None,
    });

    let mut meta = vec![format!("Crawl #{}", crawl.number)];
    if let Some(at) = crawl.finished_at {
        meta.push(format!("finished {}", fmt::ago(at)));
    }
    if let Some(d) = crawl.duration() {
        meta.push(fmt::duration(d));
        meta.push(rate(summary.pages, d));
    }
    meta.push(match next {
        Some(at) => format!("next crawl {}", fmt::until(at)),
        None => "no crawl scheduled".to_owned(),
    });

    let seo_failing = (total - passed).max(0);
    let mut fixes = stored
        .as_ref()
        .map(|s| fix_rows(&base, s))
        .unwrap_or_default();
    let more_fixes = fixes.split_off(fixes.len().min(FIXES_SHOWN));
    let summaries: Vec<(i64, StoredSummary)> = recent
        .iter()
        .filter_map(|c| Some((c.number, c.summary()?)))
        .collect();
    let history: Vec<(i64, &StoredSummary)> = summaries.iter().map(|(n, s)| (*n, s)).collect();
    let window = &history[history.len().saturating_sub(SEVERITY_CRAWLS)..];

    DoneAudit {
        seo_chip,
        geo_chip,
        stop_chip,
        meta: meta.join(" · "),
        finished: crawl.finished_at.map(fmt::datetime).unwrap_or_default(),
        kpis: kpis(KpiInput {
            crawl,
            summary: &summary,
            previous,
            new_urls: changes.kind(ChangeKind::NewUrl),
            removed_urls: changes.kind(ChangeKind::RemovedUrl),
            seo_failing,
            seo_total: total,
            geo_failing: geo.checks.map(|g| g.failing),
            open: geo.open,
        }),
        trend: trend(
            &recent
                .iter()
                .filter_map(|c| Some((c.number, c.health_score?)))
                .collect::<Vec<_>>(),
        ),
        fixes,
        more_fixes,
        severity: severity_bars(window),
        severity_aside: match window.len() {
            1 => "this crawl".to_owned(),
            n => format!("last {n} crawls"),
        },
        categories: stored.as_ref().map(category_bars).unwrap_or_default(),
        changes: changes_card(&base, changes, previous.map(|p| p.number)),
        geo: geo.block,
        codes: segments(&summary),
        pages_label: format!("{} URLs", fmt::thousands(summary.pages)),
        depth: depth_bars(&summary),
        times: time_bars(times),
        times_aside: format!("avg {} ms", fmt::thousands(summary.avg_response_ms)),
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

fn ai_panel_view(c: &ai_incident::PanelCounts) -> AiPanel {
    let row = |label, n: ai_incident::BySeverity| AiPanelRow {
        label,
        total: n.total(),
        chips: [
            ("sev-critical", n.critical, "critical"),
            ("sev-warning", n.warning, "warning"),
            ("sev-notice", n.notice, "notice"),
        ]
        .into_iter()
        .filter(|(_, count, _)| *count > 0)
        .map(|(class, count, name)| (class, format!("{count} {name}")))
        .collect(),
    };
    let rows = vec![
        row("Open", c.open),
        row("Accepted", c.accepted),
        row("Snoozed", c.snoozed),
    ];
    AiPanel {
        clear: rows.iter().all(|r| r.total == 0),
        rows,
        weakest: c
            .weakest_grade
            .map(|g| format!("Weakest evidence: {}", g.label())),
        verifying: match c.verifying {
            0 => None,
            1 => Some("1 fix being verified".to_owned()),
            n => Some(format!("{n} fixes being verified")),
        },
        links: Vec::new(),
        more: None,
    }
}

fn part(count: impl ToString, text: String, unit: &str) -> KpiPart {
    KpiPart {
        count: count.to_string(),
        text,
        unit: unit.to_owned(),
    }
}

struct KpiInput<'a> {
    crawl: &'a Crawl,
    summary: &'a CrawlSummary,
    previous: Option<&'a Crawl>,
    new_urls: i64,
    removed_urls: i64,
    seo_failing: i64,
    seo_total: i64,
    /// AI & GEO checks failing; none before the first report.
    geo_failing: Option<usize>,
    /// `(asking for attention, of them critical, fixes being verified)`.
    open: Option<(u32, u32, u32)>,
}

fn kpis(k: KpiInput) -> Vec<Kpi> {
    let KpiInput {
        crawl,
        summary: s,
        previous,
        ..
    } = k;
    let score = crawl.health_score.unwrap_or(0);
    let prev_score = previous.and_then(|p| Some((p.number, p.health_score?)));
    let (health_delta, health_tone) = match prev_score {
        None => ("— first crawl".to_owned(), ""),
        Some((n, prev)) if score < prev => (format!("↓ {} vs crawl #{n}", prev - score), "down"),
        Some((n, prev)) if score > prev => (format!("↑ {} vs crawl #{n}", score - prev), "up"),
        Some((n, _)) => (format!("no change vs crawl #{n}"), ""),
    };
    let health_swatch = match score {
        80.. => "bg-ok",
        50..80 => "bg-warn",
        _ => "bg-err",
    };

    let urls_delta = if previous.is_some() {
        format!(
            "+{} new · −{} removed",
            fmt::thousands(k.new_urls),
            fmt::thousands(k.removed_urls)
        )
    } else {
        "— first crawl".to_owned()
    };

    let pct = fmt::pct1(u64::from(s.indexable), u64::from(s.pages));
    let share = |sum: &CrawlSummary| f64::from(sum.indexable) * 100.0 / f64::from(sum.pages.max(1));
    let prev_share = previous.and_then(|p| Some((p.number, share(&p.summary()?.report_summary))));
    let (index_delta, index_tone) = match prev_share {
        None => (
            format!(
                "{} of {}",
                fmt::thousands(s.indexable),
                fmt::thousands(s.pages)
            ),
            "",
        ),
        Some((n, prev)) => {
            let d = share(s) - prev;
            if d.abs() < 0.05 {
                (format!("no change vs crawl #{n}"), "")
            } else if d > 0.0 {
                (format!("↑ {d:.1} pts vs crawl #{n}"), "up")
            } else {
                (format!("↓ {:.1} pts vs crawl #{n}", -d), "down")
            }
        }
    };

    // Faster is better, so a drop in response time is the good (`up`) colour.
    let prev_avg =
        previous.and_then(|p| Some((p.number, p.summary()?.report_summary.avg_response_ms)));
    let avg = s.avg_response_ms;
    let (avg_delta, avg_tone) = match prev_avg {
        None => ("— first crawl".to_owned(), ""),
        Some((n, prev)) if avg < prev => (
            format!("↓ {} ms vs crawl #{n}", fmt::thousands(prev - avg)),
            "up",
        ),
        Some((n, prev)) if avg > prev => (
            format!("↑ {} ms vs crawl #{n}", fmt::thousands(avg - prev)),
            "down",
        ),
        Some((n, _)) => (format!("no change vs crawl #{n}"), ""),
    };

    // SEO and AI & GEO checks together; the score reads only the SEO ones.
    let (failing, of, failing_delta) = match k.geo_failing {
        Some(g) => (
            k.seo_failing + g as i64,
            k.seo_total + GEO_CHECKS as i64,
            format!("{} SEO · {g} AI & GEO", k.seo_failing),
        ),
        None => (
            k.seo_failing,
            k.seo_total,
            format!("{} SEO · AI & GEO not checked yet", k.seo_failing),
        ),
    };

    let ai = match k.open {
        None => Kpi {
            label: KPI_LABELS[5],
            swatch: "bg-ghost",
            parts: vec![part("", "—".to_owned(), "")],
            delta: "no report yet".to_owned(),
            tone: "",
        },
        Some((open, critical, verifying)) => Kpi {
            label: KPI_LABELS[5],
            swatch: if critical > 0 {
                "bg-err"
            } else if open > 0 {
                "bg-warn"
            } else {
                "bg-ok"
            },
            parts: vec![part(open, fmt::thousands(open), "open")],
            delta: match (open, critical, verifying) {
                (_, _, v) if v > 0 => format!("{v} being verified"),
                (0, ..) => "nothing to fix".to_owned(),
                (_, 0, _) => "none critical".to_owned(),
                (_, c, _) => format!("{c} critical"),
            },
            tone: if critical > 0 { "down" } else { "" },
        },
    };

    vec![
        Kpi {
            label: KPI_LABELS[0],
            swatch: health_swatch,
            parts: vec![part(score, score.to_string(), "/100")],
            delta: health_delta,
            tone: health_tone,
        },
        Kpi {
            label: KPI_LABELS[1],
            swatch: "bg-ink",
            parts: vec![part(s.pages, fmt::thousands(s.pages), "")],
            delta: urls_delta,
            tone: "",
        },
        Kpi {
            label: KPI_LABELS[2],
            swatch: "bg-ok",
            parts: vec![part(&pct, pct.clone(), "%")],
            delta: index_delta,
            tone: index_tone,
        },
        Kpi {
            label: KPI_LABELS[3],
            swatch: "bg-blue",
            parts: vec![part(
                s.avg_response_ms,
                fmt::thousands(s.avg_response_ms),
                "ms",
            )],
            delta: avg_delta,
            tone: avg_tone,
        },
        Kpi {
            label: KPI_LABELS[4],
            swatch: if failing > 0 { "bg-warn" } else { "bg-ok" },
            parts: vec![part(failing, fmt::thousands(failing), &format!("of {of}"))],
            delta: failing_delta,
            tone: "",
        },
        ai,
    ]
}

/// The health score trend over `points` (`(crawl number, score)`, oldest first) in a 640×180
/// box. The vertical range is the scores' own, padded to tens and at least 20 points tall, so a
/// small move still reads as one.
fn trend(points: &[(i64, i16)]) -> Trend {
    const W: f64 = 640.0;
    const H: f64 = 180.0;
    let mut t = Trend {
        empty: None,
        points: String::new(),
        area: String::new(),
        label: String::new(),
        aside: String::new(),
        grid: Vec::new(),
        axis: Vec::new(),
        last: (100.0, 0.0, 0),
        note: None,
    };
    match points {
        [] => {
            t.empty = Some("No scored crawl yet.".to_owned());
            return t;
        }
        [(n, s)] => {
            t.empty = Some(format!(
                "Crawl #{n} scored {s}. The trend starts with the next crawl."
            ));
            t.aside = "1 crawl".to_owned();
            return t;
        }
        _ => {}
    }
    let min = points.iter().map(|p| i64::from(p.1)).min().unwrap_or(0);
    let max = points.iter().map(|p| i64::from(p.1)).max().unwrap_or(100);
    let mut lo = ((min - 5).max(0) / 10) * 10;
    let mut hi = (((max + 5) + 9) / 10 * 10).min(100);
    if hi - lo < 20 {
        lo = (hi - 20).max(0);
        hi = lo + 20;
    }
    let span = (hi - lo) as f64;
    let last_i = points.len() - 1;
    let x = |i: usize| i as f64 * W / last_i as f64;
    let y = |s: i16| H - (f64::from(s) - lo as f64) / span * H;
    let xy: Vec<String> = points
        .iter()
        .enumerate()
        .map(|(i, &(_, s))| format!("{:.1},{:.1}", x(i), y(s)))
        .collect();
    t.points = xy.join(" ");
    t.area = format!("M0,{H} L{} L{W},{H} Z", xy.join(" L"));
    let (first, last) = (points[0], points[last_i]);
    t.aside = format!("last {} crawls · {} → {}", points.len(), first.1, last.1);
    t.label = match last.1.cmp(&first.1) {
        std::cmp::Ordering::Equal => format!(
            "Health score stayed at {} over the last {} crawls",
            last.1,
            points.len()
        ),
        o => format!(
            "Health score {} from {} to {} over the last {} crawls",
            if o == std::cmp::Ordering::Greater {
                "rose"
            } else {
                "fell"
            },
            first.1,
            last.1,
            points.len()
        ),
    };
    t.grid = (1..=3)
        .map(|q| {
            let v = lo + (hi - lo) * q / 4;
            let top = 100.0 - (v - lo) as f64 / span * 100.0;
            (top / 100.0 * H, v, top)
        })
        .collect();
    let ticks = if points.len() <= 5 {
        (0..points.len()).collect::<Vec<_>>()
    } else {
        let mut v: Vec<usize> = (0..5).map(|k| (k * last_i + 2) / 4).collect();
        v.dedup();
        v
    };
    t.axis = ticks
        .into_iter()
        .map(|i| (format!("#{}", points[i].0), x(i) / W * 100.0))
        .collect();
    t.last = (100.0, y(last.1) / H * 100.0, last.1);
    t.note = points
        .windows(2)
        .map(|w| (w[1].0, w[1].1 - w[0].1))
        .filter(|&(_, d)| d > 0)
        .max_by_key(|&(n, d)| (d, n))
        .map(|(n, d)| format!("Best gain: crawl #{n} (+{d})"));
    t
}

fn severity_view(s: Severity) -> (&'static str, &'static str) {
    match s {
        Severity::Critical => ("critical", "Critical"),
        Severity::Warning => ("warning", "Warning"),
        Severity::Notice => ("notice", "Notice"),
    }
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
    }
}

/// The checks of a stored summary this version knows, with their counts.
fn known_counts(s: &StoredSummary) -> Vec<(CheckId, u32)> {
    s.counts
        .iter()
        .filter_map(|(slug, n)| Some((CheckId::from_slug(slug)?, *n)))
        .collect()
}

/// What one failing check weighs in the charts: its pages, or 1 for a site-wide fact.
fn weight(id: CheckId, n: u32) -> u32 {
    if def(id).scope == Scope::SiteWide {
        1
    } else {
        n
    }
}

/// Every failing check, the one whose fix gives back the most points first; ties go to the
/// more severe, then the more widespread.
fn fix_rows(base: &str, s: &StoredSummary) -> Vec<FixRow> {
    let counts = known_counts(s);
    let mut gains: Vec<(CheckId, u32, f64)> = points_regained(&counts, s.report_summary.pages)
        .into_iter()
        .zip(&counts)
        .map(|((id, g), &(_, n))| (id, n, g))
        .collect();
    gains.sort_by(|a, b| {
        b.2.total_cmp(&a.2)
            .then(def(a.0).severity.cmp(&def(b.0).severity))
            .then(b.1.cmp(&a.1))
            .then(a.0.cmp(&b.0))
    });
    gains
        .into_iter()
        .map(|(id, n, g)| {
            let d = def(id);
            let (severity, severity_label) = severity_view(d.severity);
            let site_wide = d.scope == Scope::SiteWide;
            FixRow {
                severity,
                severity_label,
                title: d.title,
                href: (!site_wide).then(|| format!("{base}/explorer?filter=check:{}", id.slug())),
                pages: if site_wide {
                    "site-wide".to_owned()
                } else {
                    fmt::pages(n)
                },
                category: category_label(d.category),
                gain: format!("+{g:.1}"),
            }
        })
        .collect()
}

/// Failing pages per crawl by severity (`(crawl number, summary)`, oldest first).
fn severity_bars(history: &[(i64, &StoredSummary)]) -> Vec<SeverityBar> {
    let totals: Vec<(i64, [u32; 3])> = history
        .iter()
        .map(|(n, s)| {
            let mut by = [0u32; 3];
            for (id, count) in known_counts(s) {
                by[def(id).severity as usize] += weight(id, count);
            }
            (*n, by)
        })
        .collect();
    let max = totals
        .iter()
        .map(|(_, b)| b.iter().sum::<u32>())
        .max()
        .unwrap_or(0)
        .max(1);
    let pct = |v: u32| {
        if v == 0 {
            0
        } else {
            (u64::from(v) * 100 / u64::from(max)).max(1) as u32
        }
    };
    totals
        .into_iter()
        .map(|(n, [c, w, no])| SeverityBar {
            label: format!("#{n}"),
            critical: pct(c),
            warning: pct(w),
            notice: pct(no),
            total: fmt::thousands(c + w + no),
            tip: format!(
                "Crawl #{n}: {} critical · {} warning · {} notice",
                fmt::thousands(c),
                fmt::thousands(w),
                fmt::thousands(no)
            ),
        })
        .collect()
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
    by.sort_by_key(|b| std::cmp::Reverse(b.1));
    let max = by.first().map_or(1, |b| b.1.max(1));
    by.into_iter()
        .map(|(c, n)| CategoryBar {
            label: category_label(c),
            count: fmt::thousands(n),
            width: (u64::from(n) * 100 / u64::from(max)).max(3) as u32,
        })
        .collect()
}

/// The crawl's changes per kind, the most severe first.
fn changes_card(base: &str, c: &ChangeCounts, previous: Option<i64>) -> ChangesCard {
    let mut by: Vec<(ChangeKind, Severity, i64)> = Vec::new();
    for &(kind, sev, n) in &c.rows {
        match by.iter_mut().find(|(k, ..)| *k == kind) {
            Some(e) => {
                e.1 = e.1.min(sev);
                e.2 += n;
            }
            None => by.push((kind, sev, n)),
        }
    }
    by.sort_by(|a, b| a.1.cmp(&b.1).then(b.2.cmp(&a.2)));
    let empty = match previous {
        _ if !by.is_empty() => None,
        None => Some("The first crawl: nothing to compare with yet.".to_owned()),
        Some(n) => Some(format!("Nothing changed since crawl #{n}.")),
    };
    ChangesCard {
        href: format!("{base}/changes"),
        rows: by
            .into_iter()
            .take(CHANGES_SHOWN)
            .map(|(kind, sev, n)| ChangeLine {
                label: kind.label(),
                count: if kind == ChangeKind::NewUrl {
                    format!("+{}", fmt::thousands(n))
                } else {
                    fmt::thousands(n)
                },
                dot: match sev {
                    Severity::Critical => "bg-err",
                    Severity::Warning => "bg-warn",
                    Severity::Notice => "bg-ghost",
                },
            })
            .collect(),
        empty,
    }
}

fn segments(s: &CrawlSummary) -> Vec<Segment> {
    let st = &s.status;
    [
        ("2xx", "bg-ok", st.ok),
        ("3xx", "bg-warn", st.redirect),
        ("4xx", "bg-err", st.client_error),
        ("5xx", "bg-5xx", st.server_error),
        ("No response", "bg-ghost", st.failed),
        ("Blocked", "bg-ink", st.blocked),
    ]
    .into_iter()
    .map(|(label, class, n)| Segment {
        label,
        class,
        n,
        count: fmt::thousands(n),
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
            let class = if i >= 5 { "bg-warn" } else { "bg-ok" };
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
        ("< 200 ms", t.fast, "bg-ok"),
        ("200–500", t.ok, "bg-ok"),
        ("500–1000", t.slow, "bg-warn"),
        ("> 1 s", t.very_slow, "bg-err"),
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
    use codoseo_core::report::StatusCounts;

    fn stored(counts: Vec<(&str, u32)>, pages: u32) -> StoredSummary {
        StoredSummary {
            stop_reason: StopReason::Completed,
            report_summary: CrawlSummary {
                pages,
                status: StatusCounts::default(),
                ..Default::default()
            },
            counts: counts.into_iter().map(|(s, n)| (s.to_owned(), n)).collect(),
            geo: None,
        }
    }

    #[test]
    fn fixes_rank_by_points_regained() {
        let s = stored(
            vec![
                ("og_missing", 40),
                ("title_missing", 3),
                ("http_4xx", 2),
                ("description_missing", 9),
                ("sitemap_missing", 1),
                ("from_a_newer_version", 5),
            ],
            100,
        );
        let rows = fix_rows("/s/x", &s);
        assert_eq!(rows.len(), 5, "unknown slugs are skipped");
        // The critical 4xx costs most even on 2 pages of 100.
        assert_eq!(
            rows[0].href.as_deref(),
            Some("/s/x/explorer?filter=check:http_4xx")
        );
        assert_eq!(rows[0].severity, "critical");
        assert_eq!(rows[0].pages, "2 pages");
        // Warnings: 9 missing descriptions give back more than 3 missing titles.
        let at = |slug: &str| {
            rows.iter()
                .position(|r| r.href.as_deref().is_some_and(|h| h.ends_with(slug)))
                .unwrap()
        };
        assert!(at("description_missing") < at("title_missing"));
        // Gains read as points with one decimal, and never go up the list.
        let gains: Vec<f64> = rows.iter().map(|r| r.gain.parse().unwrap()).collect();
        assert!(gains.windows(2).all(|w| w[0] >= w[1]), "{gains:?}");
        assert!(rows.iter().all(|r| r.gain.starts_with('+')));
        // Site-wide checks have no link and no page count.
        let site_wide = rows.iter().find(|r| r.href.is_none()).unwrap();
        assert_eq!(site_wide.pages, "site-wide");
        assert_eq!(site_wide.category, "Sitemap");
        assert!(fix_rows("/s/x", &stored(vec![], 10)).is_empty());
    }

    #[test]
    fn severity_bars_stack_each_crawl_against_the_tallest() {
        let a = stored(vec![("http_4xx", 4), ("title_missing", 4)], 10);
        let b = stored(vec![("http_4xx", 1), ("sitemap_missing", 1)], 10);
        let bars = severity_bars(&[(3, &a), (4, &b)]);
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].label, "#3");
        assert_eq!((bars[0].critical, bars[0].warning), (50, 50));
        assert_eq!(bars[0].total, "8");
        // A site-wide check counts once; the notice is 1 of the tallest bar's 8.
        assert_eq!(bars[1].notice, 12);
        assert!(bars[1].tip.contains("1 critical · 0 warning · 1 notice"));
        assert!(severity_bars(&[]).is_empty());
        let clean = stored(vec![], 10);
        assert_eq!(severity_bars(&[(1, &clean)])[0].total, "0");
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
    fn the_trend_needs_two_crawls_and_scales_to_its_scores() {
        assert!(trend(&[]).empty.is_some());
        let one = trend(&[(1, 80)]);
        assert!(one.empty.unwrap().contains("Crawl #1 scored 80"));

        let t = trend(&[(1, 71), (2, 74), (3, 73), (4, 87)]);
        assert!(t.empty.is_none());
        assert_eq!(t.aside, "last 4 crawls · 71 → 87");
        assert!(t.label.contains("rose from 71 to 87"), "{}", t.label);
        assert_eq!(t.points.split(' ').count(), 4);
        assert!(t.points.starts_with("0.0,"));
        assert!(t.points.ends_with(&format!("640.0,{:.1}", t.last.1 * 1.8)));
        assert!(t.area.starts_with("M0,180 L0.0,"));
        assert_eq!(t.note.as_deref(), Some("Best gain: crawl #4 (+14)"));
        assert_eq!(t.axis.first().unwrap().0, "#1");
        assert_eq!(t.axis.last().unwrap().0, "#4");
        // Every point sits inside the box.
        for p in t.points.split(' ') {
            let y: f64 = p.split(',').nth(1).unwrap().parse().unwrap();
            assert!((0.0..=180.0).contains(&y), "{p}");
        }
        assert_eq!(t.grid.len(), 3);

        let flat = trend(&[(5, 100), (6, 100)]);
        assert!(flat.label.contains("stayed at 100"), "{}", flat.label);
        assert!(flat.note.is_none());

        let many: Vec<(i64, i16)> = (1..=14).map(|n| (n, 60 + n as i16)).collect();
        let t = trend(&many);
        assert_eq!(t.axis.len(), 5);
        assert_eq!(t.axis[4].0, "#14");
        assert_eq!(t.axis[4].1, 100.0);
    }

    #[test]
    fn changes_list_the_worst_kinds_first() {
        let c = ChangeCounts {
            rows: vec![
                (ChangeKind::NewUrl, Severity::Notice, 12),
                (ChangeKind::BecameNoindex, Severity::Critical, 2),
                (ChangeKind::TitleChanged, Severity::Notice, 9),
                (ChangeKind::StatusChanged, Severity::Warning, 3),
                (ChangeKind::StatusChanged, Severity::Critical, 1),
            ],
        };
        let card = changes_card("/s/x", &c, Some(13));
        assert!(card.empty.is_none());
        let labels: Vec<&str> = card.rows.iter().map(|r| r.label).collect();
        assert_eq!(
            labels,
            [
                "Status changed",
                "Became noindex",
                "New URL",
                "Title changed"
            ]
        );
        assert_eq!(card.rows[0].count, "4");
        assert_eq!(card.rows[0].dot, "bg-err");
        assert_eq!(card.rows[2].count, "+12");
        assert_eq!(card.href, "/s/x/changes");

        let none = ChangeCounts::default();
        assert!(
            changes_card("/s/x", &none, None)
                .empty
                .unwrap()
                .contains("first crawl")
        );
        assert_eq!(
            changes_card("/s/x", &none, Some(3)).empty.as_deref(),
            Some("Nothing changed since crawl #3.")
        );
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
        assert_eq!(bars[4].class, "bg-ok");
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
