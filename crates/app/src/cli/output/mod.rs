//! Renderers: each writes one report in one format to any `Write`. `table`, `markdown` and
//! `csv_rows` hold the formats; this file holds what they share.

use std::io::Write;

use codoseo_checks::{CheckDef, Scope, def};
use codoseo_core::audit::Audit;
use codoseo_core::change::Change;
use codoseo_core::check::{CheckId, Severity};
use codoseo_core::output::StopReason;
use codoseo_geo::probe::{InconclusiveReason, ProbeFetch, ProbeReport, SkipReason, Verdict};
use serde::Serialize;
use url::Url;

use super::gate::Verdict as GateVerdict;
use super::llms::{LlmsFormat, LlmsOutput};
use super::probe::{ProbeFormat, ProbeOutput};
use super::{CliError, CrawlFormat, DiffFormat};

mod csv_rows;
mod github;
mod markdown;
pub mod sarif;
mod table;

/// Examples listed per failing check.
const EXAMPLES: usize = 3;
const SEVERITIES: [Severity; 3] = [Severity::Critical, Severity::Warning, Severity::Notice];

/// Crawled text made safe to print: C0 controls (including ESC, BEL, tab and newline), DEL
/// and the C1 controls U+0080 to U+009F are dropped, so a page can't send escape sequences
/// to the terminal. Every crawled string in the table, markdown and CSV output goes through
/// this; JSON output stays raw because serde escapes control characters.
pub fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// The serde (snake_case) name of a plain enum value.
pub fn slug<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

pub fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "Critical",
        Severity::Warning => "Warning",
        Severity::Notice => "Notice",
    }
}

/// Labels and values in two aligned columns.
pub fn write_pairs(
    w: &mut impl Write,
    indent: &str,
    pairs: &[(&str, String)],
) -> std::io::Result<()> {
    let width = pairs
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0);
    for (label, value) in pairs {
        writeln!(w, "{indent}{label:<width$}  {}", clean(value))?;
    }
    Ok(())
}

/// Pretty JSON and a newline, for the small commands.
pub fn write_json(w: &mut impl Write, value: &impl Serialize) -> Result<(), CliError> {
    serde_json::to_writer_pretty(&mut *w, value)?;
    writeln!(w)?;
    Ok(())
}

pub fn stop_text(stop: &StopReason) -> String {
    match stop {
        StopReason::Completed => "Completed".to_owned(),
        StopReason::PageLimit => "Page limit reached".to_owned(),
        StopReason::TimeLimit => "Time limit reached".to_owned(),
        StopReason::Unreachable(why) => format!("Site unreachable ({})", clean(why)),
        StopReason::Blocked(why) => format!("Blocked ({})", clean(why)),
        StopReason::RobotsBlocked => "Blocked by robots.txt".to_owned(),
    }
}

/// One crawl report. `verdict` is what `--fail-on` and `--baseline` made of it, which the SARIF and
/// GitHub formats mark on each result.
pub fn write_crawl(
    w: &mut impl Write,
    format: CrawlFormat,
    audit: &Audit,
    verdict: &GateVerdict,
) -> Result<(), CliError> {
    match format {
        CrawlFormat::Table => table::crawl(w, audit)?,
        CrawlFormat::Md => markdown::crawl(w, audit)?,
        CrawlFormat::Csv => csv_rows::crawl(w, audit)?,
        CrawlFormat::Sarif => sarif::crawl(w, audit, verdict)?,
        CrawlFormat::Github => github::crawl(w, audit, verdict)?,
        CrawlFormat::Json => {
            serde_json::to_writer(&mut *w, audit)?;
            writeln!(w)?;
        }
    }
    Ok(())
}

/// A failing check with the first pages (in crawl order) that have it.
pub(super) struct Finding<'a> {
    pub(super) def: &'static CheckDef,
    /// The check's title under the audit's policy thresholds.
    pub(super) title: String,
    pub(super) pages: u32,
    /// Of `pages`, those the policy leaves out of the health score.
    pub(super) excluded: u32,
    pub(super) examples: Vec<&'a Url>,
}

