//! The site audit's "Score trend": the Overall, SEO and GEO scores of the last finished crawls
//! as one server-drawn SVG, a sentence saying what happened, and numbered pins on the crawls
//! whose change records hold something serious, with what changed and what it did to the
//! scores. Crawls without a GEO (or Overall) score are left out of that line; with fewer than
//! two crawls there is no chart, only when the next crawl is due.

use codoseo_core::change::ChangeKind;
use codoseo_core::check::Severity;
use codoseo_store::reports::ChangeCounts;
use time::OffsetDateTime;

use crate::fmt;

/// The chart's box, in % of the drawn chart, so the SVG (stretched to fit) and the HTML labels
/// over it share one scale: plot from `X0` to `X1`, and from `Y0` (100) down to `Y1`.
pub const W: f64 = 100.0;
pub const H: f64 = 100.0;
const X0: f64 = 1.0;
const X1: f64 = 80.0;
const Y0: f64 = 6.0;
const Y1: f64 = 92.0;
/// End labels at least this far apart, in % of the height.
const LABEL_GAP: f64 = 7.5;
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
}

pub struct Line {
    /// `overall`, `seo` or `geo`.
    pub key: &'static str,
    pub points: String,
    /// The last point and its label: `(x, y, label y, "Overall 75")`.
    pub end: Option<(f64, f64, f64, String)>,
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
    pub lines: Vec<Line>,
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

fn x_of(i: usize, n: usize) -> f64 {
    if n <= 1 {
        X1
    } else {
        X0 + i as f64 * (X1 - X0) / (n - 1) as f64
    }
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
        lines: Vec::new(),
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
    let values = points
        .iter()
        .flat_map(|p| [p.seo, p.geo, p.overall])
        .flatten();
    let min = values.min().map_or(0, i64::from);
    // 100 at the top; the bottom is the lowest score rounded down to 20, at most 60, so a
    // small move still reads as one.
    let lo = (((min - 5).max(0) / 20) * 20).min(60);
    let y = |v: i16| Y1 - (f64::from(v) - lo as f64) / (100 - lo) as f64 * (Y1 - Y0);
    t.grid = (0..=4)
        .map(|k| {
            let v = lo + (100 - lo) * k / 4;
            (y(v as i16), v)
        })
        .collect();

    let mut ends: Vec<(usize, f64, String)> = Vec::new();
    for (key, name, get) in [
        (
            "overall",
            "Overall",
            (|p: &TrendPoint| p.overall) as fn(&TrendPoint) -> Option<i16>,
        ),
        ("seo", "SEO", |p: &TrendPoint| p.seo),
        ("geo", "GEO", |p: &TrendPoint| p.geo),
    ] {
        let pts: Vec<(f64, f64, i16)> = points
            .iter()
            .enumerate()
            .filter_map(|(i, p)| get(p).map(|v| (x_of(i, n), y(v), v)))
            .collect();
        if pts.is_empty() {
            continue;
        }
        let last = *pts.last().expect("not empty");
        ends.push((t.lines.len(), last.1, format!("{name} {}", last.2)));
        t.lines.push(Line {
            key,
            points: pts
                .iter()
                .map(|(x, y, _)| format!("{x:.2},{y:.2}"))
                .collect::<Vec<_>>()
                .join(" "),
            end: Some((last.0, last.1, last.1, String::new())),
        });
    }
    // End labels apart, in the order of their values.
    ends.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut prev: Option<f64> = None;
    for (i, y0, label) in ends {
        let ly = match prev {
            Some(p) if y0 - p < LABEL_GAP => p + LABEL_GAP,
            _ => y0,
        };
        prev = Some(ly);
        if let Some(end) = t.lines[i].end.as_mut() {
            end.2 = ly;
            end.3 = label;
        }
    }

    let day = |at: OffsetDateTime| {
        if at.date() == now.date() {
            "Today".to_owned()
        } else {
            fmt::date(at)
        }
    };
    t.axis = vec![
        (x_of(0, n), fmt::date(points[0].at), "start"),
        (x_of(n / 2, n), fmt::date(points[n / 2].at), "middle"),
        (x_of(n - 1, n), day(points[n - 1].at), "end"),
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
        }
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
        assert!(t.lines.is_empty());
    }

    #[test]
    fn three_lines_skip_crawls_without_a_geo_score_and_pins_name_the_drop() {
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
        let keys: Vec<&str> = t.lines.iter().map(|l| l.key).collect();
        assert_eq!(keys, ["overall", "seo", "geo"]);
        assert_eq!(t.lines[1].points.split(' ').count(), 5);
        assert_eq!(
            t.lines[2].points.split(' ').count(),
            4,
            "the first crawl has no GEO"
        );
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
        // Every point sits inside the box, and the end labels don't overlap.
        for l in &t.lines {
            for p in l.points.split(' ') {
                let y: f64 = p.split(',').nth(1).unwrap().parse().unwrap();
                assert!((Y0..=Y1).contains(&y), "{p}");
            }
        }
        let mut ys: Vec<f64> = t.lines.iter().map(|l| l.end.as_ref().unwrap().2).collect();
        ys.sort_by(f64::total_cmp);
        assert!(
            ys.windows(2).all(|w| w[1] - w[0] >= LABEL_GAP - 0.01),
            "{ys:?}"
        );
    }

    #[test]
    fn without_geo_scores_the_sentence_reads_the_seo_score() {
        let points = vec![
            pt(0, 80, None, ChangeCounts::default()),
            pt(1, 80, None, ChangeCounts::default()),
        ];
        let t = trend(&points, None, points[1].at);
        assert_eq!(t.lines.len(), 1);
        assert!(
            t.sentence.starts_with("The SEO score held steady at 80"),
            "{}",
            t.sentence
        );
        assert!(t.pins.is_empty());
    }
}
