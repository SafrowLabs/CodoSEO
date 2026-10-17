//! The live audit: a crawl as it runs, page by page, on the site audit (the first crawl) and on
//! its own screen (`/s/{site}/audit/watch`, for a crawl over a finished audit).
//!
//! Everything comes from the crawl row's progress (`LiveProgress`: the stage, the site files,
//! running totals) and the newest of its live rows, so a poll reads one row and a page of rows
//! whatever the size of the site. When the crawl is over the view says so with the final scores,
//! then turns to the results.

use askama::Template;
use codoseo_checks::{Category, def, page_points, points_regained};
use codoseo_core::check::{CheckId, Severity};
use codoseo_core::output::{LiveFiles, LivePhase, LiveProgress, LiveRow, LiveTally};
use codoseo_core::page::{FetchFailure, Indexability};
use codoseo_geo::eligibility::engines;
use codoseo_store::crawls::{Crawl, CrawlStatus, LiveFilter};
use time::OffsetDateTime;

use crate::fmt;
use crate::routes::audit_ai::engine_short;

/// Rows listed at most: the newest.
pub const ROWS_SHOWN: i64 = 60;
/// Issues listed at most in "Issues so far".
const ISSUES_SHOWN: usize = 8;

#[derive(Template)]
#[template(path = "audit/live_audit.html")]
pub struct LiveAudit {
    pub base: String,
    pub domain: String,
    pub number: i64,
    pub running: bool,
    /// The poll that refreshes the view, with the filter kept.
    pub poll: String,
    /// What the crawl is doing now: "Checking pages".
    pub signal: String,
    /// `14 / 452`.
    pub count: String,
    /// The progress bar's fill, 0-100.
    pub bar: u32,
    /// `1:08`.
    pub elapsed: String,
    /// Under the title: when it started and how far it may go.
    pub meta: String,
    /// The finished crawl's line: `452 pages in 2:04 · 3 new issues, 2 fixed since #44`.
    pub done_line: String,
    pub phases: Vec<PhaseView>,
    pub kpis: Vec<Kpi>,
    pub score_note: &'static str,
    pub filters: Vec<FilterChip>,
    pub files: Vec<FileRow>,
    pub rows: Vec<RowView>,
    pub issues: Vec<IssueView>,
    pub end_checks: Vec<EndCheck>,
    pub foot: String,
}

pub struct PhaseView {
    /// `is-done`, `is-run` or `is-wait`.
    pub class: &'static str,
    pub icon: String,
    pub title: &'static str,
    pub detail: String,
}

pub struct Kpi {
    pub key: &'static str,
    pub label: &'static str,
    pub value: String,
    pub unit: &'static str,
    /// `up` / `down` / empty, for the change.
    pub change_class: &'static str,
    pub change: String,
    pub detail: String,
    /// Still moving: a live dot by the label.
    pub live: bool,
    pub tag: Option<(&'static str, &'static str)>,
}

pub struct FilterChip {
    pub label: &'static str,
    pub count: String,
    pub href: String,
    pub on: bool,
}

pub struct FileRow {
    pub name: &'static str,
    pub status: String,
    pub pill: &'static str,
    pub tag_class: &'static str,
    pub tag: String,
}

pub struct RowView {
    pub n: u32,
    pub status: String,
    pub pill: &'static str,
    pub url: String,
    pub path: String,
    pub ms: String,
    pub tags: Vec<Tag>,
    pub impact: String,
    pub impact_class: &'static str,
    pub groups: Vec<Group>,
}

pub struct Tag {
    pub class: &'static str,
    pub icon: &'static str,
    pub name: &'static str,
    pub detail: String,
}

/// One box of an opened row: a group of checks with how each went.
pub struct Group {
    pub name: &'static str,
    pub dot: &'static str,
    pub items: Vec<(&'static str, String)>,
    /// The page as each probed AI bot got it: (class, label).
    pub bots: Vec<(&'static str, String)>,
}

pub struct IssueView {
    pub dot: &'static str,
    pub name: &'static str,
    pub count: String,
    pub points: String,
}

pub struct EndCheck {
    pub dot: &'static str,
    pub name: &'static str,
    pub result: String,
}

/// Which rows the list shows, from `?f=`.
pub fn filter_from(q: Option<&str>) -> LiveFilter {
    match q {
        Some("issues") => LiveFilter::Issues,
        Some("ai") => LiveFilter::Ai,
        Some("errors") => LiveFilter::Errors,
        _ => LiveFilter::All,
    }
}

fn filter_slug(f: LiveFilter) -> &'static str {
    match f {
        LiveFilter::All => "all",
        LiveFilter::Issues => "issues",
        LiveFilter::Ai => "ai",
        LiveFilter::Errors => "errors",
    }
}

/// What the view is built from.
pub struct LiveInput<'a> {
    pub base: String,
    pub domain: String,
    /// The crawl shown: running, queued, or just finished.
    pub crawl: &'a Crawl,
    /// The newest rows matching `filter`, newest first.
    pub rows: &'a [LiveRow],
    pub filter: LiveFilter,
    /// The finished crawl before this one, for the changes and the scores to compare with.
    pub previous: Option<&'a Crawl>,
    /// The most pages the plan lets the crawl fetch.
    pub max_pages: u32,
    pub now: OffsetDateTime,
}

