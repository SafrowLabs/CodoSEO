//! Renderers: each writes one report in one format to any `Write`. `table`, `markdown` and
//! `csv_rows` hold the formats; this file holds what they share.

use std::io::Write;

use codoseo_checks::{CheckDef, Scope, def};
use codoseo_core::audit::Audit;
use codoseo_core::change::Change;
use codoseo_core::check::{CheckId, Severity};
use codoseo_core::output::StopReason;
use serde::Serialize;
use url::Url;

use super::{CliError, CrawlFormat, DiffFormat};

mod csv_rows;
mod markdown;
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

pub fn write_crawl(w: &mut impl Write, format: CrawlFormat, audit: &Audit) -> Result<(), CliError> {
    match format {
        CrawlFormat::Table => table::crawl(w, audit)?,
        CrawlFormat::Md => markdown::crawl(w, audit)?,
        CrawlFormat::Csv => csv_rows::crawl(w, audit)?,
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
    pub(super) pages: u32,
    pub(super) examples: Vec<&'a Url>,
}

impl Finding<'_> {
    pub(super) fn site_wide(&self) -> bool {
        self.def.scope == Scope::SiteWide
    }
}

/// Failing checks grouped by severity, most severe first; empty groups are left out.
pub(super) fn findings(audit: &Audit) -> Vec<(Severity, Vec<Finding<'_>>)> {
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
            pages,
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

/// The AI-access findings saved in the audit as `(severity, title)`, most severe first. `None`
/// when the audit has no AI-access section at all (saved by an older version).
pub(super) fn ai_access_findings(audit: &Audit) -> Option<Vec<(Severity, String)>> {
    let section = audit.ai_access.as_ref()?;
    let mut rows: Vec<(Severity, String)> = section
        .get("findings")
        .and_then(|f| f.as_array())
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let severity = serde_json::from_value(f.get("severity")?.clone()).ok()?;
            Some((severity, f.get("title")?.as_str()?.to_owned()))
        })
        .collect();
    rows.sort_by_key(|(severity, _)| *severity);
    Some(rows)
}

pub(super) fn count_text(f: &Finding) -> String {
    match (f.site_wide(), f.pages) {
        (true, _) => "site-wide".to_owned(),
        (false, 1) => "1 page".to_owned(),
        (false, n) => format!("{n} pages"),
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

    #[test]
    fn one_line_collapses_whitespace_then_cleans() {
        assert_eq!(one_line("a \n  b\u{1b}c"), "a bc");
        assert_eq!(one_line(""), "(none)");
        assert_eq!(one_line("\u{1b}"), "(none)");
    }
}
