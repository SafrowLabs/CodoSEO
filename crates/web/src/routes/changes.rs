//! `/s/{site}/changes`: the latest finished crawl compared with the one before it. A diff
//! summary, the changes themselves (filterable by severity with `?sev=`), the health-score
//! history and the default alert rules.

use askama::Template;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use codoseo_core::change::ChangeKind;
use codoseo_core::check::Severity;
use codoseo_store::crawls::{self, Crawl};
use codoseo_store::reports::{self, ChangeCounts, ChangeRow, HealthPoint};
use codoseo_store::sites::Site;
use serde::Deserialize;
use url::Url;

use crate::auth::{CurrentUser, load_site};
use crate::error::AppError;
use crate::fmt;
use crate::layout::{Screen, Shell};
use crate::render::html;
use crate::routes::audit::site_id;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/s/{site}/changes", get(page))
}

/// Most change rows one page shows; the tab counts still cover them all.
const MAX_ROWS: i64 = 500;
/// Finished crawls in the health-score chart.
const HISTORY: i64 = 14;

#[derive(Template)]
#[template(path = "changes/index.html")]
pub struct ChangesPage {
    pub shell: Shell,
    pub base: String,
    /// Set when there is nothing to compare yet.
    pub empty: Option<EmptyState>,
    pub cmp: Option<Comparison>,
}

pub struct EmptyState {
    pub title: &'static str,
    pub text: String,
    /// Offer Run crawl (no crawl is queued or running).
    pub can_run: bool,
}

pub struct Comparison {
    /// `#47 · Oct 2`
    pub from_chip: String,
    pub to_chip: String,
    pub tiles: Vec<Tile>,
    pub tabs: Vec<Tab>,
    pub rows: Vec<ChangeView>,
    /// Shown instead of rows: `Nothing changed between #47 and #48.`
    pub nothing: Option<String>,
    /// Shown when the current tab has no changes but others do.
    pub tab_empty: Option<String>,
    /// `Showing the first 500 of 1,234 changes.`
    pub more: Option<String>,
    pub spark: Vec<SparkBar>,
    pub spark_first: String,
    pub spark_last: String,
    /// `86 · ↓ 7`
    pub score_aside: String,
    pub rules: Vec<Rule>,
}

pub struct Tile {
    pub label: &'static str,
    pub value: String,
    /// Text colour class, or empty.
    pub class: &'static str,
}

pub struct Tab {
    pub label: &'static str,
    pub href: String,
    pub n: String,
    pub active: bool,
}

pub struct ChangeView {
    /// `critical`, `warning`, `notice`
    pub severity: &'static str,
    pub severity_label: &'static str,
    pub title: String,
    /// The URL's path and query; `None` for site-wide changes.
    pub path: Option<String>,
    /// Explorer selection link, when the URL is a page on the site that the latest crawl has.
    pub href: Option<String>,
    pub mode: &'static str,
    pub mode_icon: &'static str,
    pub before: String,
    pub after: String,
    /// ` critical`, ` warning` or empty, appended to the `.after` class.
    pub after_class: &'static str,
}

pub struct SparkBar {
    pub height: u32,
    /// A background class, or empty for the soft default.
    pub class: &'static str,
    pub tip: String,
}

pub struct Rule {
    pub label: &'static str,
    pub mode: &'static str,
    pub instant: bool,
}

#[derive(Deserialize)]
pub struct ChangesQuery {
    sev: Option<String>,
}

fn parse_severity(raw: Option<&str>) -> Option<Severity> {
    match raw? {
        "critical" => Some(Severity::Critical),
        "warning" => Some(Severity::Warning),
        "notice" => Some(Severity::Notice),
        _ => None,
    }
}