impl LiveAudit {
    pub fn build(i: LiveInput<'_>) -> LiveAudit {
        let c = i.crawl;
        let live = c.live_progress();
        let running = c.status != CrawlStatus::Done;
        let phase = match (&live, c.status) {
            (_, CrawlStatus::Done) => None,
            (None, _) => Some(LivePhase::Robots),
            (Some(l), _) => Some(l.phase),
        };
        let tally = live.as_ref().map(|l| l.tally.clone()).unwrap_or_default();
        let files = live.as_ref().map(|l| l.files).unwrap_or_default();
        let done_pages = live.as_ref().map_or(0, |l| l.progress.pages_done);
        let queued = live.as_ref().map_or(0, |l| l.progress.queued);
        let total = expected_pages(done_pages, queued, files.sitemap_urls, i.max_pages);
        let elapsed_ms = elapsed(c, live.as_ref(), i.now);

        let signal = match (c.status, phase) {
            (CrawlStatus::Queued, _) => "Waiting for a free crawler".to_owned(),
            (_, Some(LivePhase::Robots)) => "Reading robots.txt".to_owned(),
            (_, Some(LivePhase::Sitemap)) => "Reading the sitemap".to_owned(),
            (_, Some(LivePhase::LlmsTxt)) => "Reading llms.txt".to_owned(),
            (_, Some(LivePhase::Pages)) => "Checking pages".to_owned(),
            (_, Some(LivePhase::Bots)) => "Fetching key pages as AI bots".to_owned(),
            (_, Some(LivePhase::Scoring)) => "Working out the final scores".to_owned(),
            (_, None) => String::new(),
        };
        let bar = if total == 0 {
            0
        } else {
            (u64::from(done_pages) * 100 / u64::from(total)).min(100) as u32
        };
        let previous = i.previous;
        let meta = match c.started_at {
            Some(at) => format!(
                "Crawl #{} · started {} · up to {} pages",
                c.number,
                fmt::ago_from(at, i.now),
                fmt::thousands(i.max_pages)
            ),
            None => format!(
                "Crawl #{} · queued {}",
                c.number,
                fmt::ago_from(c.queued_at, i.now)
            ),
        };

        LiveAudit {
            base: i.base.clone(),
            domain: i.domain,
            number: c.number,
            running,
            poll: format!("{}/audit/watch?f={}", i.base, filter_slug(i.filter)),
            signal,
            count: format!(
                "{} / {}",
                fmt::thousands(done_pages),
                fmt::thousands(total.max(done_pages))
            ),
            bar,
            elapsed: clock(elapsed_ms),
            meta,
            done_line: done_line(c, previous, done_pages, elapsed_ms),
            phases: phases(phase, files, done_pages, total),
            kpis: kpis(
                c,
                previous,
                &tally,
                live.as_ref().and_then(|l| l.seo_estimate),
                done_pages,
                total,
                running,
            ),
            score_note: if running {
                "Scores are estimates from the pages checked so far. Checks that need every page, and the GEO score, count at the end."
            } else {
                "Final scores for this crawl."
            },
            filters: filters(&i.base, i.filter, &tally, done_pages, live.as_ref()),
            files: if i.filter == LiveFilter::All {
                file_rows(files, phase)
            } else {
                Vec::new()
            },
            rows: i
                .rows
                .iter()
                .map(|r| row_view(r, &tally.checks, done_pages))
                .collect(),
            issues: issues(&tally.checks, done_pages),
            end_checks: end_checks(phase),
            foot: foot(i.rows.len(), done_pages, i.filter),
        }
    }
}

/// How many pages the crawl will likely fetch: what it has and has queued, or the sitemap's
/// count when that is more, never past the plan's cap.
fn expected_pages(done: u32, queued: u32, sitemap: Option<u32>, max: u32) -> u32 {
    (done + queued).max(sitemap.unwrap_or(0)).min(max).max(done)
}