impl Finding<'_> {
    pub(super) fn site_wide(&self) -> bool {
        self.def.scope == Scope::SiteWide
    }
}

/// Failing checks grouped by severity, most severe first; empty groups are left out.
pub(super) fn findings(audit: &Audit) -> Vec<(Severity, Vec<Finding<'_>>)> {
    let thresholds = crate::cli::policy::audit_thresholds(audit);
    let found = |id: CheckId, pages: u32| {
        let def = def(id);
        let examples = if def.scope == Scope::SiteWide {
            Vec::new()
        } else {
            let pages = &audit.snapshot.pages;
            pages
                .iter()
                .filter(|p| p.issues.has_check(id))
                .take(EXAMPLES)
                .map(|p| &p.url)
                .collect()
        };
        Finding {
            def,
            title: codoseo_checks::title_for(id, &thresholds).into_owned(),
            pages,
            excluded: audit.report.excluded.of(id),
            examples,
        }
    };
    SEVERITIES
        .into_iter()
        .filter_map(|severity| {
            let group: Vec<Finding> = audit
                .report
                .counts
                .iter()
                .filter(|(id, _)| def(*id).severity == severity)
                .map(|(id, n)| found(*id, *n))
                .collect();
            (!group.is_empty()).then_some((severity, group))
        })
        .collect()
}

/// One AI-access finding of a saved audit, with the first step of its remedy.
pub(super) struct AiRow {
    pub(super) severity: Severity,
    pub(super) title: String,
    /// The first step of the finding's remedy; none for an audit saved before remedies existed.
    pub(super) fix: Option<String>,
}

/// The AI-access findings saved in the audit, most severe first. `None` when the audit has no
/// AI-access section at all (saved by an older version).
pub(super) fn ai_access_findings(audit: &Audit) -> Option<Vec<AiRow>> {
    let section = audit.ai_access.as_ref()?;
    let remedies = section.get("remedies").and_then(|r| r.as_array());
    let mut rows: Vec<AiRow> = section
        .get("findings")
        .and_then(|f| f.as_array())
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let severity = serde_json::from_value(f.get("severity")?.clone()).ok()?;
            // A remedy answers the finding of its kind and subject.
            let fix = remedies
                .into_iter()
                .flatten()
                .find(|r| r.get("kind") == f.get("kind") && r.get("subject") == f.get("subject"))
                .and_then(|r| r.get("steps")?.as_array()?.first()?.as_str())
                .map(str::to_owned);
            Some(AiRow {
                severity,
                title: f.get("title")?.as_str()?.to_owned(),
                fix,
            })
        })
        .collect();
    rows.sort_by_key(|r| r.severity);
    Some(rows)
}

pub(super) fn count_text(f: &Finding) -> String {
    let n = match (f.site_wide(), f.pages) {
        (true, _) => "site-wide".to_owned(),
        (false, 1) => "1 page".to_owned(),
        (false, n) => format!("{n} pages"),
    };
    match f.excluded {
        0 => n,
        e => format!("{n}, {e} excluded from the health score"),
    }
}

pub(super) fn headline(audit: &Audit) -> Vec<(&'static str, String)> {
    let r = &audit.report;
    vec![
        ("Health score", format!("{} / 100", r.health_score)),
        (
            "Checks",
            format!("{} of {} checks passed", r.checks_passed, r.checks_total),
        ),
        ("Stop reason", stop_text(&audit.snapshot.stop)),
    ]
}

pub(super) fn summary(audit: &Audit) -> Vec<(&'static str, String)> {
    let s = &audit.report.summary;
    let c = &s.status;
    let last = s.depth.len().saturating_sub(1);
    let mut depth: Vec<String> = s
        .depth
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(clicks, n)| {
            let plus = if clicks == last && clicks >= 10 {
                "+"
            } else {
                ""
            };
            format!("{clicks}{plus}: {n}")
        })
        .collect();
    if s.no_depth > 0 {
        depth.push(format!("sitemap only: {}", s.no_depth));
    }
    vec![
        ("Pages", format!("{} ({} indexable)", s.pages, s.indexable)),
        (
            "Status",
            format!(
                "2xx {} · 3xx {} · 4xx {} · 5xx {} · no response {} · blocked {}",
                c.ok, c.redirect, c.client_error, c.server_error, c.failed, c.blocked
            ),
        ),
        (
            "Click depth",
            if depth.is_empty() {
                "-".to_owned()
            } else {
                depth.join(" · ")
            },
        ),
        ("Avg response", format!("{} ms", s.avg_response_ms)),
        (
            "Duration",
            format!("{:.1} s", audit.duration_ms as f64 / 1000.0),
        ),
    ]
}

