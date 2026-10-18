//! The site audit's "Score trend": the Overall, SEO and GEO scores of the last finished crawls
//! as one server-drawn SVG bar chart (a group of three bars per crawl), a sentence saying what happened, and numbered pins on the crawls
//! whose change records hold something serious, with what changed and what it did to the
//! scores. A policy change is always a pin of its own, on the crawl it judged again, so a score
//! jump can be traced to the edit behind it. A crawl without a GEO (or Overall) score has no bar for it; with fewer than
//! two crawls there is no chart, only when the next crawl is due.

use codoseo_core::change::ChangeKind;
use codoseo_core::check::Severity;
use codoseo_store::reports::ChangeCounts;
use time::OffsetDateTime;

use crate::fmt;

/// The chart's box, in % of the drawn chart, so the SVG (stretched to fit) and the HTML labels
/// over it share one scale: plot from `X0` to `X1`, and from `Y0` (100) down to `Y1` (0).
pub const W: f64 = 100.0;
pub const H: f64 = 100.0;
const X0: f64 = 7.0;
const X1: f64 = 100.0;
const Y0: f64 = 6.0;
const Y1: f64 = 92.0;
/// The share of a crawl's slot its three bars fill; the rest is the gap between crawls.
const GROUP: f64 = 0.72;
/// A group is never wider than with this many crawls, so two crawls don't draw slabs.
const MIN_SLOTS: f64 = 6.0;
/// Pins on the chart at most.
const PINS: usize = 4;

/// One finished crawl on the chart.
#[derive(Debug, Clone)]
pub struct TrendPoint {
    pub number: i64,
    pub at: OffsetDateTime,
    pub seo: Option<i16>,
    pub geo: Option<i16>,
    pub overall: Option<i16>,
    pub changes: ChangeCounts,
    /// The policy changes that judged this crawl again, oldest first.
    pub policy: Vec<PolicyPin>,
}

/// One policy change on the chart: who made it and what it did to the crawl's scores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyPin {
    /// `ann@example.com`, `the API`.
    pub who: String,
    pub health: (Option<i16>, Option<i16>),
    pub geo: (Option<i16>, Option<i16>),
}

/// One score of one crawl, as a bar from 0.
pub struct Bar {
    /// `overall`, `seo` or `geo`.
    pub key: &'static str,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// The latest crawl's bars stand out.
    pub last: bool,
    /// `Crawl #12 · Oct 3 · SEO 78`, the bar's tooltip.
    pub title: String,
}

pub struct Pin {
    pub n: usize,
    pub x: f64,
    /// Left edge in % of the box, for the button over the SVG.
    pub left: f64,
    /// `drop`, `rise` or `flat`.
    pub tone: &'static str,
    /// `Oct 3 · crawl #41`
    pub when: String,
    pub what: String,
    pub effect: String,
}

pub struct Trend {
    /// Fewer than two crawls: the placeholder's line about the next crawl.
    pub waiting: Option<String>,
    pub sentence: String,
    /// `Last 14 crawls`
    pub aside: String,
    pub bars: Vec<Bar>,
    /// The latest crawl's scores for the legend: `(key, "Overall", 75)`.
    pub latest: Vec<(&'static str, &'static str, i16)>,
    /// `(y, value)` of the grid lines.
    pub grid: Vec<(f64, i64)>,
    /// `(x, label, anchor)` under the chart.
    pub axis: Vec<(f64, String, &'static str)>,
    pub pins: Vec<Pin>,
    /// What the chart shows, for screen readers.
    pub label: String,
}

impl Trend {
    pub fn w(&self) -> f64 {
        W
    }

    pub fn h(&self) -> f64 {
        H
    }
}

/// One score on the chart: its key, its name and how to read it off a crawl.
type Series = (&'static str, &'static str, fn(&TrendPoint) -> Option<i16>);

/// The width of one crawl's slot.
fn slot(n: usize) -> f64 {
    (X1 - X0) / n.max(1) as f64
}

/// The middle of crawl `i`'s group of bars.
fn x_of(i: usize, n: usize) -> f64 {
    X0 + (i as f64 + 0.5) * slot(n)
}

/// The change behind a pin: the most severe kind, the most frequent first.
fn headline(c: &ChangeCounts) -> Option<(Severity, ChangeKind, i64)> {
    let mut by: Vec<(Severity, ChangeKind, i64)> = Vec::new();
    for &(kind, sev, n) in &c.rows {
        match by.iter_mut().find(|e| e.1 == kind) {
            Some(e) => {
                e.0 = e.0.min(sev);
                e.2 += n;
            }
            None => by.push((sev, kind, n)),
        }
    }
    by.sort_by(|a, b| a.0.cmp(&b.0).then(b.2.cmp(&a.2)));
    by.into_iter().next()
}

fn delta_text(name: &str, a: Option<i16>, b: Option<i16>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) if a != b => Some(format!("{name} {a} → {b}")),
        _ => None,
    }
}

