//! Aligned plain text.

use std::io::Write;

use codoseo_core::audit::Audit;
use codoseo_core::change::Change;

use super::{
    change_url, clean, count_text, findings, headline, one_line, severity_label, slug, summary,
    write_pairs,
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
    writeln!(w, "\nSummary")?;
    write_pairs(w, "  ", &summary(audit))
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