pub fn write_changes(
    w: &mut impl Write,
    format: DiffFormat,
    changes: &[Change],
) -> Result<(), CliError> {
    match format {
        DiffFormat::Json => write_json(w, &changes)?,
        DiffFormat::Table => table::changes(w, changes)?,
        DiffFormat::Md => markdown::changes(w, changes)?,
    }
    Ok(())
}

/// Said first wherever probe results are shown.
pub(super) const PROBE_CAVEAT: &str = "Requests sent from this machine with each bot's documented \
    user agent and compared with CodoSEObot's. Real bots come from their own addresses and may be \
    treated differently.";

/// Said first wherever llms.txt results are shown, after an "Informational · not scored" label
/// each format sets in its own style.
pub(super) const LLMS_BANNER: &str =
    "llms.txt is optional: nothing depends on it, and it never affects a health score.";

/// Said under the raw HTML content block.
pub(super) const RAW_NOTE: &str = "Read from the HTML as served, before JavaScript runs. AI \
    crawlers like GPTBot, ClaudeBot and PerplexityBot don't run JavaScript (Vercel and MERJ, 2024).";

/// A content verdict in words.
pub(super) fn content_verdict(v: codoseo_geo::content::ContentVerdict) -> &'static str {
    use codoseo_geo::content::ContentVerdict;
    match v {
        ContentVerdict::Shell => "Shell",
        ContentVerdict::LikelyShell => "Likely shell",
        ContentVerdict::Thin => "Thin",
        ContentVerdict::Present => "Present",
    }
}

/// The labelled facts of a page's raw HTML content, as shown by `check`.
pub(super) fn raw_content_pairs(
    c: &codoseo_geo::content::RawContent,
) -> Vec<(&'static str, String)> {
    use codoseo_core::page::JsonLdStatus;
    let yes = |b: bool| if b { "yes" } else { "no" }.to_owned();
    let jsonld = match c.jsonld {
        JsonLdStatus::Absent => "no".to_owned(),
        JsonLdStatus::Valid(n) => format!("yes ({n})"),
        JsonLdStatus::Invalid => "invalid".to_owned(),
        JsonLdStatus::TooLarge => "too large to check".to_owned(),
    };
    let mut pairs = vec![
        ("Verdict", content_verdict(c.verdict).to_owned()),
        ("Words", format!("{} ({} in main)", c.words, c.main_words)),
        ("Title", yes(c.title)),
        ("H1", yes(c.h1)),
        ("Description", yes(c.description)),
        ("Canonical", yes(c.canonical)),
        ("JSON-LD", jsonld),
        ("Internal links", c.links_internal.to_string()),
        (
            "Scripts",
            format!(
                "{} external, {}% of the page inline",
                c.scripts_external, c.script_share
            ),
        ),
    ];
    if !c.signals.is_empty() {
        let signals: Vec<String> = c.signals.iter().map(slug).collect();
        pairs.push(("Shell signals", signals.join(", ")));
    }
    pairs
}

pub fn write_llms(
    w: &mut impl Write,
    format: LlmsFormat,
    out: &LlmsOutput,
) -> Result<(), CliError> {
    match format {
        LlmsFormat::Json => write_json(w, out)?,
        LlmsFormat::Table => table::llms(w, out)?,
        LlmsFormat::Markdown => markdown::llms(w, out)?,
    }
    Ok(())
}

