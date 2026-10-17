//! The no-signup audit as it runs and once it is done (`/audit/{id}`): GEO first, the Overall
//! score large with SEO and GEO under it, every page as it is checked, then the fixes, of which
//! three are free and the rest wait for a sign-in.
//!
//! The running view is the live audit's (`live_audit`): the same steps, rows and issue ranking,
//! without the in-app chrome.

use codoseo_checks::{def, points_regained};
use codoseo_core::check::{CheckId, Severity};
use codoseo_core::output::{LivePhase, LiveProgress, LiveRow, StopReason};
use codoseo_core::plan::PlanLimits;
use codoseo_geo::score::{GEO_WEIGHT, GeoCheck, GeoScore, SEO_WEIGHT, Status};
use codoseo_store::crawls::{Crawl, CrawlStatus, StoredSummary};
use time::OffsetDateTime;

use crate::fmt;
use crate::live_audit::{
    IssueView, PhaseView, RowView, clock, expected_pages, issues, phases, row_view, severity_dot,
};
use crate::routes::audit_fix::{geo_check_label, reads_seo};
use crate::score_bars::ScoreBar;

/// Fixes shown before the sign-in.
pub const FREE_FIXES: usize = 3;
/// Pages listed while the audit runs: the newest.
pub const ROWS_SHOWN: i64 = 30;

