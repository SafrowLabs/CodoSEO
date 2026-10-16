//! GitHub Actions workflow commands: one annotation per failing check and per AI access finding,
//! then a short summary in plain text.
//!
//! What tripped `--fail-on` is an `::error`; other critical and warning results are `::warning`
//! and notices `::notice`. GitHub shows a limited number of annotations per step, so the ones that
//! tripped come first. Annotations name no file: a site has none in the repository.

use std::io::Write;

use codoseo_core::audit::Audit;
use codoseo_core::check::Severity;

use super::super::gate::{Novelty, Verdict, audit_findings, finding_rule};
use super::{clean, count_text, findings, headline, write_pairs};

/// A workflow command's message: `%`, CR and LF escaped as GitHub documents.
fn escape_data(s: &str) -> String {
    clean_keep_newlines(s)
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// A workflow command's property value: the message escapes plus `:` and `,`.
fn escape_property(s: &str) -> String {
    escape_data(s).replace(':', "%3A").replace(',', "%2C")
}

/// Crawled text without control characters, keeping the line breaks a message may carry.
fn clean_keep_newlines(s: &str) -> String {
    s.split('\n').map(clean).collect::<Vec<_>>().join("\n")
}

struct Annotation {
    tripped: bool,
    severity: Severity,
    title: String,
    message: String,
}

impl Annotation {
    fn command(&self) -> &'static str {
        match (self.tripped, self.severity) {
            (true, _) => "error",
            (false, Severity::Critical | Severity::Warning) => "warning",
            (false, Severity::Notice) => "notice",
        }
    }
}

fn new_note(verdict: &Verdict, novelty: Novelty) -> &'static str {
    match (verdict.baseline, novelty) {
        (false, _) => "",
        (true, Novelty::New) => " [new since the baseline]",
        (true, Novelty::Updated) => " [worse than the baseline]",
        (true, Novelty::Unchanged) => " [also in the baseline]",
    }
}

pub fn crawl(w: &mut impl Write, audit: &Audit, verdict: &Verdict) -> std::io::Result<()> {
    let mut notes = Vec::new();
    for (_, group) in findings(audit) {
        for f in group {
            let examples: Vec<&str> = f.examples.iter().map(|u| u.as_str()).collect();
            let novelty = if f.examples.is_empty() {
                verdict.issue(f.def.id, None)
            } else if audit
                .snapshot
                .pages
                .iter()
                .filter(|p| p.issues.has_check(f.def.id))
                .any(|p| verdict.issue(f.def.id, Some(p.url.as_str())) == Novelty::New)
            {
                Novelty::New
            } else {
                Novelty::Unchanged
            };
            let mut message = format!(
                "{} ({}){}",
                f.title,
                count_text(&f),
                new_note(verdict, novelty)
            );
            if !examples.is_empty() {
                message.push_str(&format!("\ne.g. {}", examples.join("\n")));
            }
            notes.push(Annotation {
                tripped: verdict.trips(f.def.id.slug()),
                severity: f.def.severity,
                title: format!("CodoSEO {}", f.def.id.slug()),
                message,
            });
        }
    }
    for f in audit_findings(audit) {
        let rule = finding_rule(&f);
        notes.push(Annotation {
            tripped: verdict.trips(&rule),
            severity: f.severity,
            title: format!("CodoSEO AI access {rule} (grade {})", f.grade.label()),
            message: format!(
                "{}{}\n{}",
                f.title,
                new_note(verdict, verdict.finding(&f)),
                f.summary
            ),
        });
    }
    for t in verdict
        .tripped
        .iter()
        .filter(|t| t.rule.starts_with("change:"))
    {
        notes.push(Annotation {
            tripped: true,
            severity: t.severity,
            title: format!("CodoSEO {}", t.rule),
            message: t.text.clone(),
        });
    }
    notes.sort_by_key(|n| (!n.tripped, n.severity));
    for n in &notes {
        writeln!(
            w,
            "::{} title={}::{}",
            n.command(),
            escape_property(&n.title),
            escape_data(&n.message)
        )?;
    }

    writeln!(w, "CodoSEO report for {}", clean(audit.start_url.as_str()))?;
    write_pairs(w, "", &headline(audit))?;
    let ai = audit_findings(audit).len();
    writeln!(w, "AI access findings  {ai}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_command_text_is_escaped() {
        // Carriage returns are dropped with the other control characters; line breaks are escaped.
        assert_eq!(escape_data("100% done\r\nnext"), "100%25 done%0Anext");
        assert_eq!(escape_property("a: b, c"), "a%3A b%2C c");
        // A hostile page title can't start a new workflow command or reach the terminal.
        assert_eq!(escape_data("x\n::error::y\x1b[2J"), "x%0A::error::y[2J");
    }
}
