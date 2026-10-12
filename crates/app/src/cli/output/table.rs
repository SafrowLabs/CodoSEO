//! Aligned plain text.

use std::io::Write;

use codoseo_core::audit::Audit;
use codoseo_core::change::Change;

use super::super::llms::LlmsOutput;
use super::super::probe::ProbeOutput;
use super::{
    LLMS_BANNER, PROBE_CAVEAT, ai_access_findings, audit_probes, change_url, clean, count_text,
    fetch_text, findings, headline, llms_facts, not_probed_lines, one_line, probe_rows,
    probe_stop_text, probe_summary_line, severity_label, slug, summary, write_pairs,
};

pub(super) fn crawl(w: &mut impl Write, audit: &Audit) -> std::io::Result<()> {
    writeln!(
        w,
        "CodoSEO report for {}\n",
        clean(audit.start_url.as_str())
    )?;
    write_pairs(w, "", &headline(audit))?;

    let groups = findings(audit);
    let title_width = groups
        .iter()
        .flat_map(|(_, g)| g)
        .map(|f| f.def.title.chars().count())
        .max()
        .unwrap_or(0);
    let slug_width = groups
        .iter()
        .flat_map(|(_, g)| g)
        .map(|f| f.def.id.slug().len())
        .max()
        .unwrap_or(0);
    for (severity, group) in &groups {
        writeln!(w, "\n{} ({})", severity_label(*severity), group.len())?;
        for f in group {
            writeln!(
                w,
                "  {:<title_width$}  {:<slug_width$}  {}",
                f.def.title,
                f.def.id.slug(),
                count_text(f)
            )?;
            for url in &f.examples {
                writeln!(w, "      {}", clean(url.as_str()))?;
            }
        }
    }
    if groups.is_empty() {
        writeln!(w, "\nNo issues found.")?;
    }
    if let Some(rows) = ai_access_findings(audit) {
        writeln!(w, "\nAI access")?;
        if let Some(probes) = audit_probes(audit) {
            writeln!(w, "  {}", probe_summary_line(&probes))?;
        }
        if rows.is_empty() {
            writeln!(w, "  No AI access issues")?;
        }
        for (severity, title) in rows {
            writeln!(w, "  {:<8}  {}", severity_label(severity), clean(&title))?;
        }
    }
    writeln!(w, "\nSummary")?;
    write_pairs(w, "  ", &summary(audit))
}

pub(super) fn llms(w: &mut impl Write, out: &LlmsOutput) -> std::io::Result<()> {
    writeln!(w, "{LLMS_BANNER}\n")?;
    write_pairs(w, "", &llms_facts(out))?;
    if !out.report.checks.is_empty() {
        writeln!(w, "\nChecks")?;
        let width = out
            .report
            .checks
            .iter()
            .map(|c| c.id.label().chars().count())
            .max()
            .unwrap_or(0);
        for c in &out.report.checks {
            writeln!(
                w,
                "  {}  {:<width$}  {}",
                if c.ok { "ok  " } else { "FAIL" },
                c.id.label(),
                clean(&c.detail)
            )?;
        }
    }
    if !out.report.link_summary.failing_urls.is_empty() {
        writeln!(w, "\nFailing links")?;
        for u in &out.report.link_summary.failing_urls {
            writeln!(w, "  {}", clean(u))?;
        }
    }
    writeln!(w, "\n{}", out.summary)?;
    Ok(())
}

pub(super) fn probe(w: &mut impl Write, out: &ProbeOutput) -> std::io::Result<()> {
    writeln!(w, "SYNTHETIC CHECK. {PROBE_CAVEAT}\n")?;
    let r = &out.report;
    let mut pairs = vec![("URL", out.url.clone())];
    if let Some(page) = r.urls.first() {
        pairs.push((
            "Baseline",
            format!("CodoSEObot · {}", fetch_text(&page.baseline)),
        ));
    }
    pairs.push(("From", r.from.clone()));
    pairs.push(("Requests", r.requests.to_string()));
    if let Some(stop) = probe_stop_text(r) {
        pairs.push(("Stopped", stop.to_owned()));
    }
    write_pairs(w, "", &pairs)?;

    let rows = probe_rows(r);
    if rows.is_empty() {
        writeln!(w, "\nNothing was probed.")?;
    } else {
        let width = |col: usize, head: &str| {
            rows.iter()
                .map(|r| r[col].chars().count())
                .chain([head.len()])
                .max()
                .unwrap_or(0)
        };
        let (bot, result, status, bytes) = (
            width(0, "Bot"),
            width(1, "Result"),
            width(2, "Status"),
            width(3, "Bytes"),
        );
        writeln!(
            w,
            "\n{:<bot$}  {:<result$}  {:<status$}  {:<bytes$}  Title",
            "Bot", "Result", "Status", "Bytes"
        )?;
        for r in &rows {
            writeln!(
                w,
                "{:<bot$}  {:<result$}  {:<status$}  {:<bytes$}  {}",
                r[0], r[1], r[2], r[3], r[4]
            )?;
        }
    }
    let skipped = not_probed_lines(r);
    if !skipped.is_empty() {
        writeln!(w, "\nNot probed")?;
        for line in skipped {
            writeln!(w, "  {line}")?;
        }
    }
    writeln!(w, "\nFindings (default intent)")?;
    if out.findings.is_empty() {
        writeln!(w, "  No AI fetch issues")?;
    }
    for f in &out.findings {
        writeln!(
            w,
            "  {:<8}  {}",
            severity_label(f.severity),
            clean(&f.title)
        )?;
    }
    Ok(())
}

pub(super) fn changes(w: &mut impl Write, changes: &[Change]) -> std::io::Result<()> {
    if changes.is_empty() {
        return writeln!(w, "No changes.");
    }
    let rows: Vec<[String; 4]> = changes
        .iter()
        .map(|c| {
            [
                slug(&c.severity),
                slug(&c.kind),
                change_url(c),
                format!("{} → {}", one_line(&c.before), one_line(&c.after)),
            ]
        })
        .collect();
    let width = |col: usize| {
        rows.iter()
            .map(|r| r[col].chars().count())
            .max()
            .unwrap_or(0)
    };
    let (sev, kind, url) = (width(0), width(1), width(2));
    for r in &rows {
        writeln!(
            w,
            "{:<sev$}  {:<kind$}  {:<url$}  {}",
            r[0], r[1], r[2], r[3]
        )?;
    }
    Ok(())
}
