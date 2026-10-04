//! `/s/{site}/audit`: the site audit, the landing screen for a site. The health score and
//! KPIs, every failing check, and the response-code, depth and response-time charts, all from
//! the latest finished crawl. Before the first crawl finishes it shows that crawl's live
//! progress instead, refreshed from `/s/{site}/audit/live`.

use askama::Template;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use codoseo_checks::{Scope, def};
use codoseo_core::change::ChangeKind;
use codoseo_core::check::{CheckId, Severity};
use codoseo_core::output::StopReason;
use codoseo_core::report::CrawlSummary;
use codoseo_store::crawls::{self, Crawl, CrawlStatus, StoredSummary};
use codoseo_store::reports::{self, ResponseBuckets};
use codoseo_store::sites::Site;
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::{CurrentUser, load_site};
use crate::error::AppError;
use crate::fmt;
use crate::layout::{Screen, Shell};
use crate::render::{Hx, html};
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
}

impl AuditBody {
    /// The empty and failed states reload themselves when a crawl is queued.
    pub fn reloads_on_queue(&self) -> bool {
        self.done.is_none() && self.live.is_none()
    }

    /// The KPI labels, for the skeleton strip while the first crawl runs.
    pub fn kpi_labels(&self) -> [&'static str; 5] {
        KPI_LABELS
    }
}

const KPI_LABELS: [&str; 5] = [
    "Health score",
    "URLs crawled",
    "Indexable",
    "Avg response",
    "Crawl time",
];

