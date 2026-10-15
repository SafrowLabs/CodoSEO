//! The site audit's "Fix these, in order" list: one ranked list of the SEO checks that fail and
//! the open AI access issues, grouped Critical, Warning, Notice, each with the points fixing it
//! gives back, whether it is new since the previous crawl, the fix and a coding-agent prompt.
//! Its footer says which blocks the owner marked as intended (they cost nothing), what the
//! crawl fixed since the previous one, and what passes.
//!
//! Everything is built here from what the route loaded, with no database access, so the demo
//! (no GEO score) and the unit tests draw it the same way.

use std::collections::{BTreeSet, HashMap};

use codoseo_checks::{Scope, def, points_regained};
use codoseo_core::check::{CheckId, Severity};
use codoseo_geo::findings::{Evidence, FindingKind};
use codoseo_geo::llms::LlmsState;
use codoseo_geo::remedy::{agent_prompt, copy_fix};
use codoseo_geo::report::AccessReport;
use codoseo_geo::score::{GEO_WEIGHT, GeoCheck, GeoScore, SEO_WEIGHT, Status};
use codoseo_geo::{Intent, Stance, registry};
use codoseo_store::crawls::StoredSummary;
use codoseo_store::geo::Incident;
use time::OffsetDateTime;

use crate::ai_incident::{self, asks_attention};
use crate::fmt;
use crate::routes::ai_access::{Seg, intend_confirm, intended_change, path_of, summary_segments};

/// URL chips a row shows when its pages are expanded.
pub const URLS_SHOWN: usize = 5;

/// One row of the list.
pub struct FixRow {
    /// The row's element id (`fx-seo-http_4xx`, `fx-ai-12`, `fx-ai-llms`).
    pub id: String,
    pub severity: Severity,
    /// `seo` or `ai`.
    pub kind: &'static str,
    /// Not failing on the previous crawl (never on a site's first crawl).
    pub fresh: bool,
    pub title: String,
    pub evidence: Vec<Seg>,
    /// `+6.1 SEO`; `None` when the score that would move is unknown.
    pub gain: Option<String>,
    /// What the gain means, for its title.
    pub gain_tip: String,
    /// Overall points the fix gives back, for the ranking.
    pub rank: f64,
    /// "Mark intended": the form's action and the question it confirms with.
    pub intend: Option<(String, String)>,
    pub fix: String,
    pub prompt: String,
    /// `3 pages`, `site-wide`.
    pub pages: String,
    /// The URL chips, `(path, full URL)`.
    pub urls: Vec<(String, String)>,
    /// Every page in the URL explorer; `None` on the demo and for site-wide checks.
    pub all_href: Option<String>,
}

impl FixRow {
    pub fn severity_slug(&self) -> &'static str {
        severity_slug(self.severity)
    }

    pub fn severity_label(&self) -> &'static str {
        severity_label(self.severity)
    }

    pub fn kind_label(&self) -> &'static str {
        if self.kind == "ai" { "AI" } else { "SEO" }
    }
}

pub fn severity_slug(s: Severity) -> &'static str {
    match s {
        Severity::Critical => "critical",
        Severity::Warning => "warning",
        Severity::Notice => "notice",
    }
}

pub fn severity_label(s: Severity) -> &'static str {
    match s {
        Severity::Critical => "Critical",
        Severity::Warning => "Warning",
        Severity::Notice => "Notice",
    }
}

/// One severity's rows under its sticky header.
pub struct FixGroup {
    pub severity: Severity,
    pub hint: &'static str,
    pub rows: Vec<FixRow>,
}

impl FixGroup {
    pub fn slug(&self) -> &'static str {
        severity_slug(self.severity)
    }

    pub fn label(&self) -> &'static str {
        severity_label(self.severity)
    }
}

pub struct FixList {
    pub groups: Vec<FixGroup>,
    pub total: usize,
    pub critical: usize,
    pub seo: usize,
    pub ai: usize,
    /// Bots blocked as the owner intended: not counted.
    pub intended: Vec<String>,
    /// Failing on the previous crawl, passing now.
    pub fixed: Vec<String>,
    /// Facts that pass, such as `0 broken links`.
    pub passing: Vec<&'static str>,
    pub checks_passed: u32,
    pub checks_total: u32,
    /// Every row's prompt, for "Copy all fix prompts".
    pub all_prompts: String,
}

