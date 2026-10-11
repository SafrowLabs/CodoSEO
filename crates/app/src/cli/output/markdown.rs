//! Markdown.

use std::io::Write;

use codoseo_core::audit::Audit;
use codoseo_core::change::Change;

use super::super::probe::ProbeOutput;
use super::{
    PROBE_CAVEAT, ai_access_findings, audit_probes, change_url, clean, count_text, fetch_text,
    findings, headline, not_probed_lines, one_line, probe_rows, probe_stop_text,
    probe_summary_line, severity_label, slug, summary,
};

fn md_cell(text: &str) -> String {
    clean(text).replace('|', "\\|")
}

pub(super) fn crawl(w: &mut impl Write, audit: &Audit) -> std::io::Result<()> {
    writeln!(
        w,
        "# CodoSEO report for {}\n",
        clean(audit.start_url.as_str())
    )?;
    for (label, value) in headline(audit) {
        writeln!(w, "- **{label}:** {value}")?;
    }
    for (severity, group) in findings(audit) {
        writeln!(w, "\n## {} ({})\n", severity_label(severity), group.len())?;
        writeln!(
            w,
            "| Check | Slug | Affected | Examples |\n|---|---|---|---|"
        )?;
        for f in group {
            let examples = if f.examples.is_empty() {
                "-".to_owned()
            } else {
                f.examples
                    .iter()
                    .map(|u| md_cell(u.as_str()))
                    .collect::<Vec<_>>()
                    .join("<br>")
            };
            writeln!(
                w,
                "| {} | `{}` | {} | {} |",
                f.def.title,
                f.def.id.slug(),
                count_text(&f),
                examples
            )?;
        }
    }
    if let Some(rows) = ai_access_findings(audit) {
        writeln!(w, "\n## AI access\n")?;
        if let Some(probes) = audit_probes(audit) {
            writeln!(w, "{}\n", probe_summary_line(&probes))?;
        }
        if rows.is_empty() {
            writeln!(w, "No AI access issues")?;
        }
        for (severity, title) in rows {
            writeln!(w, "- **{}:** {}", severity_label(severity), md_cell(&title))?;
        }
    }
    writeln!(w, "\n## Summary\n")?;
    for (label, value) in summary(audit) {
        writeln!(w, "- **{label}:** {value}")?;
    }
    Ok(())
}

pub(super) fn probe(w: &mut impl Write, out: &ProbeOutput) -> std::io::Result<()> {
    let r = &out.report;
    writeln!(w, "# AI fetch check for {}\n", md_cell(&out.url))?;
    writeln!(w, "> **Synthetic check.** {PROBE_CAVEAT}\n")?;
    if let Some(page) = r.urls.first() {
        writeln!(
            w,
            "- **Baseline:** CodoSEObot · {}",
            md_cell(&fetch_text(&page.baseline))
        )?;
    }
    writeln!(w, "- **From:** {}", md_cell(&r.from))?;
    writeln!(w, "- **Requests:** {}", r.requests)?;
    if let Some(stop) = probe_stop_text(r) {
        writeln!(w, "- **Stopped:** {stop}")?;
    }
    let rows = probe_rows(r);
    if rows.is_empty() {
        writeln!(w, "\nNothing was probed.")?;
    } else {
        writeln!(
            w,
            "\n| Bot | Result | Status | Bytes | Title |\n|---|---|---|---|---|"
        )?;
        for row in &rows {
            let cells: Vec<String> = row.iter().map(|c| md_cell(c)).collect();
            writeln!(w, "| {} |", cells.join(" | "))?;
        }
    }
    let skipped = not_probed_lines(r);
    if !skipped.is_empty() {
        writeln!(w, "\n## Not probed\n")?;
        for line in skipped {
            writeln!(w, "- {}", md_cell(&line))?;
        }
    }
    writeln!(w, "\n## Findings (default intent)\n")?;
    if out.findings.is_empty() {
        writeln!(w, "No AI fetch issues")?;
    }
    for f in &out.findings {
        writeln!(
            w,
            "- **{}:** {}",
            severity_label(f.severity),
            md_cell(&f.title)
        )?;
    }
    Ok(())
}

pub(super) fn changes(w: &mut impl Write, changes: &[Change]) -> std::io::Result<()> {
    if changes.is_empty() {
        return writeln!(w, "No changes.");
    }
    writeln!(
        w,
        "| Severity | Change | URL | Before → after |\n|---|---|---|---|"
    )?;
    for c in changes {
        writeln!(
            w,
            "| {} | `{}` | {} | {} |",
            slug(&c.severity),
            slug(&c.kind),
            md_cell(&change_url(c)),
            md_cell(&format!("{} → {}", one_line(&c.before), one_line(&c.after)))
        )?;
    }
    Ok(())
}