async fn page(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(site): Path<String>,
    Query(q): Query<ChangesQuery>,
) -> Result<Response, AppError> {
    let site = load_site(&state, &user, site_id(&site)?).await?;
    let shell = Shell::load(&state, &user, Some(&site), Screen::Changes).await?;
    let base = format!("/s/{}", site.id);
    let pool = &state.pool;

    let latest = crawls::latest_done(pool, site.id).await?;
    let previous = match &latest {
        Some(l) => crawls::previous_done(pool, site.id, l.id).await?,
        None => None,
    };
    let has_done = latest.is_some();
    let (Some(latest), Some(previous)) = (latest, previous) else {
        let can_run = crawls::active(pool, site.id).await?.is_none();
        let empty = if has_done {
            EmptyState {
                title: "One crawl so far",
                text: "Changes appear after your second crawl.".to_owned(),
                can_run,
            }
        } else {
            EmptyState {
                title: "No finished crawl yet",
                text: if can_run {
                    "Each crawl is compared with the one before it. Run your first crawl to start."
                        .to_owned()
                } else {
                    "Your first crawl is on its way. Changes appear after the second one."
                        .to_owned()
                },
                can_run,
            }
        };
        return Ok(html(&ChangesPage {
            shell,
            base,
            empty: Some(empty),
            cmp: None,
        })?
        .into_response());
    };

    let severity = parse_severity(q.sev.as_deref());
    let counts = reports::change_kind_counts(pool, latest.id).await?;
    let rows = reports::changes_for_crawl(pool, latest.id, severity, MAX_ROWS).await?;
    let history = reports::health_history(pool, site.id, HISTORY).await?;

    let in_tab = severity.map_or(counts.total(), |s| counts.severity(s));
    let views = rows
        .iter()
        .map(|c| change_view(&site, &base, c, &previous, &latest))
        .collect::<Vec<_>>();
    let nothing = (counts.total() == 0).then(|| {
        format!(
            "Nothing changed between #{} and #{}.",
            previous.number, latest.number
        )
    });
    let tab_empty = (nothing.is_none() && views.is_empty()).then(|| match severity {
        Some(s) => format!(
            "No {} changes in this crawl.",
            severity_label(s).1.to_lowercase()
        ),
        None => "No changes in this crawl.".to_owned(),
    });
    let more = (in_tab > views.len() as i64).then(|| {
        format!(
            "Showing the first {} of {} changes.",
            fmt::thousands(views.len() as i64),
            fmt::thousands(in_tab)
        )
    });
    let (spark, spark_first, spark_last) = spark(&history);

    let cmp = Comparison {
        from_chip: chip(&previous),
        to_chip: chip(&latest),
        tiles: tiles(&counts),
        tabs: tabs(&base, &counts, severity),
        rows: views,
        nothing,
        tab_empty,
        more,
        spark,
        spark_first,
        spark_last,
        score_aside: score_aside(&latest, &previous),
        rules: rules(),
    };
    Ok(html(&ChangesPage {
        shell,
        base,
        empty: None,
        cmp: Some(cmp),
    })?
    .into_response())
}

/// `#47 · Oct 2`
fn chip(c: &Crawl) -> String {
    match c.finished_at {
        Some(at) => format!("#{} · {}", c.number, fmt::date(at)),
        None => format!("#{}", c.number),
    }
}

fn tiles(c: &ChangeCounts) -> Vec<Tile> {
    let tone = |n: i64, class| if n > 0 { class } else { "" };
    let new = c.kind(ChangeKind::NewUrl);
    let removed = c.kind(ChangeKind::RemovedUrl);
    let noindex = c.kind(ChangeKind::BecameNoindex);
    let titles = c.kind(ChangeKind::TitleChanged) + c.kind(ChangeKind::TitleRemoved);
    vec![
        Tile {
            label: "New URLs",
            value: format!("+{}", fmt::thousands(new)),
            class: tone(new, "c-ok"),
        },
        Tile {
            label: "Removed URLs",
            value: format!("−{}", fmt::thousands(removed)),
            class: tone(removed, "c-err"),
        },
        Tile {
            label: "Status changed",
            value: fmt::thousands(c.kind(ChangeKind::StatusChanged)),
            class: "",
        },
        Tile {
            label: "Became non-indexable",
            value: fmt::thousands(noindex),
            class: tone(noindex, "c-err"),
        },
        Tile {
            label: "Titles changed",
            value: fmt::thousands(titles),
            class: "",
        },
    ]
}