impl FixList {
    pub fn is_clear(&self) -> bool {
        self.total == 0
    }

    pub fn count(&self, s: Severity) -> usize {
        self.groups
            .iter()
            .filter(|g| g.severity == s)
            .map(|g| g.rows.len())
            .sum()
    }
}

/// The AI side of the list: the latest access report, the intent and the open incidents.
pub struct AiFixInput<'a> {
    /// The site's start URL, which the remedies are written for.
    pub origin: &'a str,
    pub report: &'a AccessReport,
    pub intent: &'a Intent,
    pub open: &'a [Incident],
    pub now: OffsetDateTime,
}

pub struct FixInput<'a> {
    /// `/s/{site}`, or `/demo`.
    pub base: &'a str,
    pub demo: bool,
    pub crawl_id: uuid::Uuid,
    pub current: Option<&'a StoredSummary>,
    /// The previous finished crawl's summary; `None` on the first crawl.
    pub previous: Option<&'a StoredSummary>,
    /// SEO check totals of the crawl.
    pub seo_passed: u32,
    pub seo_total: u32,
    /// Example URLs per failing SEO check.
    pub examples: &'a HashMap<CheckId, Vec<String>>,
    pub ai: Option<AiFixInput<'a>>,
}

/// The checks of a stored summary this version knows, with their counts.
pub fn known_counts(s: &StoredSummary) -> Vec<(CheckId, u32)> {
    s.counts
        .iter()
        .filter_map(|(slug, n)| Some((CheckId::from_slug(slug)?, *n)))
        .collect()
}

/// What changed between two crawls' failing checks: the SEO checks and GEO checks failing now
/// that did not fail before (`new`), and the labels of those that failed before and pass now
/// (`fixed`). With no previous crawl nothing is new and nothing fixed. GEO checks are compared
/// only when both crawls have a breakdown; the four Recommended checks that read SEO checks are
/// left to the SEO side.
#[derive(Debug, Default, PartialEq)]
pub struct CheckDiff {
    pub new_seo: BTreeSet<CheckId>,
    pub new_geo: BTreeSet<GeoCheck>,
    pub fixed: Vec<String>,
}

pub fn diff_checks(previous: Option<&StoredSummary>, current: Option<&StoredSummary>) -> CheckDiff {
    let (Some(prev), Some(cur)) = (previous, current) else {
        return CheckDiff::default();
    };
    let before: BTreeSet<CheckId> = known_counts(prev).into_iter().map(|c| c.0).collect();
    let now: BTreeSet<CheckId> = known_counts(cur).into_iter().map(|c| c.0).collect();
    let mut fixed: Vec<String> = before
        .difference(&now)
        .map(|id| def(*id).title.to_owned())
        .collect();
    let mut new_geo = BTreeSet::new();
    if let (Some(pg), Some(cg)) = (&prev.geo, &cur.geo) {
        let failing = |g: &GeoScore| -> BTreeSet<GeoCheck> {
            g.checks
                .iter()
                .filter(|c| c.status == Status::Fail && !reads_seo(c.check))
                .map(|c| c.check)
                .collect()
        };
        let passing = |g: &GeoScore| -> BTreeSet<GeoCheck> {
            g.checks
                .iter()
                .filter(|c| c.status == Status::Pass)
                .map(|c| c.check)
                .collect()
        };
        let (was, is, ok) = (failing(pg), failing(cg), passing(cg));
        new_geo = is.difference(&was).copied().collect();
        fixed.extend(
            was.iter()
                .filter(|c| ok.contains(c))
                .map(|c| geo_check_label(*c).to_owned()),
        );
    }
    CheckDiff {
        new_seo: now.difference(&before).copied().collect(),
        new_geo,
        fixed,
    }
}

/// The Recommended GEO checks that read SEO checks' counts: the SEO rows stand for them.
fn reads_seo(c: GeoCheck) -> bool {
    matches!(
        c,
        GeoCheck::JsonldInvalid
            | GeoCheck::TitleMissing
            | GeoCheck::DescriptionMissing
            | GeoCheck::Sitemap
    )
}