/// The lines every llms.txt format shares: what it found and each check, `(label, value)`.
pub(super) fn llms_facts(out: &LlmsOutput) -> Vec<(&'static str, String)> {
    use codoseo_geo::llms::LlmsState;
    let r = &out.report;
    let mut pairs = vec![
        ("URL", out.url.clone()),
        ("State", r.state.label().to_owned()),
    ];
    match (&r.error, r.status) {
        (Some(e), _) => pairs.push(("Fetch", format!("no answer ({e})"))),
        (None, 0) => pairs.push(("Fetch", "no answer".to_owned())),
        (None, s) => pairs.push(("Fetch", format!("HTTP {s}"))),
    }
    if r.state == LlmsState::Available {
        pairs.push(("Adopted", "yes".to_owned()));
        if let Some(t) = &r.title {
            pairs.push(("Title", t.clone()));
        }
        let l = &r.link_summary;
        pairs.push((
            "Links",
            format!(
                "{} checked, {} failing, {} not checked (other sites or past the limit)",
                l.checked, l.failing, l.not_checked
            ),
        ));
    }
    pairs
}

pub fn write_probe(
    w: &mut impl Write,
    format: ProbeFormat,
    out: &ProbeOutput,
) -> Result<(), CliError> {
    match format {
        ProbeFormat::Json => write_json(w, out)?,
        ProbeFormat::Table => table::probe(w, out)?,
        ProbeFormat::Markdown => markdown::probe(w, out)?,
    }
    Ok(())
}

/// A verdict in the words of the web's fetch check table.
pub(super) fn verdict_text(v: &Verdict) -> String {
    match v {
        Verdict::Same => "Same as CodoSEObot".to_owned(),
        Verdict::Blocked { status: Some(s) } => format!("Blocked ({s})"),
        Verdict::Blocked { status: None } => "Blocked (connection dropped)".to_owned(),
        Verdict::Challenged { vendor } => format!("Challenged ({})", vendor.label()),
        Verdict::Diverted { to } => format!("Redirected to {}", clean(to)),
        Verdict::Degraded => "Different page".to_owned(),
        Verdict::Throttled => "Rate limited".to_owned(),
        Verdict::Inconclusive { reason } => format!(
            "Inconclusive ({})",
            match reason {
                InconclusiveReason::BaselineFailed => "CodoSEObot got no HTML page",
                InconclusiveReason::BaselineChallenged => "CodoSEObot was challenged too",
                InconclusiveReason::ProbeError => "no response",
                InconclusiveReason::Unstable => "the re-fetch disagreed",
                InconclusiveReason::Unconfirmed => "no room left to confirm",
                InconclusiveReason::Stopped => "the time limit came first",
            }
        ),
        Verdict::SkippedRobots => "Not probed: robots.txt".to_owned(),
    }
}

/// `HTTP 200 · 1234 bytes · "Title"`, or the error.
pub(super) fn fetch_text(f: &ProbeFetch) -> String {
    let mut parts = Vec::new();
    match (f.status, f.error) {
        (Some(s), _) => parts.push(format!("HTTP {s}")),
        (None, Some(e)) => parts.push(format!("no response ({})", slug(&e))),
        (None, None) => parts.push("no response".to_owned()),
    }
    if f.status.is_some() {
        parts.push(format!("{} bytes", f.bytes));
    }
    if let Some(t) = &f.title {
        parts.push(format!("\"{}\"", clean(t)));
    }
    parts.join(" · ")
}

/// One row per probed bot on the first probed page: token, result, status, bytes, title.
pub(super) fn probe_rows(r: &ProbeReport) -> Vec<[String; 5]> {
    let Some(page) = r.urls.first() else {
        return Vec::new();
    };
    page.results
        .iter()
        .map(|res| {
            let f = res.fetch.as_ref();
            [
                res.token.clone(),
                verdict_text(&res.verdict),
                f.and_then(|f| f.status)
                    .map_or_else(|| "-".to_owned(), |s| s.to_string()),
                f.filter(|f| f.status.is_some())
                    .map_or_else(|| "-".to_owned(), |f| f.bytes.to_string()),
                f.and_then(|f| f.title.as_deref())
                    .map_or_else(|| "-".to_owned(), clean),
            ]
        })
        .collect()
}