fn elapsed(c: &Crawl, live: Option<&LiveProgress>, now: OffsetDateTime) -> u64 {
    let reported = live.map_or(0, |l| l.progress.elapsed_ms);
    match (c.started_at, c.finished_at) {
        (Some(s), Some(f)) => (f - s).whole_milliseconds().max(0) as u64,
        // Counted from the claim, so the clock ticks between heartbeats.
        (Some(s), None) => ((now - s).whole_milliseconds().max(0) as u64).max(reported),
        _ => 0,
    }
}

/// `2:04`, or `1:02:04` past an hour.
fn clock(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn done_line(c: &Crawl, previous: Option<&Crawl>, pages: u32, ms: u64) -> String {
    let mut line = format!("{} in {}", fmt::pages(pages), clock(ms));
    let (Some(now), Some(prev)) = (c.summary(), previous.and_then(|p| Some((p, p.summary()?))))
    else {
        return line;
    };
    let (prev, before) = prev;
    let ids = |counts: &[(String, u32)]| -> Vec<String> {
        counts.iter().map(|(slug, _)| slug.clone()).collect()
    };
    let (now_ids, before_ids) = (ids(&now.counts), ids(&before.counts));
    let new = now_ids.iter().filter(|s| !before_ids.contains(s)).count();
    let fixed = before_ids.iter().filter(|s| !now_ids.contains(s)).count();
    line.push_str(&format!(
        " · {} new {}, {} fixed since #{}",
        new,
        if new == 1 { "issue" } else { "issues" },
        fixed,
        prev.number
    ));
    line
}

fn phases(phase: Option<LivePhase>, files: LiveFiles, done: u32, total: u32) -> Vec<PhaseView> {
    let at = |p: LivePhase| match phase {
        None => "is-done",
        Some(now) if now > p => "is-done",
        Some(now) if now == p => "is-run",
        _ => "is-wait",
    };
    let status = |s: Option<u16>| match s {
        Some(0) => "no response".to_owned(),
        Some(s @ 200..=299) => format!("{s} · read"),
        Some(s) => format!("{s} · not found"),
        None => String::new(),
    };
    // The llms.txt stage is skipped by crawls that don't read it.
    let llms = match (files.llms_txt, at(LivePhase::LlmsTxt)) {
        (Some(_), _) => status(files.llms_txt),
        (None, "is-done") => "not read".to_owned(),
        _ => "waiting".to_owned(),
    };
    let final_detail = match phase {
        Some(LivePhase::Bots) => "fetching as AI bots".to_owned(),
        Some(LivePhase::Scoring) => "SEO, GEO and Overall".to_owned(),
        None => "done".to_owned(),
        _ => "after the pages".to_owned(),
    };
    let list = [
        (LivePhase::Robots, "robots.txt", status(files.robots)),
        (
            LivePhase::Sitemap,
            "Sitemap",
            files
                .sitemap_urls
                .map(|n| format!("{} URLs", fmt::thousands(n)))
                .unwrap_or_default(),
        ),
        (LivePhase::LlmsTxt, "llms.txt", llms),
        (
            LivePhase::Pages,
            "Pages",
            if done == 0 {
                String::new()
            } else {
                format!("{} of ~{}", fmt::thousands(done), fmt::thousands(total))
            },
        ),
        (LivePhase::Bots, "Final score", final_detail),
    ];
    list.into_iter()
        .enumerate()
        .map(|(n, (p, title, detail))| {
            // The last step covers the bots and the scoring.
            let class = if p == LivePhase::Bots {
                match phase {
                    None => "is-done",
                    Some(LivePhase::Bots | LivePhase::Scoring) => "is-run",
                    _ => "is-wait",
                }
            } else {
                at(p)
            };
            PhaseView {
                class,
                icon: if class == "is-done" {
                    "✓".to_owned()
                } else {
                    (n + 1).to_string()
                },
                title,
                detail: if detail.is_empty() && class == "is-wait" {
                    "waiting".to_owned()
                } else {
                    detail
                },
            }
        })
        .collect()
}

fn score_change(
    now: Option<i16>,
    before: Option<i16>,
    prev: Option<&Crawl>,
) -> (&'static str, String) {
    match (now, before, prev) {
        (Some(n), Some(b), Some(p)) if n != b => (
            if n > b { "up" } else { "down" },
            format!(
                "{}{} vs #{}",
                if n > b { "+" } else { "−" },
                (n - b).abs(),
                p.number
            ),
        ),
        (Some(_), Some(_), Some(p)) => ("", format!("same as #{}", p.number)),
        _ => ("", String::new()),
    }
}

fn kpis(
    c: &Crawl,
    prev: Option<&Crawl>,
    tally: &LiveTally,
    estimate: Option<u8>,
    done: u32,
    total: u32,
    running: bool,
) -> Vec<Kpi> {
    let score = |v: Option<i16>| v.map_or("–".to_owned(), |v| v.to_string());
    let (seo_now, geo_now, overall_now) = if running {
        (estimate.map(i16::from), None, None)
    } else {
        (c.health_score, c.geo_score, c.overall_score)
    };
    let last = |f: fn(&Crawl) -> Option<i16>| prev.and_then(f);
    let at_end = |v: Option<i16>| match v {
        Some(v) => format!("final at the end · last {v}"),
        None => "final at the end".to_owned(),
    };
    let (oc, ochange) = score_change(overall_now, last(|p| p.overall_score), prev);
    // The estimate leaves out the checks that need every page, so it is not compared with a final
    // score.
    let (sc, schange) = if running {
        ("", String::new())
    } else {
        score_change(seo_now, last(|p| p.health_score), prev)
    };
    let (gc, gchange) = score_change(geo_now, last(|p| p.geo_score), prev);
    let reach = tally.ai_judged.saturating_sub(tally.ai_blocked);
    let avg = (tally.answered > 0).then(|| tally.response_ms_total / u64::from(tally.answered));
    vec![
        Kpi {
            key: "overall",
            label: "Overall",
            value: score(overall_now),
            unit: "/100",
            change_class: oc,
            change: ochange,
            detail: if running {
                at_end(last(|p| p.overall_score))
            } else {
                String::new()
            },
            live: false,
            tag: None,
        },
        Kpi {
            key: "seo",
            label: "SEO score",
            value: score(seo_now),
            unit: "/100",
            change_class: sc,
            change: schange,
            detail: if running && seo_now.is_some() {
                "estimate".to_owned()
            } else {
                String::new()
            },
            live: running,
            tag: Some(("t-seo", "SEO")),
        },
        Kpi {
            key: "geo",
            label: "GEO score",
            value: score(geo_now),
            unit: "/100",
            change_class: gc,
            change: gchange,
            detail: if running {
                at_end(last(|p| p.geo_score))
            } else {
                String::new()
            },
            live: false,
            tag: Some(("t-ai", "GEO")),
        },
        Kpi {
            key: "reach",
            label: "AI can reach",
            value: if tally.ai_judged == 0 {
                "–".to_owned()
            } else {
                fmt::thousands(reach)
            },
            unit: "pages",
            change_class: if tally.ai_blocked > 0 { "down" } else { "" },
            change: if tally.ai_blocked > 0 {
                format!("{} kept out", fmt::thousands(tally.ai_blocked))
            } else {
                String::new()
            },
            detail: if tally.ai_judged == 0 {
                "robots.txt not read yet".to_owned()
            } else {
                format!("of {} open to all AI", fmt::thousands(tally.ai_judged))
            },
            live: running,
            tag: None,
        },
        Kpi {
            key: "pages",
            label: "Pages checked",
            value: fmt::thousands(done),
            unit: "",
            change_class: "",
            change: String::new(),
            detail: if running {
                format!("of ~{}", fmt::thousands(total))
            } else {
                format!("{} with issues", fmt::thousands(tally.with_issues))
            },
            live: running,
            tag: None,
        },
        Kpi {
            key: "speed",
            label: "Avg response",
            value: avg.map_or("–".to_owned(), |ms| ms.to_string()),
            unit: if avg.is_some() { "ms" } else { "" },
            change_class: "",
            change: String::new(),
            detail: format!("{} pages answered", fmt::thousands(tally.answered)),
            live: running,
            tag: None,
        },
    ]
}

fn filters(
    base: &str,
    on: LiveFilter,
    tally: &LiveTally,
    done: u32,
    live: Option<&LiveProgress>,
) -> Vec<FilterChip> {
    let errors = live.map_or(0, |l| l.errors);
    [
        (LiveFilter::All, "All", done),
        (LiveFilter::Issues, "With issues", tally.with_issues),
        (LiveFilter::Ai, "AI can't reach", tally.ai_blocked),
        (LiveFilter::Errors, "Errors", errors),
    ]
    .into_iter()
    .map(|(f, label, n)| FilterChip {
        label,
        count: fmt::thousands(n),
        href: format!("{base}/audit/watch?f={}", filter_slug(f)),
        on: f == on,
    })
    .collect()
}

fn file_rows(files: LiveFiles, phase: Option<LivePhase>) -> Vec<FileRow> {
    let reached = |p: LivePhase| phase.is_none_or(|now| now > p);
    let mut rows = Vec::new();
    if let Some(s) = files.robots {
        let (tag_class, tag) = match s {
            200..=299 => ("ck-ok", "AI rules read".to_owned()),
            0 => ("ck-crit", "no response: no verdicts for AI bots".to_owned()),
            400..=499 => ("ck-note", "no file: every bot may crawl".to_owned()),
            _ => ("ck-crit", "failing: crawlers back off".to_owned()),
        };
        rows.push(FileRow {
            name: "robots.txt",
            status: pill_status(s),
            pill: pill_class(s),
            tag_class,
            tag,
        });
    }
    if let Some(n) = files.sitemap_urls {
        rows.push(FileRow {
            name: "sitemap.xml",
            status: if n > 0 {
                "200".to_owned()
            } else {
                "–".to_owned()
            },
            pill: if n > 0 { "p-ok" } else { "p-skip" },
            tag_class: if n > 0 { "ck-ok" } else { "ck-warn" },
            tag: if n > 0 {
                format!("{} URLs listed", fmt::thousands(n))
            } else {
                "no sitemap found".to_owned()
            },
        });
    }
    match files.llms_txt {
        Some(s) => rows.push(FileRow {
            name: "llms.txt",
            status: pill_status(s),
            pill: pill_class(s),
            tag_class: if (200..300).contains(&s) {
                "ck-ok"
            } else {
                "ck-note"
            },
            tag: if (200..300).contains(&s) {
                "found: a guide for AI products".to_owned()
            } else {
                "none: optional, helps AI products".to_owned()
            },
        }),
        None if reached(LivePhase::LlmsTxt) => {}
        None => {}
    }
    rows
}

fn pill_status(s: u16) -> String {
    if s == 0 {
        "ERR".to_owned()
    } else {
        s.to_string()
    }
}

fn pill_class(s: u16) -> &'static str {
    match s {
        200..=299 => "p-ok",
        300..=399 => "p-redir",
        _ => "p-err",
    }
}