pub fn geo_check_label(c: GeoCheck) -> &'static str {
    match c {
        GeoCheck::RobotsUnavailable => "robots.txt can be read",
        GeoCheck::BotsBlocked => "AI search bots can reach your pages",
        GeoCheck::BotsNotBlocked => "Bots you block stay out",
        GeoCheck::AnswersRestricted => "Pages can appear in AI answers",
        GeoCheck::FetchBlocked => "A fetch as each AI bot works",
        GeoCheck::ContentMissing => "Text is in the HTML without JavaScript",
        GeoCheck::JsonldInvalid => "Structured data parses",
        GeoCheck::TitleMissing => "Every page has a title",
        GeoCheck::DescriptionMissing => "Every page has a description",
        GeoCheck::Sitemap => "Sitemap is there and answers",
        GeoCheck::LlmsTxt => "llms.txt",
        GeoCheck::ContentSignals => "Content signals",
        GeoCheck::MarkdownNegotiation => "Markdown for agents",
    }
}

/// The GEO check an incident of `kind` counts against.
pub fn geo_check_of(kind: FindingKind) -> GeoCheck {
    match kind {
        FindingKind::RobotsUnavailable => GeoCheck::RobotsUnavailable,
        FindingKind::BotsBlocked => GeoCheck::BotsBlocked,
        FindingKind::BotsNotBlocked => GeoCheck::BotsNotBlocked,
        FindingKind::AnswersRestricted => GeoCheck::AnswersRestricted,
        FindingKind::FetchBlocked => GeoCheck::FetchBlocked,
        FindingKind::ContentMissing => GeoCheck::ContentMissing,
        FindingKind::LlmsTxtUnavailable => GeoCheck::LlmsTxt,
    }
}

/// `+6.1 SEO`, with what it means; `(None, "")` for a gain too small to show.
fn gain(points: f64, score: &str, weight: f64) -> (Option<String>, String) {
    if points < 0.05 {
        return (None, String::new());
    }
    (
        Some(format!("+{points:.1} {score}")),
        format!(
            "Fixing this gives back {points:.1} {score} points ({:.1} overall)",
            points * weight
        ),
    )
}