pub struct DoneAudit {
    /// `37 of 40 checks passed`
    pub checks_chip: String,
    /// `page limit reached`, when the crawl stopped early.
    pub stop_chip: Option<&'static str>,
    pub kpis: Vec<Kpi>,
    pub issues: Vec<IssueRow>,
    pub codes: Vec<Segment>,
    /// `1,284 URLs`
    pub pages_label: String,
    pub depth: Vec<Bar>,
    pub times: Vec<HBar>,
    /// `avg 182 ms`
    pub times_aside: String,
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

/// One number of a KPI value plus its unit. Durations have two (`1` `m` `35` `s`).
pub struct KpiPart {
    /// The raw number for the count-up (`1284`, `90.2`).
    pub count: String,
    /// The same number, formatted (`1,284`).
    pub text: String,
    pub unit: String,
}

pub struct IssueRow {
    /// `critical`, `warning`, `notice`
    pub severity: &'static str,
    pub severity_label: &'static str,
    pub title: &'static str,
    /// Explorer link; `None` for site-wide checks.
    pub href: Option<String>,
    pub count: String,
    /// `12.4%`, or `—` for site-wide checks.
    pub pct: String,
    /// Minibar width relative to the largest count, 0–100.
    pub bar: u32,
    pub bar_class: &'static str,
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
    /// `queued` or `running`, for the status dot.
    pub state: &'static str,
    pub line: String,
    pub sub: String,
}

impl AuditLive {
    fn new(site: &Site, banner: bool, active: Option<&Crawl>) -> AuditLive {
        let base = format!("/s/{}", site.id);
        let Some(c) = active else {
            return AuditLive {
                base,
                banner,
                active: false,
                state: "idle",
                line: "Crawl finished".to_owned(),
                sub: "Loading the audit…".to_owned(),
            };
        };
        let running = c.status == CrawlStatus::Running;
        let progress = c.progress();
        let pages = progress.map_or(0, |p| p.pages_done);
        let (line, sub) = match (banner, running) {
            (true, true) => (
                format!(
                    "Crawl #{} running · {} pages",
                    c.number,
                    fmt::thousands(pages)
                ),
                String::new(),
            ),
            (true, false) => (format!("Crawl #{} queued", c.number), String::new()),
            (false, true) => (
                match progress {
                    Some(p) => format!(
                        "Crawling {} · {} pages · {}",
                        site.domain,
                        fmt::thousands(p.pages_done),
                        fmt::millis(p.elapsed_ms)
                    ),
                    None => format!("Crawling {} · starting", site.domain),
                },
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
    let body = load_body(&state, &site).await?;
    if hx.partial() {
        return Ok(html(&body)?.into_response());
    }
    let shell = Shell::load(&state, &user, Some(&site), Screen::Audit).await?;
    Ok(html(&AuditPage { shell, body })?.into_response())
}

#[derive(Deserialize)]
struct LiveQuery {
    view: Option<String>,
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
    Ok(html(&AuditLive::new(&site, banner, active.as_ref()))?.into_response())
}

async fn load_body(state: &AppState, site: &Site) -> Result<AuditBody, AppError> {
    let pool = &state.pool;
    let latest = crawls::latest_done(pool, site.id).await?;
    let active = crawls::active(pool, site.id).await?;
    let mut body = AuditBody {
        base: format!("/s/{}", site.id),
        domain: site.domain.clone(),
        done: None,
        live: None,
        failure: None,
    };
    match latest {
        Some(crawl) => {
            body.done = Some(done_audit(state, site, &crawl).await?);
            body.live = Some(AuditLive::new(site, true, active.as_ref()));
        }
        None if active.is_some() => {
            body.live = Some(AuditLive::new(site, false, active.as_ref()));
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

async fn done_audit(state: &AppState, site: &Site, crawl: &Crawl) -> Result<DoneAudit, AppError> {
    let pool = &state.pool;
    let base = format!("/s/{}", site.id);
    let stored = crawl.summary();
    let summary = stored
        .as_ref()
        .map(|s| s.report_summary.clone())
        .unwrap_or_default();
    let previous = crawls::previous_done(pool, site.id, crawl.id).await?;
    let changes = reports::change_kind_counts(pool, crawl.id).await?;
    let times = reports::response_time_buckets(pool, crawl.id).await?;

    let checks_chip = format!(
        "{} of {} checks passed",
        crawl.checks_passed.unwrap_or(0),
        crawl.checks_total.unwrap_or(0)
    );
    let stop_chip = stored.as_ref().and_then(|s| match s.stop_reason {
        StopReason::PageLimit => Some("page limit reached"),
        StopReason::TimeLimit => Some("time limit reached"),
        _ => None,
    });

    Ok(DoneAudit {
        checks_chip,
        stop_chip,
        kpis: kpis(
            crawl,
            &summary,
            previous.as_ref(),
            changes.kind(ChangeKind::NewUrl),
            changes.kind(ChangeKind::RemovedUrl),
        ),
        issues: stored
            .as_ref()
            .map(|s| issue_rows(&base, s))
            .unwrap_or_default(),
        codes: segments(&summary),
        pages_label: format!("{} URLs", fmt::thousands(summary.pages)),
        depth: depth_bars(&summary),
        times: time_bars(&times),
        times_aside: format!("avg {} ms", fmt::thousands(summary.avg_response_ms)),
    })
}

fn part(count: impl ToString, text: String, unit: &str) -> KpiPart {
    KpiPart {
        count: count.to_string(),
        text,
        unit: unit.to_owned(),
    }
}

fn kpis(
    crawl: &Crawl,
    s: &CrawlSummary,
    previous: Option<&Crawl>,
    new_urls: i64,
    removed_urls: i64,
) -> Vec<Kpi> {
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
            fmt::thousands(new_urls),
            fmt::thousands(removed_urls)
        )
    } else {
        "— first crawl".to_owned()
    };

    let pct = fmt::pct1(u64::from(s.indexable), u64::from(s.pages));

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

    let duration = crawl.duration().unwrap_or_default();
    let secs = duration.as_seconds_f64();
    let rate = match f64::from(s.pages) / secs {
        _ if secs < 0.5 => "— URL/s".to_owned(),
        r if r < 0.1 => "< 0.1 URL/s".to_owned(),
        r => format!("{r:.1} URL/s"),
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
            delta: format!(
                "{} of {}",
                fmt::thousands(s.indexable),
                fmt::thousands(s.pages)
            ),
            tone: "",
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
            swatch: "bg-accent",
            parts: duration_parts(&fmt::duration(duration)),
            delta: rate,
            tone: "",
        },
    ]
}

/// `1m 35s` -> `[1 m] [35 s]`, so each number can count up on its own.
fn duration_parts(formatted: &str) -> Vec<KpiPart> {
    formatted
        .split_whitespace()
        .map(|piece| {
            let split = piece
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(piece.len());
            let (digits, unit) = piece.split_at(split);
            let n: u64 = digits.parse().unwrap_or(0);
            part(n, n.to_string(), unit)
        })
        .collect()
}

fn severity_label(s: Severity) -> (&'static str, &'static str, &'static str) {
    match s {
        Severity::Critical => ("critical", "Critical", "bg-err"),
        Severity::Warning => ("warning", "Warning", "bg-warn"),
        Severity::Notice => ("notice", "Notice", "bg-ghost"),
    }
}

/// Failing checks, critical first, then by affected pages.
fn issue_rows(base: &str, s: &StoredSummary) -> Vec<IssueRow> {
    let mut failing: Vec<(CheckId, u32)> = s
        .counts
        .iter()
        .filter_map(|(slug, n)| Some((CheckId::from_slug(slug)?, *n)))
        .collect();
    failing.sort_by_key(|&(id, n)| (def(id).severity, std::cmp::Reverse(n), id));
    let max = failing
        .iter()
        .filter(|(id, _)| def(*id).scope != Scope::SiteWide)
        .map(|&(_, n)| n)
        .max()
        .unwrap_or(0)
        .max(1);
    let pages = u64::from(s.report_summary.pages);
    failing
        .into_iter()
        .map(|(id, n)| {
            let d = def(id);
            let (severity, severity_label, bar_class) = severity_label(d.severity);
            let site_wide = d.scope == Scope::SiteWide;
            IssueRow {
                severity,
                severity_label,
                title: d.title,
                href: (!site_wide).then(|| format!("{base}/explorer?filter=check:{}", id.slug())),
                count: if site_wide {
                    "—".to_owned()
                } else {
                    fmt::thousands(n)
                },
                pct: if site_wide {
                    "—".to_owned()
                } else {
                    format!("{}%", fmt::pct1(u64::from(n), pages))
                },
                bar: if site_wide { 0 } else { (n * 100 / max).max(2) },
                bar_class,
            }
        })
        .collect()
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
        }
    }

    #[test]
    fn issues_sort_by_severity_then_count() {
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
        let rows = issue_rows("/s/x", &s);
        let titles: Vec<_> = rows.iter().map(|r| r.severity).collect();
        assert_eq!(rows.len(), 5, "unknown slugs are skipped");
        assert_eq!(titles[0], "critical");
        assert_eq!(
            rows[0].href.as_deref(),
            Some("/s/x/explorer?filter=check:http_4xx")
        );
        // Warnings by count: description_missing (9) before title_missing (3).
        assert!(
            rows[1]
                .href
                .as_deref()
                .unwrap()
                .ends_with("description_missing")
        );
        assert!(rows[2].href.as_deref().unwrap().ends_with("title_missing"));
        assert_eq!(rows[1].pct, "9.0%");
        // Site-wide checks have no link and no share of the crawl.
        let site_wide = rows.iter().find(|r| r.href.is_none()).unwrap();
        assert_eq!(site_wide.pct, "—");
        // The largest page count gets the full minibar.
        assert_eq!(rows.iter().map(|r| r.bar).max(), Some(100));
        assert!(issue_rows("/s/x", &stored(vec![], 10)).is_empty());
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
    fn durations_split_into_count_up_parts() {
        let parts = duration_parts("1m 35s");
        assert_eq!(parts.len(), 2);
        assert_eq!(
            (parts[0].count.as_str(), parts[0].unit.as_str()),
            ("1", "m")
        );
        assert_eq!(
            (parts[1].count.as_str(), parts[1].unit.as_str()),
            ("35", "s")
        );
        let parts = duration_parts("2h 05m");
        assert_eq!(parts[1].text, "5");
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