/// The checks a page's Indexable tag answers for.
fn is_indexing(id: CheckId) -> bool {
    matches!(
        def(id).category,
        Category::Indexability | Category::Response
    )
}

fn severity_dot(s: Severity) -> &'static str {
    match s {
        Severity::Critical => "is-critical",
        Severity::Warning => "is-warning",
        Severity::Notice => "is-notice",
    }
}

fn row_view(r: &LiveRow, counts: &[(CheckId, u32)], pages: u32) -> RowView {
    let ids: Vec<CheckId> = r.issues.iter().filter_map(CheckId::from_bit).collect();
    let (indexing, content): (Vec<CheckId>, Vec<CheckId>) =
        ids.iter().partition(|&&id| is_indexing(id));
    let html_ok = (200..300).contains(&r.status);

    let mut tags = vec![indexable_tag(r, &indexing)];
    if html_ok {
        tags.push(ai_tag(r));
        tags.push(content_tag(&content));
    }

    let points = page_points(counts, pages, r.issues);
    let (impact, impact_class) = if ids.is_empty() {
        ("clean".to_owned(), "imp-clean")
    } else if points < 0.05 {
        ("−0.0".to_owned(), "imp-none")
    } else {
        (format!("−{points:.1}"), "imp-cost")
    };

    let check_items = |list: &[CheckId]| -> Vec<(&'static str, String)> {
        if list.is_empty() {
            vec![("is-ok", "Every check passes".to_owned())]
        } else {
            list.iter()
                .map(|&id| (severity_dot(def(id).severity), def(id).title.to_owned()))
                .collect()
        }
    };
    let mut groups = vec![Group {
        name: "Indexable",
        dot: tags[0].dot(),
        items: {
            let mut items = vec![("is-na", indexability_line(r))];
            items.extend(check_items(&indexing));
            items
        },
        bots: Vec::new(),
    }];
    if html_ok {
        groups.push(Group {
            name: "AI can reach",
            dot: tags[1].dot(),
            items: engine_items(r),
            bots: r
                .probes
                .iter()
                .map(|p| {
                    let class = match p.outcome.as_str() {
                        "same" => "ck-ok",
                        "skipped_robots" | "inconclusive" => "ck-pass",
                        _ => "ck-crit",
                    };
                    (
                        class,
                        format!("{} · {}", p.token, outcome_words(&p.outcome, p.status)),
                    )
                })
                .collect(),
        });
        groups.push(Group {
            name: "Content",
            dot: tags[2].dot(),
            items: check_items(&content),
            bots: Vec::new(),
        });
    }

    RowView {
        n: r.n,
        status: match (r.status, r.error) {
            (0, _) if r.indexability == Indexability::BlockedByRobots => "robots".to_owned(),
            (0, Some(FetchFailure::Timeout)) => "timeout".to_owned(),
            (0, _) => "ERR".to_owned(),
            (s, _) => s.to_string(),
        },
        pill: if r.status == 0 && r.indexability == Indexability::BlockedByRobots {
            "p-skip"
        } else {
            pill_class(r.status)
        },
        url: r.url.clone(),
        path: path_of(&r.url),
        ms: if r.status == 0 {
            String::new()
        } else {
            format!("{} ms", fmt::thousands(r.response_ms))
        },
        tags,
        impact,
        impact_class,
        groups,
    }
}