fn tabs(base: &str, c: &ChangeCounts, current: Option<Severity>) -> Vec<Tab> {
    let mut tabs = vec![Tab {
        label: "All",
        href: format!("{base}/changes"),
        n: fmt::thousands(c.total()),
        active: current.is_none(),
    }];
    for s in [Severity::Critical, Severity::Warning, Severity::Notice] {
        let (slug, label) = severity_label(s);
        tabs.push(Tab {
            label,
            href: format!("{base}/changes?sev={slug}"),
            n: fmt::thousands(c.severity(s)),
            active: current == Some(s),
        });
    }
    tabs
}

fn severity_label(s: Severity) -> (&'static str, &'static str) {
    match s {
        Severity::Critical => ("critical", "Critical"),
        Severity::Warning => ("warning", "Warning"),
        Severity::Notice => ("notice", "Notice"),
    }
}

/// The change kinds that alert instantly under the default rules (spec section 10), matching
/// the alert jobs `finalize` queues. Everything else waits for the Monday digest.
fn is_instant(kind: ChangeKind) -> bool {
    matches!(
        kind,
        ChangeKind::BecameNoindex
            | ChangeKind::ErrorSpike
            | ChangeKind::RobotsTxtChanged
            | ChangeKind::SitemapShrank
    )
}

fn title(c: &ChangeRow) -> String {
    match c.kind {
        ChangeKind::NewUrl => "New URL".to_owned(),
        ChangeKind::RemovedUrl => "Removed URL".to_owned(),
        ChangeKind::StatusChanged => format!("Status {} → {}", c.before, c.after),
        ChangeKind::BecameNoindex => "Became noindex".to_owned(),
        ChangeKind::TitleChanged => "Title changed".to_owned(),
        ChangeKind::TitleRemoved => "Title removed".to_owned(),
        ChangeKind::CanonicalChanged => "Canonical changed".to_owned(),
        ChangeKind::RedirectChainGrew => "Redirect chain grew".to_owned(),
        ChangeKind::RobotsTxtChanged => "robots.txt changed".to_owned(),
        ChangeKind::SitemapShrank => "Sitemap lost URLs".to_owned(),
        ChangeKind::ErrorSpike => "4xx/5xx spike".to_owned(),
        ChangeKind::SiteMoved => "Site moved".to_owned(),
    }
}

/// `example.com` and `www.example.com` are the same site.
fn same_site(host: &str, domain: &str) -> bool {
    let strip = |h: &str| h.trim_start_matches("www.").to_ascii_lowercase();
    strip(host) == strip(domain)
}

/// One side of the diff as people read it: counts get units, blanks say what they mean.
fn diff_value(c: &ChangeRow, value: &str, crawl: &Crawl) -> String {
    let count = |unit: &str, units: &str| match value.parse::<i64>() {
        Ok(n) => format!(
            "{} {}",
            fmt::thousands(n),
            if n == 1 { unit } else { units }
        ),
        Err(_) => value.to_owned(),
    };
    match c.kind {
        _ if value.is_empty() => match c.kind {
            ChangeKind::NewUrl | ChangeKind::RemovedUrl => {
                format!("not in crawl #{}", crawl.number)
            }
            ChangeKind::TitleRemoved => "no title".to_owned(),
            _ => "—".to_owned(),
        },
        ChangeKind::ErrorSpike => count("error page", "error pages"),
        ChangeKind::SitemapShrank => count("URL", "URLs"),
        ChangeKind::RedirectChainGrew => count("hop", "hops"),
        _ => value.to_owned(),
    }
}

