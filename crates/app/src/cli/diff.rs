//! `codoseo diff`: what changed between two saved audits.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Args;
use codoseo_core::audit::Audit;
use codoseo_core::output::StopReason;
use codoseo_diff::{diff, key_pages};

use super::output::{clean, write_changes};
use super::{CliError, DiffFormat, EXIT_FAIL_ON, EXIT_OK, FailOn, Outcome, open_output};

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// The earlier audit (`crawl --format json`)
    before: PathBuf,
    /// The later audit
    after: PathBuf,
    #[arg(long, value_enum, default_value_t = DiffFormat::Table)]
    format: DiffFormat,
    /// Exit with 1 when a change at this severity or worse was found
    #[arg(long, value_enum)]
    fail_on: Option<FailOn>,
}

fn load(path: &Path) -> Result<Audit, CliError> {
    let bytes = std::fs::read(path)
        .map_err(|e| CliError::msg(format!("cannot read {}: {e}", path.display())))?;
    Audit::from_json(&bytes).map_err(|e| CliError::msg(format!("{}: {e}", path.display())))
}

pub fn run(args: DiffArgs) -> Outcome {
    let before = load(&args.before)?;
    let after = load(&args.after)?;
    let key = key_pages(&before.snapshot, &HashSet::new());
    let changes = diff(&before.snapshot, &after.snapshot, &key);

    let mut w = open_output(None)?;
    if let Some(note) = incomplete_note(&before.snapshot.stop, &after.snapshot.stop) {
        eprintln!("{note}");
        // JSON stays a plain array of changes.
        if args.format != DiffFormat::Json {
            writeln!(w, "{note}\n")?;
        }
    }
    write_changes(&mut w, args.format, &changes)?;
    w.flush()?;

    let failed = args
        .fail_on
        .is_some_and(|threshold| changes.iter().any(|c| threshold.reached_by(c.severity)));
    Ok(if failed { EXIT_FAIL_ON } else { EXIT_OK })
}

/// Why a crawl ended, in a few words for the note.
fn stop_words(stop: &StopReason) -> String {
    match stop {
        StopReason::Completed => "completed".to_owned(),
        StopReason::PageLimit => "page limit".to_owned(),
        StopReason::TimeLimit => "time limit".to_owned(),
        StopReason::Unreachable(why) => format!("site unreachable: {}", clean(why)),
        StopReason::Blocked(why) => format!("blocked: {}", clean(why)),
        StopReason::RobotsBlocked => "blocked by robots.txt".to_owned(),
    }
}

/// A one-line note when either crawl did not complete. The diff then leaves out new URLs
/// (older crawl incomplete) and removed URLs (newer crawl incomplete).
fn incomplete_note(before: &StopReason, after: &StopReason) -> Option<String> {
    let note = match (before.is_complete(), after.is_complete()) {
        (true, true) => return None,
        (false, true) => format!(
            "the older crawl stopped early ({}); new URLs were not compared",
            stop_words(before)
        ),
        (true, false) => format!(
            "the newer crawl stopped early ({}); removed URLs were not compared",
            stop_words(after)
        ),
        (false, false) if before == after => format!(
            "both crawls stopped early ({}); new and removed URLs were not compared",
            stop_words(after)
        ),
        (false, false) => format!(
            "both crawls stopped early (older: {}, newer: {}); new and removed URLs were not compared",
            stop_words(before),
            stop_words(after)
        ),
    };
    Some(format!("Note: {note}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_names_the_incomplete_crawl_and_what_was_skipped() {
        let done = StopReason::Completed;
        assert_eq!(incomplete_note(&done, &done), None);
        assert_eq!(
            incomplete_note(&StopReason::TimeLimit, &done).as_deref(),
            Some("Note: the older crawl stopped early (time limit); new URLs were not compared.")
        );
        assert_eq!(
            incomplete_note(&done, &StopReason::RobotsBlocked).as_deref(),
            Some(
                "Note: the newer crawl stopped early (blocked by robots.txt); removed URLs were not compared."
            )
        );
        assert_eq!(
            incomplete_note(&StopReason::PageLimit, &StopReason::TimeLimit).as_deref(),
            Some(
                "Note: both crawls stopped early (older: page limit, newer: time limit); new and removed URLs were not compared."
            )
        );
        let hostile = StopReason::Unreachable("x\x1b[2Jy".to_owned());
        assert!(!incomplete_note(&done, &hostile).unwrap().contains('\x1b'));
    }
}