impl Tag {
    fn dot(&self) -> &'static str {
        match self.class {
            "ck-ok" => "is-ok",
            "ck-crit" => "is-critical",
            "ck-warn" => "is-warning",
            _ => "is-notice",
        }
    }
}

fn indexable_tag(r: &LiveRow, issues: &[CheckId]) -> Tag {
    let tag = |class, icon, detail: &str| Tag {
        class,
        icon,
        name: "Indexable",
        detail: detail.to_owned(),
    };
    match (r.status, r.indexability) {
        (0, Indexability::BlockedByRobots) => tag("ck-warn", "", "· blocked by robots.txt"),
        (0, _) => tag("ck-crit", "", "· no response"),
        (300..=399, _) => tag("ck-note", "", "· redirects"),
        (400..=599, _) => tag("ck-crit", "", &format!("· {}", r.status)),
        (_, Indexability::Noindex) => tag("ck-warn", "", "· noindex"),
        (_, Indexability::Canonicalised) => tag("ck-note", "", "· canonical elsewhere"),
        _ => {
            // Notices (a missing canonical) don't stop indexing: they show when the row opens.
            let serious: Vec<&CheckId> = issues
                .iter()
                .filter(|&&id| def(id).severity != Severity::Notice)
                .collect();
            match serious.iter().map(|&&id| def(id).severity).min() {
                None => tag("ck-ok", "✓ ", ""),
                Some(worst) => tag(severity_tag(worst), "", &issue_count(serious.len())),
            }
        }
    }
}

