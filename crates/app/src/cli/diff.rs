//! `codoseo diff`: what changed between two saved audits.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Args;
use codoseo_core::audit::Audit;
use codoseo_diff::{diff, key_pages};

use super::output::write_changes;
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
    write_changes(&mut w, args.format, &changes)?;
    w.flush()?;

    let failed = args
        .fail_on
        .is_some_and(|threshold| changes.iter().any(|c| threshold.reached_by(c.severity)));
    Ok(if failed { EXIT_FAIL_ON } else { EXIT_OK })
}