/// The bots left out of a probe run: those named with their reason, then a count of the crawlers
/// that are never probed (training, ads, agents).
pub(super) fn not_probed_lines(r: &ProbeReport) -> Vec<String> {
    let mut lines: Vec<String> = r
        .skipped_bots
        .iter()
        .filter_map(|s| {
            let why = match s.reason {
                SkipReason::NoUserAgent => "no documented user agent",
                SkipReason::VerifiedSearchEngine => "verified search engine",
                SkipReason::NotProbedPurpose => return None,
            };
            Some(format!("{}: {why}", s.token))
        })
        .collect();
    let others = r
        .skipped_bots
        .iter()
        .filter(|s| s.reason == SkipReason::NotProbedPurpose)
        .count();
    if others > 0 {
        lines.push(format!(
            "{others} other crawlers (training, ads, agents) are never probed"
        ));
    }
    lines
}

/// Why a run stopped early, in words.
pub(super) fn probe_stop_text(r: &ProbeReport) -> Option<&'static str> {
    use codoseo_geo::probe::ProbeStop;
    r.stop.map(|s| match s {
        ProbeStop::Budget => "stopped early: time budget used up",
        ProbeStop::RequestCap => "stopped early: request cap reached",
    })
}

/// The probes saved in an audit's AI access section (`crawl --probes`).
pub(super) fn audit_probes(audit: &Audit) -> Option<ProbeReport> {
    let probes = audit.ai_access.as_ref()?.get("report")?.get("probes")?;
    serde_json::from_value(probes.clone()).ok()
}

/// `Synthetic fetch check: 3 pages × 13 bots, 2 results worse than CodoSEObot (45 requests)`.
pub(super) fn probe_summary_line(r: &ProbeReport) -> String {
    let worse = r
        .urls
        .iter()
        .flat_map(|u| &u.results)
        .filter(|res| res.verdict.is_positive())
        .count();
    let pages = r.urls.len();
    format!(
        "Synthetic fetch check: {pages} page{} × {} bots, {worse} result{} worse than CodoSEObot ({} requests)",
        if pages == 1 { "" } else { "s" },
        r.bots.len(),
        if worse == 1 { "" } else { "s" },
        r.requests
    )
}

pub(super) fn one_line(text: &str) -> String {
    let line = clean(&text.split_whitespace().collect::<Vec<_>>().join(" "));
    if line.is_empty() {
        "(none)".to_owned()
    } else {
        line
    }
}