fn ai_tag(r: &LiveRow) -> Tag {
    let fetch_problems = r
        .probes
        .iter()
        .filter(|p| {
            !matches!(
                p.outcome.as_str(),
                "same" | "skipped_robots" | "inconclusive"
            )
        })
        .count();
    let tag = |class, icon, name, detail: String| Tag {
        class,
        icon,
        name,
        detail,
    };
    if r.ai_of == 0 {
        return tag("ck-note", "", "AI can reach", "· unknown".to_owned());
    }
    let blocked = r.ai_blocked.count_ones() as u8;
    let reach = r.ai_of.saturating_sub(blocked);
    let firewall = if fetch_problems > 0 {
        format!(
            " · {fetch_problems} bot{} refused",
            if fetch_problems == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };
    match (blocked, fetch_problems) {
        (0, 0) => tag(
            "ck-ok",
            "✓ ",
            "AI can reach",
            format!("· {reach}/{}", r.ai_of),
        ),
        (b, _) if b == r.ai_of => tag("ck-crit", "", "AI can't reach", firewall),
        _ => tag(
            "ck-warn",
            "",
            "AI can reach",
            format!("· {reach}/{}{firewall}", r.ai_of),
        ),
    }
}

fn content_tag(issues: &[CheckId]) -> Tag {
    if issues.is_empty() {
        return Tag {
            class: "ck-ok",
            icon: "✓ ",
            name: "Content",
            detail: String::new(),
        };
    }
    let worst = issues
        .iter()
        .map(|&id| def(id).severity)
        .min()
        .unwrap_or(Severity::Notice);
    let detail = if issues.len() == 1 {
        format!("· {}", def(issues[0]).title.to_lowercase())
    } else {
        issue_count(issues.len())
    };
    Tag {
        class: severity_tag(worst),
        icon: "",
        name: "Content",
        detail,
    }
}

fn severity_tag(s: Severity) -> &'static str {
    match s {
        Severity::Critical => "ck-crit",
        Severity::Warning => "ck-warn",
        Severity::Notice => "ck-note",
    }
}

fn issue_count(n: usize) -> String {
    format!("· {n} {}", if n == 1 { "issue" } else { "issues" })
}

fn indexability_line(r: &LiveRow) -> String {
    match r.indexability {
        Indexability::Indexable => "Search engines may index it".to_owned(),
        Indexability::Noindex => "A noindex keeps it out of search".to_owned(),
        Indexability::Canonicalised => "Its canonical points to another page".to_owned(),
        Indexability::Redirected => "It redirects".to_owned(),
        Indexability::ClientError | Indexability::ServerError => {
            format!("It answers {}", r.status)
        }
        Indexability::BlockedByRobots => "robots.txt keeps crawlers out".to_owned(),
    }
}