/// The trend over `points` (oldest first). `next` is the next scheduled crawl, for the
/// placeholder; `now` names today on the axis.
pub fn trend(points: &[TrendPoint], next: Option<OffsetDateTime>, now: OffsetDateTime) -> Trend {
    let mut t = Trend {
        waiting: None,
        sentence: String::new(),
        aside: match points.len() {
            1 => "1 crawl".to_owned(),
            n => format!("Last {n} crawls"),
        },
        bars: Vec::new(),
        latest: Vec::new(),
        grid: Vec::new(),
        axis: Vec::new(),
        pins: Vec::new(),
        label: String::new(),
    };
    if points.len() < 2 {
        t.waiting = Some(match next {
            Some(at) => format!("Next crawl: {}. Or run one now.", fmt::until_from(at, now)),
            None => "No crawl is scheduled. Run one to start the trend.".to_owned(),
        });
        return t;
    }
    let n = points.len();
    // Bars start at 0, so their heights compare.
    let y = |v: i16| Y1 - f64::from(v.clamp(0, 100)) / 100.0 * (Y1 - Y0);
    t.grid = (0..=4)
        .map(|k| {
            let v = 25 * k;
            (y(v as i16), i64::from(v))
        })
        .collect();

    let series: [Series; 3] = [
        ("overall", "Overall", |p| p.overall),
        ("seo", "SEO", |p| p.seo),
        ("geo", "GEO", |p| p.geo),
    ];
    let group = slot(n).min((X1 - X0) / MIN_SLOTS) * GROUP;
    let w = group / series.len() as f64;
    for (i, p) in points.iter().enumerate() {
        let left = x_of(i, n) - group / 2.0;
        for (k, (key, name, get)) in series.iter().enumerate() {
            let Some(v) = get(p) else { continue };
            let top = y(v);
            t.bars.push(Bar {
                key,
                x: left + k as f64 * w,
                y: top,
                // A hairline gap between the three bars.
                w: w * 0.86,
                h: Y1 - top,
                last: i == n - 1,
                title: format!("Crawl #{} · {} · {name} {v}", p.number, fmt::date(p.at)),
            });
        }
    }
    t.latest = series
        .iter()
        .filter_map(|(key, name, get)| Some((*key, *name, get(&points[n - 1])?)))
        .collect();

    let day = |at: OffsetDateTime| {
        if at.date() == now.date() {
            "Today".to_owned()
        } else {
            fmt::date(at)
        }
    };
    t.axis = vec![
        (X0, fmt::date(points[0].at), "start"),
        (x_of(n / 2, n), fmt::date(points[n / 2].at), "middle"),
        (X1, day(points[n - 1].at), "end"),
    ];
    if n < 5 {
        t.axis.remove(1);
    }

    // Pins: the crawls with a critical or warning change, the biggest score moves first.
    let mut pins: Vec<(usize, Severity, i64, String, String, i32)> = Vec::new();
    for (i, p) in points.iter().enumerate().skip(1) {
        let Some((sev, kind, count)) = headline(&p.changes) else {
            continue;
        };
        if sev == Severity::Notice {
            continue;
        }
        let q = &points[i - 1];
        let moves: Vec<String> = [
            delta_text("Overall", q.overall, p.overall),
            delta_text("SEO", q.seo, p.seo),
            delta_text("GEO", q.geo, p.geo),
        ]
        .into_iter()
        .flatten()
        .collect();
        let swing = [(q.overall, p.overall), (q.seo, p.seo), (q.geo, p.geo)]
            .iter()
            .filter_map(|(a, b)| Some(i32::from((*b)?) - i32::from((*a)?)))
            .min_by_key(|d| -(d.abs()))
            .unwrap_or(0);
        let what = if count == 1 {
            format!("{}: 1 change", kind.label())
        } else {
            format!("{}: {count} changes", kind.label())
        };
        let effect = if moves.is_empty() {
            "No score change.".to_owned()
        } else {
            format!("{}.", moves.join(" · "))
        };
        pins.push((i, sev, count, what, effect, swing));
    }
    pins.sort_by(|a, b| {
        b.5.abs()
            .cmp(&a.5.abs())
            .then(a.1.cmp(&b.1))
            .then(b.0.cmp(&a.0))
    });
    pins.truncate(PINS);
    // Policy changes are always shown, neutral: the owner's choice, not an event on the site.
    for (i, p) in points.iter().enumerate() {
        if p.policy.is_empty() {
            continue;
        }
        let mut who: Vec<&str> = p.policy.iter().map(|c| c.who.as_str()).collect();
        who.dedup();
        let what = match p.policy.len() {
            1 => format!("Policy changed by {}", who.join(", ")),
            n => format!("Policy changed {n} times by {}", who.join(", ")),
        };
        let first = &p.policy[0];
        let last = &p.policy[p.policy.len() - 1];
        let moves: Vec<String> = [
            delta_text("Health", first.health.0, last.health.1),
            delta_text("GEO", first.geo.0, last.geo.1),
        ]
        .into_iter()
        .flatten()
        .collect();
        let effect = if moves.is_empty() {
            "No score change.".to_owned()
        } else {
            format!("{}.", moves.join(" · "))
        };
        pins.push((i, Severity::Notice, 0, what, effect, 0));
    }
    pins.sort_by_key(|p| p.0);
    t.pins = pins
        .into_iter()
        .enumerate()
        .map(|(k, (i, _, _, what, effect, swing))| {
            let x = x_of(i, n);
            Pin {
                n: k + 1,
                x,
                left: x / W * 100.0,
                tone: match swing {
                    d if d < 0 => "drop",
                    d if d > 0 => "rise",
                    _ => "flat",
                },
                when: format!("{} · crawl #{}", day(points[i].at), points[i].number),
                what,
                effect,
            }
        })
        .collect();

    t.sentence = sentence(points, &t.pins);
    t.label = format!("Scores over the last {n} crawls. {}", t.sentence);
    t
}