pub fn fix_list(input: FixInput<'_>) -> FixList {
    let diff = diff_checks(input.previous, input.current);
    let has_previous = input.previous.is_some();
    let geo = input.current.and_then(|s| s.geo.as_ref());
    let mut rows: Vec<FixRow> = Vec::new();

    // SEO: every failing check this version knows.
    if let Some(s) = input.current {
        let counts = known_counts(s);
        let pages = s.report_summary.pages;
        for ((id, points), &(_, n)) in points_regained(&counts, pages).into_iter().zip(&counts) {
            let d = def(id);
            let site_wide = d.scope == Scope::SiteWide;
            let (why, fix) = seo_hint(id);
            let evidence = if site_wide {
                format!("Site-wide. {why}")
            } else {
                format!("On {} of {}. {why}", fmt::thousands(n), fmt::pages(pages))
            };
            let (gain, gain_tip) = gain(points, "SEO", SEO_WEIGHT);
            let urls: Vec<(String, String)> = input
                .examples
                .get(&id)
                .map(|v| {
                    v.iter()
                        .take(URLS_SHOWN)
                        .map(|u| (path_of(u), u.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let prompt = seo_prompt(d.title, &evidence, fix, &urls, n);
            rows.push(FixRow {
                id: format!("fx-seo-{}", id.slug()),
                severity: d.severity,
                kind: "seo",
                fresh: has_previous && diff.new_seo.contains(&id),
                title: d.title.to_owned(),
                evidence: summary_segments(&evidence),
                gain,
                gain_tip,
                rank: points * SEO_WEIGHT,
                intend: None,
                fix: fix.to_owned(),
                prompt,
                pages: if site_wide {
                    "site-wide".to_owned()
                } else {
                    fmt::pages(n)
                },
                urls,
                all_href: (!site_wide && !input.demo)
                    .then(|| format!("{}/explorer?filter=check:{}", input.base, id.slug())),
            });
        }
    }

    // AI: the open incidents that ask for attention, then llms.txt when it is missing.
    let mut intended = Vec::new();
    if let Some(ai) = &input.ai {
        let asking: Vec<&Incident> = ai
            .open
            .iter()
            .filter(|i| asks_attention(i, ai.now))
            .collect();
        let points_of = |c: GeoCheck| -> Option<f64> {
            geo.and_then(|g| g.checks.iter().find(|x| x.check == c))
                .filter(|x| x.status == Status::Fail)
                .map(|x| x.points_regained)
        };
        for i in &asking {
            let ev: Evidence = serde_json::from_value(i.evidence.clone()).unwrap_or_default();
            let check = geo_check_of(i.kind);
            // A check's points are shared by the incidents that fail it.
            let sharing = asking
                .iter()
                .filter(|o| geo_check_of(o.kind) == check)
                .count()
                .max(1);
            let points = points_of(check).map(|p| p / sharing as f64);
            let (gain, gain_tip) = match points {
                Some(p) => gain(p, "GEO", GEO_WEIGHT),
                None => (None, String::new()),
            };
            let f = ai_incident::fix(ai.origin, Some(ai.report), i, &ev);
            let urls: Vec<(String, String)> = ai_incident::affected_urls(&ev)
                .into_iter()
                .take(URLS_SHOWN)
                .map(|u| (path_of(&u), u))
                .collect();
            let total = (ev.urls.total as usize).max(urls.len());
            let fresh = has_previous && i.opened_crawl_id == Some(input.crawl_id);
            rows.push(FixRow {
                id: format!("fx-ai-{}", i.id),
                severity: i.severity,
                kind: "ai",
                fresh,
                title: i.title.clone(),
                evidence: summary_segments(&i.summary),
                gain,
                gain_tip,
                rank: points.unwrap_or(0.0) * GEO_WEIGHT,
                intend: (!input.demo)
                    .then(|| intended_change(i.kind, &i.subject, &ev))
                    .flatten()
                    .map(|c| {
                        (
                            format!(
                                "{}/ai-access/incidents/{}/intended?back=audit",
                                input.base, i.id
                            ),
                            intend_confirm(i.kind, &c),
                        )
                    }),
                fix: copy_fix(&f.remedy),
                prompt: agent_prompt(&f.remedy, &f.facts),
                pages: match total {
                    0 => "site-wide".to_owned(),
                    n => fmt::pages(n as u32),
                },
                urls,
                all_href: None,
            });
        }
        let llms_incident = asking
            .iter()
            .any(|i| i.kind == FindingKind::LlmsTxtUnavailable);
        let missing = ai.report.llms_txt.as_ref().is_some_and(|l| {
            matches!(
                l.state,
                LlmsState::Absent | LlmsState::SoftNotFound | LlmsState::Unavailable
            )
        });
        if missing && !llms_incident {
            let (gain, gain_tip) = match points_of(GeoCheck::LlmsTxt) {
                Some(p) => gain(p, "GEO", GEO_WEIGHT),
                None => (None, String::new()),
            };
            let rank = points_of(GeoCheck::LlmsTxt).unwrap_or(0.0) * GEO_WEIGHT;
            let llms_url = format!("{}/llms.txt", ai.origin.trim_end_matches('/'));
            let fix = format!(
                "# Your site name\n> One or two sentences on what the site offers.\n\n## Key pages\n- [Home]({})\n",
                ai.origin
            );
            let evidence = "/llms.txt is missing. Optional, but it gives AI tools a short list of your key pages.".to_owned();
            rows.push(FixRow {
                id: "fx-ai-llms".to_owned(),
                severity: Severity::Notice,
                kind: "ai",
                fresh: has_previous && diff.new_geo.contains(&GeoCheck::LlmsTxt),
                title: "llms.txt is missing".to_owned(),
                prompt: seo_prompt(
                    "llms.txt is missing",
                    &evidence,
                    &format!("Publish a Markdown file at {llms_url}:\n{fix}"),
                    &[],
                    0,
                ),
                evidence: summary_segments(&evidence),
                gain,
                gain_tip,
                rank,
                intend: None,
                fix,
                pages: "site-wide".to_owned(),
                urls: Vec::new(),
                all_href: None,
            });
        }
        intended = intended_blocks(ai.report, ai.intent);
    }

    // Most severe first; inside a severity, the most points back first.
    rows.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then(b.rank.total_cmp(&a.rank))
            .then(a.kind.cmp(b.kind))
            .then(a.title.cmp(&b.title))
    });
    let total = rows.len();
    let critical = rows
        .iter()
        .filter(|r| r.severity == Severity::Critical)
        .count();
    let seo = rows.iter().filter(|r| r.kind == "seo").count();
    let all_prompts = rows
        .iter()
        .map(|r| r.prompt.as_str())
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    let mut groups: Vec<FixGroup> = Vec::new();
    for r in rows {
        match groups.last_mut() {
            Some(g) if g.severity == r.severity => g.rows.push(r),
            _ => groups.push(FixGroup {
                severity: r.severity,
                hint: match r.severity {
                    Severity::Critical => {
                        "Fix these first. They stop pages from ranking or being cited."
                    }
                    Severity::Warning => "Costing you traffic or AI visibility.",
                    Severity::Notice => "Small wins when you have time.",
                },
                rows: vec![r],
            }),
        }
    }

    let failing: BTreeSet<CheckId> = input
        .current
        .map(|s| known_counts(s).into_iter().map(|c| c.0).collect())
        .unwrap_or_default();
    let mut passing = Vec::new();
    if input.current.is_some() {
        if !failing.contains(&CheckId::LinksToBroken) {
            passing.push("0 broken links");
        }
        if !failing.contains(&CheckId::Http5xx) {
            passing.push("0 server errors");
        }
        if !failing.contains(&CheckId::SitemapMissing) && !failing.contains(&CheckId::SitemapNon200)
        {
            passing.push("Sitemap OK");
        }
    }
    let (geo_passed, geo_applicable) = geo.map_or((0, 0), |g| {
        (u32::from(g.checks_passed), u32::from(g.checks_applicable))
    });

    FixList {
        groups,
        total,
        critical,
        seo,
        ai: total - seo,
        intended,
        fixed: diff.fixed,
        passing,
        checks_passed: input.seo_passed + geo_passed,
        checks_total: input.seo_total + geo_applicable,
        all_prompts,
    }
}

/// Bots kept out as the owner intends (`Block`, and blocked on the home page): not counted.
pub fn intended_blocks(report: &AccessReport, intent: &Intent) -> Vec<String> {
    if !report.has_verdicts() {
        return Vec::new();
    }
    registry()
        .bots
        .iter()
        .filter(|b| intent.effective(b) == Stance::Block)
        .filter(|b| report.bot(&b.token).and_then(|a| a.home_allowed) == Some(false))
        .map(|b| b.token.clone())
        .collect()
}

/// A coding-agent prompt for an SEO check, in the shape of the AI access prompts.
fn seo_prompt(
    title: &str,
    evidence: &str,
    fix: &str,
    urls: &[(String, String)],
    pages: u32,
) -> String {
    let mut out = format!("Issue: {title}\nWhat the crawl found: {evidence}\n");
    if !urls.is_empty() {
        out.push_str("Example URLs:\n");
        for (_, u) in urls {
            out.push_str(&format!("- {u}\n"));
        }
        if pages as usize > urls.len() {
            out.push_str(&format!("and {} more\n", pages as usize - urls.len()));
        }
    }
    out.push_str(&format!(
        "Change:\n{fix}\nKeep every other page as it is. Make the smallest change that fixes this."
    ));
    out
}

/// Why an SEO check matters, and how to fix it.
pub fn seo_hint(id: CheckId) -> (&'static str, &'static str) {
    match id {
        CheckId::Http4xx => (
            "Visitors and crawlers hit a dead end.",
            "Restore each page, or 301-redirect it to its closest live page, and update the internal links that point to it.",
        ),
        CheckId::Http5xx => (
            "The server failed to answer, so the page can't rank or be cited.",
            "Check the server error log for these URLs at the time of the crawl, fix the cause, then run a crawl.",
        ),
        CheckId::FetchFailed => (
            "No answer at all: a timeout, a reset connection or a TLS error.",
            "Check that these URLs answer within a few seconds over HTTPS, and that no firewall drops the crawler.",
        ),
        CheckId::RedirectLoop => (
            "The redirects never reach a page.",
            "Follow the redirects for each URL and point it straight at one final 200 page.",
        ),
        CheckId::Redirected => (
            "Each redirect costs crawlers a hop.",
            "Link to the final URL directly, and keep the redirect only for old links from outside.",
        ),
        CheckId::RedirectChain => (
            "Two or more hops slow crawlers down and leak link value.",
            "Point each old URL straight at its final page with a single 301.",
        ),
        CheckId::Noindex => (
            "Search engines leave these pages out.",
            "Remove the noindex robots meta tag or X-Robots-Tag header from pages that should be found.",
        ),
        CheckId::Canonicalised => (
            "The page asks to be replaced by another URL in search.",
            "Point the canonical at the page itself, unless it really is a copy of the other URL.",
        ),
        CheckId::CanonicalMissing => (
            "Without a canonical, copies of the page can compete with it.",
            "Add <link rel=\"canonical\" href=\"…\"> with the page's own URL to each page.",
        ),
        CheckId::BlockedByRobots => (
            "robots.txt keeps search engines from reading these pages.",
            "Remove or narrow the robots.txt Disallow rule that matches these URLs.",
        ),
        CheckId::CanonicalToNon200 => (
            "The canonical points at a page that doesn't answer 200.",
            "Point each canonical at a live 200 URL.",
        ),
        CheckId::RobotsBlocksSite => (
            "Search engines may not crawl any page.",
            "Remove \"Disallow: /\" for User-agent: * in robots.txt, unless the site should stay out of search.",
        ),
        CheckId::TitleMissing => (
            "Search and AI tools fall back to other text for the page's name.",
            "Add a unique <title> of 30 to 60 characters to each page.",
        ),
        CheckId::TitleTooLong => (
            "Search engines cut it off.",
            "Shorten each title to 60 characters or fewer, with the key words first.",
        ),
        CheckId::TitleTooShort => (
            "A short title says little about the page.",
            "Write a title of 30 to 60 characters that says what the page is about.",
        ),
        CheckId::TitleMultiple => (
            "Crawlers have to guess which title counts.",
            "Keep one <title> element in each page's <head>.",
        ),
        CheckId::TitleDuplicate => (
            "Pages with the same title compete with each other.",
            "Give each page its own title, or canonicalise the duplicates.",
        ),
        CheckId::DescriptionMissing => (
            "Search and AI snippets fall back to random page text.",
            "Add a <meta name=\"description\"> of 70 to 160 characters to each page.",
        ),
        CheckId::DescriptionTooLong => (
            "Search engines cut it off.",
            "Shorten each description to 160 characters or fewer.",
        ),
        CheckId::DescriptionTooShort => (
            "A short description says little about the page.",
            "Write a description of 70 to 160 characters.",
        ),
        CheckId::DescriptionDuplicate => (
            "Pages with the same description look alike in results.",
            "Write a description for each page of its own.",
        ),
        CheckId::H1Missing => (
            "The page has no main heading.",
            "Add one <h1> that says what the page is about.",
        ),
        CheckId::H1Multiple => (
            "More than one main heading blurs the page's topic.",
            "Keep one <h1> per page and make the others <h2>.",
        ),
        CheckId::H1Duplicate => (
            "Pages with the same heading look like duplicates.",
            "Give each page a main heading of its own.",
        ),
        CheckId::ThinContent => (
            "Little text gives search and AI little to go on.",
            "Add useful text, or merge the page into a fuller one and redirect it.",
        ),
        CheckId::ContentDuplicate => (
            "Copies split ranking signals between URLs.",
            "Canonicalise the copies to one URL, or rewrite them so each page is distinct.",
        ),
        CheckId::ImagesMissingAlt => (
            "Screen readers and image search can't tell what the images show.",
            "Add a short alt text describing each image (alt=\"\" for decoration).",
        ),
        CheckId::LinksToBroken => (
            "Visitors and crawlers follow links to dead pages.",
            "Update or remove the links that point to broken URLs.",
        ),
        CheckId::LinksToRedirect => (
            "Each link costs crawlers an extra hop.",
            "Update the links to point at the final URLs.",
        ),
        CheckId::Orphan => (
            "No page links here, so crawlers may never find it.",
            "Link to each page from a related page or the navigation, or remove it from the sitemap.",
        ),
        CheckId::NoInternalOutlinks => (
            "A dead end for crawlers and visitors.",
            "Link from each page to related pages on the site.",
        ),
        CheckId::NofollowInternalLinks => (
            "nofollow on your own links holds back crawling.",
            "Remove rel=\"nofollow\" from links to your own pages.",
        ),
        CheckId::DeepPage => (
            "Pages far from the homepage get crawled less.",
            "Link to these pages from higher up, such as category pages or the navigation.",
        ),
        CheckId::SitemapNon200 => (
            "The sitemap lists URLs that don't answer 200.",
            "Remove redirected and missing URLs from the sitemap, or fix the pages.",
        ),
        CheckId::SitemapNoindex => (
            "The sitemap asks for pages that say not to index them.",
            "Drop noindex pages from the sitemap, or remove their noindex.",
        ),
        CheckId::SitemapCanonicalised => (
            "The sitemap lists URLs that point to another canonical.",
            "List only canonical URLs in the sitemap.",
        ),
        CheckId::NotInSitemap => (
            "Indexable pages missing from the sitemap are found later.",
            "Add every indexable page to the sitemap.",
        ),
        CheckId::SitemapMissing => (
            "A sitemap helps search and AI crawlers find every page.",
            "Publish /sitemap.xml listing the site's indexable URLs, and name it in robots.txt with a Sitemap: line.",
        ),
        CheckId::MixedContent => (
            "Browsers block or warn about HTTP resources on HTTPS pages.",
            "Load every script, style and image over HTTPS.",
        ),
        CheckId::NotHttps => (
            "Pages over plain HTTP are marked not secure.",
            "Serve the site over HTTPS and 301-redirect HTTP to it.",
        ),
        CheckId::SlowResponse => (
            "Slow pages get crawled less and lose visitors.",
            "Find what makes these pages slow (database, uncached rendering) and cache or speed it up.",
        ),
        CheckId::OgMissing => (
            "Shared links show no title or picture.",
            "Add og:title and og:image meta tags to each page.",
        ),
        CheckId::JsonldInvalid => (
            "Search and AI tools can't read the structured data.",
            "Fix the JSON-LD so it parses (check it with a JSON validator).",
        ),
        CheckId::HreflangMissingSelf => (
            "Hreflang sets need an entry for the page itself.",
            "Add a hreflang link for each page's own language and URL.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_core::output::StopReason;
    use codoseo_core::report::CrawlSummary;
    use codoseo_geo::score::{Outcome, geo_score};

    fn stored(counts: &[(&str, u32)], geo: Option<GeoScore>) -> StoredSummary {
        StoredSummary {
            stop_reason: StopReason::Completed,
            report_summary: CrawlSummary {
                pages: 100,
                ..Default::default()
            },
            counts: counts.iter().map(|(s, n)| ((*s).to_owned(), *n)).collect(),
            geo,
        }
    }

    fn geo(fail: &[GeoCheck]) -> GeoScore {
        let outcomes: Vec<(GeoCheck, Outcome)> = GeoCheck::ALL
            .into_iter()
            .filter(|c| !matches!(c, GeoCheck::ContentSignals | GeoCheck::MarkdownNegotiation))
            .map(|c| {
                (
                    c,
                    if fail.contains(&c) {
                        Outcome::Fail {
                            severity: c.nominal(),
                            share: 1.0,
                        }
                    } else {
                        Outcome::Pass
                    },
                )
            })
            .collect();
        geo_score(&outcomes).unwrap()
    }

    #[test]
    fn new_and_fixed_compare_with_the_previous_crawl() {
        let before = stored(
            &[("http_4xx", 3), ("title_missing", 2)],
            Some(geo(&[GeoCheck::BotsBlocked])),
        );
        let after = stored(
            &[("http_4xx", 1), ("http_5xx", 1), ("from_the_future", 1)],
            Some(geo(&[GeoCheck::LlmsTxt])),
        );
        let d = diff_checks(Some(&before), Some(&after));
        assert_eq!(d.new_seo, BTreeSet::from([CheckId::Http5xx]));
        assert_eq!(d.new_geo, BTreeSet::from([GeoCheck::LlmsTxt]));
        assert_eq!(
            d.fixed,
            vec![
                "Title is missing".to_owned(),
                "AI search bots can reach your pages".to_owned()
            ]
        );
        // The first crawl: nothing is new, nothing fixed.
        assert_eq!(diff_checks(None, Some(&after)), CheckDiff::default());
        // Without a GEO breakdown on either side, only the SEO checks compare.
        let old = stored(&[("http_4xx", 3)], None);
        let d = diff_checks(Some(&old), Some(&after));
        assert!(d.new_geo.is_empty());
        assert!(d.fixed.is_empty());
        assert_eq!(d.new_seo, BTreeSet::from([CheckId::Http5xx]));
    }

    #[test]
    fn a_geo_check_reading_an_seo_check_is_left_to_the_seo_row() {
        let before = stored(
            &[("title_missing", 2)],
            Some(geo(&[GeoCheck::TitleMissing])),
        );
        let after = stored(&[], Some(geo(&[])));
        let d = diff_checks(Some(&before), Some(&after));
        assert_eq!(d.fixed, vec!["Title is missing".to_owned()]);
    }

    fn list(current: &StoredSummary, previous: Option<&StoredSummary>) -> FixList {
        fix_list(FixInput {
            base: "/s/x",
            demo: false,
            crawl_id: uuid::Uuid::nil(),
            current: Some(current),
            previous,
            seo_passed: 41,
            seo_total: 44,
            examples: &HashMap::from([(
                CheckId::Http4xx,
                vec!["https://example.com/gone".to_owned()],
            )]),
            ai: None,
        })
    }

    #[test]
    fn the_list_groups_by_severity_and_ranks_by_points() {
        let prev = stored(&[("http_4xx", 2)], None);
        let cur = stored(
            &[
                ("og_missing", 40),
                ("http_4xx", 2),
                ("description_missing", 9),
                ("title_missing", 3),
                ("links_to_broken", 1),
            ],
            None,
        );
        let l = list(&cur, Some(&prev));
        assert_eq!(l.total, 5);
        assert_eq!((l.critical, l.seo, l.ai), (1, 5, 0));
        let slugs: Vec<&str> = l.groups.iter().map(FixGroup::slug).collect();
        assert_eq!(slugs, ["critical", "warning", "notice"]);
        let warning: Vec<&str> = l.groups[1].rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(warning[0], "fx-seo-description_missing", "{warning:?}");
        let gone = &l.groups[0].rows[0];
        assert!(!gone.fresh, "failing on the previous crawl too");
        assert!(l.groups[1].rows.iter().all(|r| r.fresh));
        assert_eq!(
            gone.urls,
            vec![("/gone".to_owned(), "https://example.com/gone".to_owned())]
        );
        assert_eq!(
            gone.all_href.as_deref(),
            Some("/s/x/explorer?filter=check:http_4xx")
        );
        assert!(gone.gain.as_deref().unwrap().ends_with(" SEO"));
        assert!(gone.prompt.contains("https://example.com/gone"));
        assert!(l.all_prompts.contains("Issue: Page returns a 4xx error"));
        // The broken link fails, so it's not among the passing facts.
        assert_eq!(l.passing, vec!["0 server errors", "Sitemap OK"]);
        assert_eq!((l.checks_passed, l.checks_total), (41, 44));
    }

    #[test]
    fn a_clean_crawl_is_all_clear() {
        let l = list(&stored(&[], None), None);
        assert!(l.is_clear());
        assert!(l.groups.is_empty());
        assert_eq!(l.passing.len(), 3);
    }

    #[test]
    fn every_check_has_a_hint() {
        for d in codoseo_checks::CHECKS.iter() {
            let (why, fix) = seo_hint(d.id);
            assert!(!why.is_empty() && !fix.is_empty(), "{:?}", d.id);
        }
    }
}