/// The answer engines that can't use the page, after a line for those that can.
fn engine_items(r: &LiveRow) -> Vec<(&'static str, String)> {
    if r.ai_of == 0 {
        return vec![("is-na", "robots.txt gave no verdicts".to_owned())];
    }
    let out: Vec<(&'static str, String)> = engines()
        .iter()
        .enumerate()
        .take(usize::from(r.ai_of))
        .filter(|(i, _)| r.ai_blocked & (1 << i) != 0)
        .map(|(_, e)| ("is-critical", format!("{} · kept out", engine_short(e.id))))
        .collect();
    let can = usize::from(r.ai_of) - out.len();
    let mut items = vec![(
        if can == 0 { "is-critical" } else { "is-ok" },
        match can {
            0 => "No AI product can use it".to_owned(),
            n if n == usize::from(r.ai_of) => format!("All {n} AI products can use it"),
            n => format!("{n} of {} AI products can use it", r.ai_of),
        },
    )];
    items.extend(out);
    items
}

fn outcome_words(outcome: &str, status: Option<u16>) -> String {
    match outcome {
        "same" => status.map_or("ok".to_owned(), |s| s.to_string()),
        "skipped_robots" => "not asked: robots.txt".to_owned(),
        "inconclusive" => "unclear".to_owned(),
        "throttled" => "rate limited".to_owned(),
        "degraded" => "different page".to_owned(),
        "diverted" => "redirected away".to_owned(),
        s if s.starts_with("challenge_") => "challenge page".to_owned(),
        _ => status.map_or("refused".to_owned(), |s| format!("{s} refused")),
    }
}

fn issues(counts: &[(CheckId, u32)], pages: u32) -> Vec<IssueView> {
    let mut gains = points_regained(counts, pages.max(1));
    gains.sort_by(|a, b| b.1.total_cmp(&a.1));
    gains
        .into_iter()
        .take(ISSUES_SHOWN)
        .filter_map(|(id, pts)| {
            let n = counts.iter().find(|c| c.0 == id)?.1;
            Some(IssueView {
                dot: severity_dot(def(id).severity),
                name: def(id).title,
                count: fmt::pages(n),
                points: format!("+{pts:.1}"),
            })
        })
        .collect()
}

fn end_checks(phase: Option<LivePhase>) -> Vec<EndCheck> {
    let (dot, result) = match phase {
        None => ("is-ok", "done"),
        Some(LivePhase::Bots | LivePhase::Scoring) => ("is-notice", "checking…"),
        _ => ("is-na", "after the pages"),
    };
    [
        "Duplicate titles, descriptions and H1s",
        "Broken and redirected links",
        "Orphan pages and sitemap coverage",
        "AI access report and GEO score",
    ]
    .into_iter()
    .map(|name| EndCheck {
        dot,
        name,
        result: result.to_owned(),
    })
    .collect()
}

fn foot(shown: usize, done: u32, filter: LiveFilter) -> String {
    match filter {
        LiveFilter::All if shown as u32 >= done => format!("{} so far", fmt::pages(done)),
        LiveFilter::All => format!(
            "The newest {} of {} pages",
            fmt::thousands(shown as u32),
            fmt::thousands(done)
        ),
        _ => format!("The newest {} matching pages", fmt::thousands(shown as u32)),
    }
}

/// `https://example.com/a/b?q=1` → `/a/b?q=1`.
fn path_of(url: &str) -> String {
    url.split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|i| rest[i..].to_owned()))
        .unwrap_or_else(|| url.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_core::check::IssueBits;
    use codoseo_core::output::{LiveProbe, Progress};
    use codoseo_store::crawl_queue::CrawlTrigger;
    use uuid::Uuid;

    fn row(n: u32, status: u16) -> LiveRow {
        LiveRow {
            n,
            url: format!("https://shop.example.com/p/{n}"),
            status,
            error: None,
            response_ms: 120,
            indexability: if status == 200 {
                Indexability::Indexable
            } else {
                Indexability::ClientError
            },
            issues: IssueBits::default(),
            ai_blocked: 0,
            ai_of: 10,
            probes: Vec::new(),
        }
    }

    fn crawl(status: CrawlStatus, live: Option<&LiveProgress>) -> Crawl {
        let now = OffsetDateTime::now_utc();
        Crawl {
            id: Uuid::nil(),
            site_id: Uuid::nil(),
            number: 45,
            status,
            trigger: CrawlTrigger::Manual,
            queued_at: now - time::Duration::seconds(90),
            started_at: Some(now - time::Duration::seconds(68)),
            finished_at: None,
            health_score: None,
            checks_passed: None,
            checks_total: None,
            geo_score: None,
            geo_checks_passed: None,
            geo_checks_applicable: None,
            overall_score: None,
            progress: live.map(|l| serde_json::to_value(l).unwrap()),
            summary: None,
            failure_reason: None,
        }
    }

    fn progress(phase: LivePhase, done: u32) -> LiveProgress {
        let mut l = LiveProgress::new(
            Progress {
                pages_done: done,
                queued: 10,
                failures: 0,
                depth: 1,
                elapsed_ms: 68_000,
            },
            &[],
        );
        l.phase = phase;
        l.files = LiveFiles {
            robots: Some(200),
            sitemap_urls: Some(452),
            llms_txt: Some(404),
        };
        l
    }

    fn build(c: &Crawl, rows: &[LiveRow]) -> LiveAudit {
        LiveAudit::build(LiveInput {
            base: "/s/x".to_owned(),
            domain: "shop.example.com".to_owned(),
            crawl: c,
            rows,
            filter: LiveFilter::All,
            previous: None,
            max_pages: 500,
            now: OffsetDateTime::now_utc(),
        })
    }

    #[test]
    fn a_running_crawl_shows_its_stage_count_and_files() {
        let live = progress(LivePhase::Pages, 14);
        let c = crawl(CrawlStatus::Running, Some(&live));
        let v = build(&c, &[row(14, 200)]);
        assert!(v.running);
        assert_eq!(v.signal, "Checking pages");
        assert_eq!(v.count, "14 / 452");
        assert_eq!(v.bar, 3);
        assert!(v.elapsed.starts_with("1:0"));
        let classes: Vec<&str> = v.phases.iter().map(|p| p.class).collect();
        assert_eq!(
            classes,
            ["is-done", "is-done", "is-done", "is-run", "is-wait"]
        );
        assert_eq!(v.phases[1].detail, "452 URLs");
        let files: Vec<&str> = v.files.iter().map(|f| f.name).collect();
        assert_eq!(files, ["robots.txt", "sitemap.xml", "llms.txt"]);
        // Nothing in the output reads as an em dash.
        assert!(!v.to_string().contains('—'));
    }

    #[test]
    fn a_page_gets_three_tags_and_what_it_costs_the_score() {
        let mut r = row(3, 200);
        r.issues.set_check(CheckId::TitleMissing);
        r.ai_blocked = 0b1;
        r.probes = vec![LiveProbe {
            token: "PerplexityBot".to_owned(),
            outcome: "blocked_403".to_owned(),
            status: Some(403),
        }];
        let v = row_view(&r, &[(CheckId::TitleMissing, 1)], 4);
        let names: Vec<&str> = v.tags.iter().map(|t| t.name).collect();
        assert_eq!(names, ["Indexable", "AI can reach", "Content"]);
        assert_eq!(v.tags[0].class, "ck-ok");
        assert_eq!(v.tags[1].class, "ck-warn");
        assert_eq!(v.tags[1].detail, "· 9/10 · 1 bot refused");
        assert_eq!(v.tags[2].detail, "· title is missing");
        assert_eq!(v.impact, "−2.2");
        assert_eq!(
            v.groups[1].bots[0],
            ("ck-crit", "PerplexityBot · 403 refused".to_owned())
        );
        // The opened row names only the products kept out, after a count of the rest.
        let first = engine_short(engines()[0].id);
        assert_eq!(
            v.groups[1].items,
            [
                ("is-ok", "9 of 10 AI products can use it".to_owned()),
                ("is-critical", format!("{first} · kept out")),
            ]
        );
    }

    #[test]
    fn an_error_page_only_says_it_is_not_indexable() {
        let v = row_view(&row(2, 404), &[], 2);
        assert_eq!(v.tags.len(), 1);
        assert_eq!(
            (v.tags[0].class, v.tags[0].detail.as_str()),
            ("ck-crit", "· 404")
        );
        assert_eq!(v.pill, "p-err");
    }

    #[test]
    fn a_finished_crawl_shows_final_scores_and_every_step_done() {
        let live = progress(LivePhase::Scoring, 452);
        let mut c = crawl(CrawlStatus::Done, Some(&live));
        c.finished_at = Some(c.started_at.unwrap() + time::Duration::seconds(124));
        c.health_score = Some(82);
        c.geo_score = Some(64);
        c.overall_score = Some(75);
        let v = build(&c, &[]);
        assert!(!v.running);
        assert!(v.phases.iter().all(|p| p.class == "is-done"));
        assert_eq!(v.done_line, "452 pages in 2:04");
        let scores: Vec<&str> = v.kpis.iter().take(3).map(|k| k.value.as_str()).collect();
        assert_eq!(scores, ["75", "82", "64"]);
    }
}