/// "The overall score improved from 71 to 75 across the last 14 crawls (5 weeks). GEO fell 18
/// points on Oct 3 after …"
fn sentence(points: &[TrendPoint], pins: &[Pin]) -> String {
    let n = points.len();
    let (name, get): (&str, fn(&TrendPoint) -> Option<i16>) =
        if points.iter().filter(|p| p.overall.is_some()).count() >= 2 {
            ("overall", |p| p.overall)
        } else {
            ("SEO", |p| p.seo)
        };
    let series: Vec<(usize, i16)> = points
        .iter()
        .enumerate()
        .filter_map(|(i, p)| Some((i, get(p)?)))
        .collect();
    let span = points[n - 1].at - points[0].at;
    let period = match span.whole_days() {
        d if d >= 14 => format!(" ({} weeks)", (d + 3) / 7),
        d if d >= 2 => format!(" ({d} days)"),
        _ => String::new(),
    };
    let mut out = match (series.first(), series.last()) {
        (Some(&(_, a)), Some(&(_, b))) if series.len() >= 2 => {
            let verb = match b.cmp(&a) {
                std::cmp::Ordering::Greater => "improved",
                std::cmp::Ordering::Less => "fell",
                std::cmp::Ordering::Equal => "held steady",
            };
            if a == b {
                format!(
                    "The {name} score {verb} at {b} across the last {} crawls{period}.",
                    series.len()
                )
            } else {
                format!(
                    "The {name} score {verb} from {a} to {b} across the last {} crawls{period}.",
                    series.len()
                )
            }
        }
        _ => format!("The scores of the last {n} crawls."),
    };
    // The biggest single drop of any score, when it is worth naming.
    let mut worst: Option<(i32, &str, usize)> = None;
    for (label, get) in [
        (
            "Overall",
            (|p: &TrendPoint| p.overall) as fn(&TrendPoint) -> Option<i16>,
        ),
        ("SEO", |p: &TrendPoint| p.seo),
        ("GEO", |p: &TrendPoint| p.geo),
    ] {
        for i in 1..n {
            if let (Some(a), Some(b)) = (get(&points[i - 1]), get(&points[i])) {
                let d = i32::from(b) - i32::from(a);
                if d <= -3 && worst.is_none_or(|w| d < w.0) {
                    worst = Some((d, label, i));
                }
            }
        }
    }
    if let Some((d, label, i)) = worst {
        let cause = pins
            .iter()
            .find(|p| (p.x - x_of(i, n)).abs() < 0.01)
            .map(|p| format!(" after “{}”", p.what))
            .unwrap_or_default();
        out.push_str(&format!(
            " {label} fell {} points on {}{cause}.",
            -d,
            fmt::date(points[i].at)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    fn pt(i: i64, seo: i16, geo: Option<i16>, changes: ChangeCounts) -> TrendPoint {
        let at = time::macros::datetime!(2026-09-01 06:00 UTC) + Duration::days(3 * i);
        TrendPoint {
            number: i + 1,
            at,
            seo: Some(seo),
            geo,
            overall: geo.map(|g| (0.6 * f64::from(seo) + 0.4 * f64::from(g)).round() as i16),
            changes,
            policy: Vec::new(),
        }
    }

    #[test]
    fn a_policy_change_is_always_a_neutral_pin() {
        let now = time::macros::datetime!(2026-10-14 10:00 UTC);
        let mut points: Vec<TrendPoint> = (0..3)
            .map(|i| pt(i, 75, Some(60), ChangeCounts::default()))
            .collect();
        points[2].seo = Some(79);
        points[2].policy = vec![PolicyPin {
            who: "ann@example.com".into(),
            health: (Some(75), Some(79)),
            geo: (Some(60), Some(60)),
        }];
        let t = trend(&points, None, now);
        assert_eq!(t.pins.len(), 1);
        let pin = &t.pins[0];
        assert_eq!(pin.what, "Policy changed by ann@example.com");
        assert_eq!(pin.effect, "Health 75 → 79.");
        assert_eq!(pin.tone, "flat");
        assert!(pin.when.ends_with("crawl #3"), "{}", pin.when);
    }

    #[test]
    fn one_crawl_waits_for_the_next() {
        let now = time::macros::datetime!(2026-10-14 10:00 UTC);
        let t = trend(&[pt(0, 80, Some(70), ChangeCounts::default())], None, now);
        assert!(t.waiting.unwrap().contains("No crawl is scheduled"));
        let next = time::macros::datetime!(2026-10-15 09:00 UTC);
        let t = trend(&[pt(0, 80, None, ChangeCounts::default())], Some(next), now);
        assert_eq!(
            t.waiting.as_deref(),
            Some("Next crawl: Oct 15, 09:00 UTC. Or run one now.")
        );
        assert!(t.bars.is_empty());
    }

    #[test]
    fn three_bars_a_crawl_skip_a_missing_geo_score_and_pins_name_the_drop() {
        let blocked = ChangeCounts {
            rows: vec![
                (ChangeKind::AiBotBlocked, Severity::Critical, 1),
                (ChangeKind::NewUrl, Severity::Notice, 4),
            ],
        };
        let points = vec![
            pt(0, 71, None, ChangeCounts::default()),
            pt(1, 72, Some(70), ChangeCounts::default()),
            pt(2, 74, Some(70), ChangeCounts::default()),
            pt(3, 75, Some(52), blocked),
            pt(4, 78, Some(64), ChangeCounts::default()),
        ];
        let now = points[4].at;
        let t = trend(&points, None, now);
        assert!(t.waiting.is_none());
        let count = |key: &str| t.bars.iter().filter(|b| b.key == key).count();
        assert_eq!(count("seo"), 5);
        assert_eq!(count("geo"), 4, "the first crawl has no GEO");
        assert_eq!(count("overall"), 4);
        let latest: Vec<&str> = t.latest.iter().map(|l| l.0).collect();
        assert_eq!(latest, ["overall", "seo", "geo"]);
        assert_eq!(t.bars.iter().filter(|b| b.last).count(), 3);
        assert_eq!(t.bars[0].title, "Crawl #1 · Sep 1 · SEO 71");
        assert_eq!(t.pins.len(), 1);
        let pin = &t.pins[0];
        assert_eq!(pin.tone, "drop");
        assert!(pin.when.ends_with("crawl #4"), "{}", pin.when);
        assert!(pin.what.starts_with("AI bot blocked"), "{}", pin.what);
        assert!(pin.effect.contains("GEO 70 → 52"), "{}", pin.effect);
        assert!(
            t.sentence
                .starts_with("The overall score improved from 71 to 72 across the last 4 crawls"),
            "{}",
            t.sentence
        );
        assert!(
            t.sentence.contains("GEO fell 18 points on Sep 10"),
            "{}",
            t.sentence
        );
        assert!(t.sentence.contains("AI bot blocked"), "{}", t.sentence);
        assert_eq!(t.axis.last().unwrap().1, "Today");
        // Every bar stands on 0 inside the box, and the groups don't overlap.
        for b in &t.bars {
            assert!((Y0..=Y1).contains(&b.y), "{}", b.title);
            assert!((b.y + b.h - Y1).abs() < 0.01, "{}", b.title);
            assert!(b.x >= X0 && b.x + b.w <= X1, "{}", b.title);
        }
        let seo: Vec<f64> = t
            .bars
            .iter()
            .filter(|b| b.key == "seo")
            .map(|b| b.x)
            .collect();
        assert!(
            seo.windows(2).all(|w| w[1] - w[0] >= slot(5) - 0.01),
            "{seo:?}"
        );
    }

    #[test]
    fn without_geo_scores_the_sentence_reads_the_seo_score() {
        let points = vec![
            pt(0, 80, None, ChangeCounts::default()),
            pt(1, 80, None, ChangeCounts::default()),
        ];
        let t = trend(&points, None, points[1].at);
        assert_eq!(t.bars.len(), 2);
        assert!(
            t.sentence.starts_with("The SEO score held steady at 80"),
            "{}",
            t.sentence
        );
        assert!(t.pins.is_empty());
    }
}
