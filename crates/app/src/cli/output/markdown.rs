//! Markdown.

use std::io::Write;

use codoseo_core::audit::Audit;
use codoseo_core::change::Change;

use super::{
    change_url, clean, count_text, findings, headline, one_line, severity_label, slug, summary,
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
    writeln!(w, "\n## Summary\n")?;
    for (label, value) in summary(audit) {
        writeln!(w, "- **{label}:** {value}")?;
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