fn change_view(
    site: &Site,
    base: &str,
    c: &ChangeRow,
    previous: &Crawl,
    latest: &Crawl,
) -> ChangeView {
    let (severity, severity_label) = severity_label(c.severity);
    let parsed = c.url.as_deref().and_then(|u| Url::parse(u).ok());
    let path = match (&parsed, &c.url) {
        (Some(u), _) => Some(match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_owned(),
        }),
        (None, Some(raw)) => Some(raw.clone()),
        (None, None) => None,
    };
    // A removed URL isn't in the latest crawl, so there is nothing to select in the explorer.
    let href = parsed
        .as_ref()
        .filter(|u| {
            c.kind != ChangeKind::RemovedUrl
                && u.host_str().is_some_and(|h| same_site(h, &site.domain))
        })
        .map(|u| {
            format!(
                "{base}/explorer?sel={:016x}",
                codoseo_core::url::url_hash(u)
            )
        });
    let instant = is_instant(c.kind);
    ChangeView {
        severity,
        severity_label,
        title: title(c),
        path,
        href,
        mode: if instant {
            "Instant alert"
        } else {
            "Weekly digest"
        },
        mode_icon: if instant { "i-bell" } else { "i-calendar" },
        before: diff_value(c, &c.before, previous),
        after: diff_value(c, &c.after, latest),
        after_class: match c.severity {
            Severity::Critical => " critical",
            Severity::Warning => " warning",
            Severity::Notice => "",
        },
    }
}

/// Bars scaled over the min..max score range (at least 6% tall). The latest bar is red when
/// the score dropped.
fn spark(history: &[HealthPoint]) -> (Vec<SparkBar>, String, String) {
    let min = history.iter().map(|h| h.score).min().unwrap_or(0);
    let max = history.iter().map(|h| h.score).max().unwrap_or(0);
    let dropped = match history {
        [.., a, b] => b.score < a.score,
        _ => false,
    };
    let last = history.len().saturating_sub(1);
    let bars = history
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let height = if max > min {
                6 + u32::try_from((h.score - min) * 94 / (max - min)).unwrap_or(0)
            } else {
                100
            };
            SparkBar {
                height,
                class: match (i == last, dropped) {
                    (true, true) => "bg-err",
                    (true, false) => "bg-ok",
                    _ => "",
                },
                tip: format!("#{} · {}", h.number, h.score),
            }
        })
        .collect();
    let label = |h: Option<&HealthPoint>| h.map(|h| format!("#{}", h.number)).unwrap_or_default();
    (bars, label(history.first()), label(history.last()))
}

/// `86 · ↓ 7`
fn score_aside(latest: &Crawl, previous: &Crawl) -> String {
    match (latest.health_score, previous.health_score) {
        (Some(a), Some(b)) if a < b => format!("{a} · ↓ {}", b - a),
        (Some(a), Some(b)) if a > b => format!("{a} · ↑ {}", a - b),
        (Some(a), _) => a.to_string(),
        (None, _) => String::new(),
    }
}

/// The default alert rules (spec section 10).
fn rules() -> Vec<Rule> {
    let instant = |label| Rule {
        label,
        mode: "Instant",
        instant: true,
    };
    vec![
        instant("Key page becomes noindex"),
        instant("Any 5xx or new 4xx spike"),
        instant("robots.txt changes"),
        instant("Sitemap loses 10%+ URLs"),
        Rule {
            label: "Everything else",
            mode: "Monday digest",
            instant: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(number: i64, score: i16) -> HealthPoint {
        HealthPoint { number, score }
    }

    #[test]
    fn spark_scales_over_the_range_and_flags_a_drop() {
        let (bars, first, last) = spark(&[point(3, 80), point(4, 90), point(5, 85)]);
        assert_eq!(
            bars.iter().map(|b| b.height).collect::<Vec<_>>(),
            [6, 100, 53]
        );
        assert_eq!(bars[2].class, "bg-err");
        assert_eq!(bars[0].class, "");
        assert_eq!((first.as_str(), last.as_str()), ("#3", "#5"));

        let (bars, ..) = spark(&[point(1, 70), point(2, 70)]);
        assert_eq!(bars[1].height, 100);
        assert_eq!(bars[1].class, "bg-ok");
        assert!(spark(&[]).0.is_empty());
    }

    #[test]
    fn severity_query_values() {
        assert_eq!(parse_severity(Some("warning")), Some(Severity::Warning));
        assert_eq!(parse_severity(Some("WARNING")), None);
        assert_eq!(parse_severity(None), None);
    }

    #[test]
    fn hosts_match_with_or_without_www() {
        assert!(same_site("www.Example.com", "example.com"));
        assert!(same_site("example.com", "www.example.com"));
        assert!(!same_site("other.com", "example.com"));
    }
}