/// The free audit's running or finished view.
pub struct FreeView {
    pub running: bool,
    /// What the audit is doing now, for the pill by the title.
    pub signal: String,
    pub count: String,
    pub bar: u32,
    pub elapsed: String,
    /// `87 pages in 1:48`, once done.
    pub done_line: String,
    pub lede: String,
    pub steps: Vec<PhaseView>,
    /// Overall, GEO and SEO as one bar chart.
    pub bars: Vec<ScoreBar>,
    /// The AI essentials, one line each: (dot, text).
    pub geo_facts: Vec<(&'static str, String)>,
    pub cards: Vec<Card>,
    pub score_note: &'static str,
    pub rows: Vec<RowView>,
    /// `24 of ~100 pages`.
    pub counter: String,
    pub issues: Vec<IssueView>,
    pub fixes: Vec<Fix>,
    /// Fixes kept for the full report.
    pub hidden_fixes: usize,
    pub all_clear: bool,
    pub stop_chip: Option<String>,
    /// The counted link to RankOrg, once the audit is done.
    pub rankorg: String,
}

pub struct Card {
    pub tag: Option<(&'static str, &'static str)>,
    pub label: &'static str,
    pub value: String,
    pub unit: &'static str,
    pub tags: Vec<(&'static str, String)>,
}

pub struct Fix {
    pub dot: &'static str,
    pub title: &'static str,
    /// `t-ai` or `t-seo`.
    pub track_class: &'static str,
    pub track: &'static str,
    pub evidence: String,
    /// Overall points the fix gives back: `+3.2`.
    pub points: String,
}

/// The sign-in card's words, which depend on how far the audit got.
pub struct Limit {
    pub title: String,
    /// `all 9 fixes`, or `every fix` before they are known.
    pub fixes: String,
    /// Pages crawled of the free cap, 0-100.
    pub bar: u32,
    pub line: String,
}

pub struct FreeInput<'a> {
    pub crawl: &'a Crawl,
    /// The newest rows, newest first, while it runs.
    pub rows: &'a [LiveRow],
    pub now: OffsetDateTime,
}

fn cap() -> u32 {
    PlanLimits::quick_audit().max_pages.unwrap_or(100)
}

/// What the sign-in card says for `crawl`.
pub fn limit(crawl: &Crawl) -> Limit {
    let live = crawl.live_progress();
    let sitemap = live
        .as_ref()
        .and_then(|l| l.files.sitemap_urls)
        .unwrap_or(0);
    let done = live.as_ref().map_or(0, |l| l.progress.pages_done);
    let cap = cap();
    let finished = crawl.status == CrawlStatus::Done;
    let title = match (finished, sitemap > cap) {
        (true, true) => format!(
            "Get the full report and all {} pages",
            fmt::thousands(sitemap)
        ),
        (true, false) => "Get the full report".to_owned(),
        (false, true) => format!(
            "This free audit stops at {cap} pages. Your sitemap lists {}.",
            fmt::thousands(sitemap)
        ),
        (false, false) => format!("This free audit stops at {cap} pages."),
    };
    let fixes = crawl
        .summary()
        .map(|s| fix_count(&s))
        .filter(|&n| n > 0 && finished)
        .map_or("every fix".to_owned(), |n| {
            format!("all {n} fix{}", if n == 1 { "" } else { "es" })
        });
    Limit {
        title,
        fixes,
        bar: (u64::from(done.min(cap)) * 100 / u64::from(cap.max(1))) as u32,
        line: format!(
            "{} of {cap} free pages used · signed in: up to 500",
            fmt::thousands(done.min(cap))
        ),
    }
}

impl FreeView {
    pub fn build(i: FreeInput<'_>) -> FreeView {
        let c = i.crawl;
        let live = c.live_progress();
        let running = c.status != CrawlStatus::Done;
        let summary = c.summary();
        let phase = match (&live, c.status) {
            (_, CrawlStatus::Done) => None,
            (None, _) => Some(LivePhase::Robots),
            (Some(l), _) => Some(l.phase),
        };
        let files = live.as_ref().map(|l| l.files).unwrap_or_default();
        let done = live.as_ref().map_or(0, |l| l.progress.pages_done);
        let pages = summary.as_ref().map_or(done, |s| s.report_summary.pages);
        let queued = live.as_ref().map_or(0, |l| l.progress.queued);
        let total = expected_pages(done, queued, files.sitemap_urls, cap());
        let elapsed_ms = match (c.started_at, c.finished_at) {
            (Some(s), Some(f)) => (f - s).whole_milliseconds().max(0) as u64,
            (Some(s), None) => (i.now - s).whole_milliseconds().max(0) as u64,
            _ => 0,
        };
        let tally = live.as_ref().map(|l| l.tally.clone()).unwrap_or_default();
        let geo = summary.as_ref().and_then(|s| s.geo.clone());

        let (seo, geo_score, overall) = if running {
            (
                live.as_ref().and_then(|l| l.seo_estimate).map(i16::from),
                None,
                None,
            )
        } else {
            (c.health_score, c.geo_score, c.overall_score)
        };
        let bars = score_bars(
            running,
            geo.as_ref(),
            [overall, geo_score, seo],
            summary.as_ref(),
        );

        let fixes_all = summary.as_ref().map(all_fixes).unwrap_or_default();
        FreeView {
            running,
            signal: signal(phase),
            count: format!(
                "{} / {}",
                fmt::thousands(done),
                fmt::thousands(total.max(done))
            ),
            bar: if total == 0 {
                0
            } else {
                (u64::from(done) * 100 / u64::from(total)).min(100) as u32
            },
            elapsed: clock(elapsed_ms),
            done_line: format!("{} in {}", fmt::pages(pages), clock(elapsed_ms)),
            lede: if running {
                "Each page is checked as it arrives: can search engines index it, can AI products reach it, and is the content in shape.".to_owned()
            } else {
                "How this site does in AI answers and in search, from the pages we checked."
                    .to_owned()
            },
            steps: phases(phase, files, done, total),
            geo_facts: geo_facts(geo.as_ref(), running),
            bars,
            cards: vec![pages_card(running, live.as_ref(), done, total)],
            score_note: if running {
                "Scores so far are estimates. GEO and Overall come when the audit is done."
            } else {
                "Overall is 60% SEO and 40% GEO. Points show how much a fix adds to Overall."
            },
            rows: i
                .rows
                .iter()
                .map(|r| row_view(r, &tally.checks, done))
                .collect(),
            counter: format!(
                "{} of ~{} pages",
                fmt::thousands(done),
                fmt::thousands(total)
            ),
            issues: if running {
                issues(&tally.checks, done)
            } else {
                Vec::new()
            },
            hidden_fixes: fixes_all.len().saturating_sub(FREE_FIXES),
            all_clear: !running && fixes_all.is_empty(),
            rankorg: format!("/go/rankorg?src=audit&audit={}", c.id),
            fixes: fixes_all.into_iter().take(FREE_FIXES).collect(),
            stop_chip: summary.as_ref().and_then(|s| match s.stop_reason {
                StopReason::PageLimit => {
                    Some(format!("Stopped at the free limit of {} pages", cap()))
                }
                StopReason::TimeLimit => Some("Stopped at the 2 minute limit".to_owned()),
                _ => None,
            }),
        }
    }
}

fn signal(phase: Option<LivePhase>) -> String {
    match phase {
        Some(LivePhase::Robots) => "Reading robots.txt",
        Some(LivePhase::Sitemap) => "Reading the sitemap",
        Some(LivePhase::LlmsTxt) => "Reading llms.txt",
        Some(LivePhase::Pages) => "Checking pages",
        Some(LivePhase::Bots | LivePhase::Scoring) => "Working out the scores",
        None => "",
    }
    .to_owned()
}

/// The AI essentials as plain lines: can AI search bots reach the pages, may they be quoted,
/// is the text there without JavaScript.
fn geo_facts(geo: Option<&GeoScore>, running: bool) -> Vec<(&'static str, String)> {
    let Some(geo) = geo else {
        return vec![(
            "is-na",
            if running {
                "AI checks come in when the pages are done".to_owned()
            } else {
                "No AI checks for this audit".to_owned()
            },
        )];
    };
    [
        GeoCheck::BotsBlocked,
        GeoCheck::AnswersRestricted,
        GeoCheck::ContentMissing,
    ]
    .into_iter()
    .filter_map(|check| {
        let line = geo.checks.iter().find(|c| c.check == check)?;
        match line.status {
            Status::Pass => Some(("is-ok", geo_check_label(check).to_owned())),
            // The dot says it fails; the line says what it should be.
            Status::Fail => Some((
                severity_dot(line.severity.unwrap_or(Severity::Warning)),
                geo_check_label(check).to_owned(),
            )),
            Status::Skipped => None,
        }
    })
    .collect()
}

/// Overall, GEO and SEO, each with a short line on what it rests on.
fn score_bars(
    running: bool,
    geo: Option<&GeoScore>,
    [overall, geo_score, seo]: [Option<i16>; 3],
    summary: Option<&StoredSummary>,
) -> Vec<ScoreBar> {
    let geo_detail = match geo {
        Some(g) => {
            let failing = g.checks.iter().filter(|c| c.status == Status::Fail).count();
            let fix = if failing > 0 {
                format!("{failing} to fix")
            } else {
                "nothing to fix".to_owned()
            };
            format!(
                "{} of {} checks pass · {fix}",
                g.checks_passed, g.checks_applicable
            )
        }
        None if running => "at the end".to_owned(),
        None => String::new(),
    };
    let seo_detail = match summary {
        Some(s) => {
            let count = |sev: Severity| {
                s.counts
                    .iter()
                    .filter_map(|(slug, _)| CheckId::from_slug(slug))
                    .filter(|&id| def(id).severity == sev)
                    .count()
            };
            let (crit, warn) = (count(Severity::Critical), count(Severity::Warning));
            let mut parts = Vec::new();
            if crit > 0 {
                parts.push(format!("{crit} critical"));
            }
            if warn > 0 {
                parts.push(format!(
                    "{warn} warning{}",
                    if warn == 1 { "" } else { "s" }
                ));
            }
            if parts.is_empty() {
                "no critical issues".to_owned()
            } else {
                parts.join(" · ")
            }
        }
        None => "estimate".to_owned(),
    };
    vec![
        ScoreBar {
            detail: if running {
                "at the end".to_owned()
            } else {
                "60% SEO + 40% GEO".to_owned()
            },
            ..ScoreBar::new("overall", overall)
        },
        ScoreBar {
            detail: geo_detail,
            ..ScoreBar::new("geo", geo_score)
        },
        ScoreBar {
            detail: seo_detail,
            live: running,
            ..ScoreBar::new("seo", seo)
        },
    ]
}

fn pages_card(running: bool, live: Option<&LiveProgress>, done: u32, total: u32) -> Card {
    let errors = live.map_or(0, |l| l.errors);
    let mut page_tags = vec![if errors > 0 {
        (
            "ck-crit",
            format!("{errors} error{}", if errors == 1 { "" } else { "s" }),
        )
    } else {
        ("ck-ok", "no errors".to_owned())
    }];
    if let Some(blocked) = live.map(|l| l.tally.ai_blocked).filter(|&n| n > 0) {
        page_tags.push(("ck-warn", format!("{blocked} AI can't reach")));
    }
    Card {
        tag: None,
        label: "Pages checked",
        value: fmt::thousands(done),
        unit: "",
        tags: if running {
            let mut t = vec![("ck-run", format!("of ~{}", fmt::thousands(total)))];
            t.extend(page_tags);
            t
        } else {
            page_tags
        },
    }
}

/// Every fix the audit found, AI first, then search, each by the Overall points it gives back.
fn all_fixes(s: &StoredSummary) -> Vec<Fix> {
    let mut out = Vec::new();
    if let Some(geo) = &s.geo {
        let mut failing: Vec<_> = geo
            .checks
            .iter()
            .filter(|c| c.status == Status::Fail)
            .collect();
        // The checks only AI products ask about first; those that read SEO's (titles,
        // descriptions) come after them.
        failing.sort_by(|a, b| {
            reads_seo(a.check)
                .cmp(&reads_seo(b.check))
                .then(b.points_regained.total_cmp(&a.points_regained))
        });
        out.extend(failing.into_iter().map(|c| Fix {
            dot: severity_dot(c.severity.unwrap_or(Severity::Warning)),
            title: geo_check_label(c.check),
            track_class: "t-ai",
            track: "AI",
            evidence: "Costs you in AI answers.".to_owned(),
            points: format!("+{:.1}", c.points_regained * GEO_WEIGHT),
        }));
    }
    let counts: Vec<(CheckId, u32)> = s
        .counts
        .iter()
        .filter_map(|(slug, n)| Some((CheckId::from_slug(slug)?, *n)))
        .collect();
    let pages = s.report_summary.pages.max(1);
    let mut gains = points_regained(&counts, pages);
    gains.sort_by(|a, b| b.1.total_cmp(&a.1));
    out.extend(gains.into_iter().filter_map(|(id, pts)| {
        let n = counts.iter().find(|c| c.0 == id)?.1;
        Some(Fix {
            dot: severity_dot(def(id).severity),
            title: def(id).title,
            track_class: "t-seo",
            track: "SEO",
            evidence: format!("On {} of {}.", fmt::thousands(n), fmt::pages(pages)),
            points: format!("+{:.1}", pts * SEO_WEIGHT),
        })
    }));
    out
}

fn fix_count(s: &StoredSummary) -> usize {
    all_fixes(s).len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_core::check::IssueBits;
    use codoseo_core::output::{LiveFiles, Progress};
    use codoseo_core::page::Indexability;
    use codoseo_store::crawl_queue::CrawlTrigger;
    use uuid::Uuid;

    fn crawl(status: CrawlStatus, live: &LiveProgress) -> Crawl {
        let now = OffsetDateTime::now_utc();
        Crawl {
            id: Uuid::nil(),
            site_id: Uuid::nil(),
            number: 1,
            status,
            trigger: CrawlTrigger::Quick,
            queued_at: now - time::Duration::seconds(60),
            started_at: Some(now - time::Duration::seconds(48)),
            finished_at: None,
            health_score: None,
            checks_passed: None,
            checks_total: None,
            geo_score: None,
            geo_checks_passed: None,
            geo_checks_applicable: None,
            overall_score: None,
            progress: Some(serde_json::to_value(live).unwrap()),
            summary: None,
            failure_reason: None,
        }
    }

    fn live(done: u32, sitemap: u32) -> LiveProgress {
        let mut l = LiveProgress::new(
            Progress {
                pages_done: done,
                queued: 40,
                failures: 0,
                depth: 1,
                elapsed_ms: 48_000,
            },
            &[],
        );
        l.files = LiveFiles {
            robots: Some(200),
            sitemap_urls: Some(sitemap),
            llms_txt: None,
        };
        l.seo_estimate = Some(88);
        l
    }

    #[test]
    fn a_running_audit_shows_its_progress_and_estimates_only_seo() {
        let c = crawl(CrawlStatus::Running, &live(24, 452));
        let row = LiveRow {
            n: 24,
            url: "https://shop.example.com/a".to_owned(),
            status: 200,
            error: None,
            response_ms: 90,
            indexability: Indexability::Indexable,
            issues: IssueBits::default(),
            ai_blocked: 0,
            ai_of: 10,
            probes: Vec::new(),
        };
        let v = FreeView::build(FreeInput {
            crawl: &c,
            rows: &[row],
            now: OffsetDateTime::now_utc(),
        });
        assert!(v.running);
        assert_eq!(v.count, "24 / 100", "never past the free cap");
        let bars: Vec<_> = v.bars.iter().map(|b| (b.key, b.shown())).collect();
        assert_eq!(
            bars,
            [
                ("overall", "–".into()),
                ("geo", "–".into()),
                ("seo", "88".into())
            ],
            "GEO before SEO"
        );
        assert_eq!(v.cards[0].label, "Pages checked");
        assert_eq!(v.rows.len(), 1);
        let l = limit(&c);
        assert_eq!(
            l.title,
            "This free audit stops at 100 pages. Your sitemap lists 452."
        );
        assert_eq!(l.fixes, "every fix");
        assert_eq!(l.bar, 24);
    }

    #[test]
    fn no_copy_uses_an_em_dash() {
        let c = crawl(CrawlStatus::Running, &live(5, 20));
        let v = FreeView::build(FreeInput {
            crawl: &c,
            rows: &[],
            now: OffsetDateTime::now_utc(),
        });
        for text in [
            &v.lede,
            &v.score_note.to_owned(),
            &limit(&c).title,
            &limit(&c).line,
        ] {
            assert!(!text.contains('—'), "{text}");
        }
    }
}