pub(super) fn change_url(c: &Change) -> String {
    c.url
        .as_ref()
        .map_or_else(|| "-".to_owned(), |u| clean(u.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_drops_c0_del_and_c1_controls_and_keeps_text() {
        assert_eq!(clean("Evil\u{1b}[31mRed"), "Evil[31mRed");
        assert_eq!(
            clean("a\u{0}b\u{7}c\u{7f}d\u{85}e\u{9b}f\u{9f}g"),
            "abcdefg"
        );
        assert_eq!(clean("tab\there\nline\r"), "tabhereline");
        assert_eq!(clean("Zürich → 東京 \u{a0}ok"), "Zürich → 東京 \u{a0}ok");
    }

    fn llms_out(summary: &str) -> LlmsOutput {
        let fetch = codoseo_core::output::LlmsTxtFetch {
            status: 404,
            ..Default::default()
        };
        LlmsOutput {
            url: "https://example.com/llms.txt".to_owned(),
            informational: true,
            summary: summary.to_owned(),
            report: codoseo_geo::llms::assess(&fetch, false),
        }
    }

    #[test]
    fn the_llms_banner_says_informational_once_in_each_format() {
        let out = llms_out("No llms.txt.");
        let mut table_out = Vec::new();
        table::llms(&mut table_out, &out).unwrap();
        let table_out = String::from_utf8(table_out).unwrap();
        assert!(
            table_out.starts_with(
                "INFORMATIONAL · NOT SCORED. llms.txt is optional: nothing depends on it, and it never affects a health score.\n"
            ),
            "{table_out}"
        );
        assert_eq!(table_out.to_lowercase().matches("not scored").count(), 1);
        let mut md = Vec::new();
        markdown::llms(&mut md, &out).unwrap();
        let md = String::from_utf8(md).unwrap();
        assert!(
            md.contains(
                "\n> **Informational · not scored.** llms.txt is optional: nothing depends on it, and it never affects a health score.\n"
            ),
            "{md}"
        );
        assert_eq!(md.to_lowercase().matches("not scored").count(), 1, "{md}");
    }

    #[test]
    fn the_llms_table_cleans_its_summary() {
        let out = llms_out("/llms.txt could not be fetched (\u{1b}[31mred).");
        let mut table_out = Vec::new();
        table::llms(&mut table_out, &out).unwrap();
        let table_out = String::from_utf8(table_out).unwrap();
        assert!(!table_out.contains('\u{1b}'), "{table_out:?}");
        assert!(table_out.contains("([31mred)."), "{table_out}");
    }

    #[test]
    fn one_line_collapses_whitespace_then_cleans() {
        assert_eq!(one_line("a \n  b\u{1b}c"), "a bc");
        assert_eq!(one_line(""), "(none)");
        assert_eq!(one_line("\u{1b}"), "(none)");
    }

    fn audit_with(ai_access: Option<serde_json::Value>) -> Audit {
        use codoseo_core::audit::AUDIT_FORMAT_VERSION;
        use codoseo_core::report::{CrawlReport, CrawlSummary};
        use codoseo_core::snapshot::Snapshot;
        let url = Url::parse("https://example.com/").unwrap();
        Audit {
            format_version: AUDIT_FORMAT_VERSION,
            tool_version: "0.0.1".to_owned(),
            created_at: 0,
            duration_ms: 0,
            start_url: url.clone(),
            report: CrawlReport {
                health_score: 100,
                checks_passed: 1,
                checks_total: 1,
                counts: Vec::new(),
                inlink_samples: Vec::new(),
                summary: CrawlSummary::default(),
                excluded: Default::default(),
            },
            snapshot: Snapshot {
                origin: url,
                stop: codoseo_core::output::StopReason::Completed,
                pages: Vec::new(),
                robots: None,
                sitemap: Default::default(),
            },
            ai_access,
            policy: None,
        }
    }

    fn section(step: Option<&str>) -> serde_json::Value {
        let remedies = step.map_or_else(Vec::new, |s| {
            vec![serde_json::json!({
                "kind": "bots_blocked", "subject": "search", "fix": "x", "steps": [s, "second"],
                "verify": [], "grade": "A", "sources": []
            })]
        });
        serde_json::json!({
            "report": {},
            "findings": [
                {"kind": "bots_blocked", "subject": "search", "severity": "critical", "title": "Search bots are blocked"},
                {"kind": "llms_txt_unavailable", "subject": "", "severity": "notice", "title": "llms.txt is gone"}
            ],
            "remedies": remedies
        })
    }

    #[test]
    fn a_fix_line_follows_each_ai_finding_that_has_a_remedy_and_is_escaped_like_other_cells() {
        let audit = audit_with(Some(section(Some("Edit a|b\\c\u{1b}[31m"))));
        let mut md = Vec::new();
        markdown::crawl(&mut md, &audit).unwrap();
        let md = String::from_utf8(md).unwrap();
        assert!(
            md.contains("- **Critical:** Search bots are blocked\n  - Fix: Edit a\\|b\\\\c[31m\n"),
            "{md}"
        );
        // Only the first step, and only for the finding the remedy answers.
        assert!(!md.contains("second"), "{md}");
        assert_eq!(md.matches("Fix:").count(), 1, "{md}");

        let mut table_out = Vec::new();
        table::crawl(&mut table_out, &audit).unwrap();
        let table_out = String::from_utf8(table_out).unwrap();
        assert!(
            table_out.contains(
                "  Critical  Search bots are blocked\n            Fix: Edit a|b\\c[31m\n"
            ),
            "{table_out}"
        );
    }

    #[test]
    fn audits_saved_without_remedies_print_no_fix_line() {
        let audit = audit_with(Some(section(None)));
        let mut md = Vec::new();
        markdown::crawl(&mut md, &audit).unwrap();
        let md = String::from_utf8(md).unwrap();
        assert!(md.contains("Search bots are blocked"), "{md}");
        assert!(!md.contains("Fix:"), "{md}");
    }
}
