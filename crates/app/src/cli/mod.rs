//! Argument definitions and dispatch.

mod all;
mod check;
mod crawl;
mod diff;
mod mcp;
mod migrate;
mod output;
mod redirects;
mod robots;
mod web;
mod worker;

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use codoseo_core::check::Severity;

/// What a command did: its exit code, or the reason it could not run.
pub use output::clean;

pub type Outcome = Result<u8, CliError>;

pub const EXIT_OK: u8 = 0;
/// A `--fail-on` threshold was reached.
pub const EXIT_FAIL_ON: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Csv(#[from] csv::Error),
    #[error(transparent)]
    Crawl(#[from] codoseo_crawler::crawl::CrawlError),
    #[error(transparent)]
    Fetch(#[from] codoseo_crawler::fetch::FetchError),
}

impl CliError {
    pub fn msg(text: impl Into<String>) -> CliError {
        CliError::Message(text.into())
    }

    /// The reader of our output went away (`codoseo ... | head`): not worth an error.
    pub fn is_broken_pipe(&self) -> bool {
        let broken = |kind: io::ErrorKind| kind == io::ErrorKind::BrokenPipe;
        match self {
            CliError::Io(e) => broken(e.kind()),
            CliError::Json(e) => e.io_error_kind().is_some_and(broken),
            CliError::Csv(e) => matches!(e.kind(), csv::ErrorKind::Io(e) if broken(e.kind())),
            _ => false,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "codoseo",
    version,
    about = "A fast, polite SEO crawler and site auditor",
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Crawl a site, run the checks and print a report
    Crawl(crawl::CrawlArgs),
    /// Inspect one page: its fields, redirect chain and page issues
    Check(check::CheckArgs),
    /// Show a site's robots.txt and whether CodoSEObot may fetch a path
    Robots(robots::RobotsArgs),
    /// Follow a URL's redirects and show each hop
    Redirects(redirects::RedirectsArgs),
    /// Compare two saved audits (`crawl --format json`)
    Diff(diff::DiffArgs),
    /// Run the local MCP server over stdio
    Mcp(mcp::McpArgs),
    /// Apply every pending Postgres migration from DATABASE_URL
    Migrate(migrate::MigrateArgs),
    /// Claim and run crawls from the Postgres queue until SIGTERM
    Worker(worker::WorkerArgs),
    /// Serve the web app (CODOSEO_MODE, DATABASE_URL, BASE_URL, ...)
    Web(web::WebArgs),
    /// Self-hosting in one process: migrate, then run the web app and a worker together
    All(all::AllArgs),
}

pub async fn run(cli: Cli) -> Outcome {
    match cli.command {
        Command::Crawl(args) => crawl::run(args).await,
        Command::Check(args) => check::run(args).await,
        Command::Robots(args) => robots::run(args).await,
        Command::Redirects(args) => redirects::run(args).await,
        Command::Diff(args) => diff::run(args),
        Command::Mcp(args) => mcp::run(args).await,
        Command::Migrate(args) => migrate::run(args).await,
        Command::Worker(args) => worker::run(args).await,
        Command::Web(args) => web::run(args).await,
        Command::All(args) => all::run(args).await,
    }
}

/// `--format` for `crawl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CrawlFormat {
    Table,
    Json,
    Md,
    Csv,
}

/// `--format` for `diff`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DiffFormat {
    Table,
    Json,
    Md,
}

/// `--format` for the single-URL commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PlainFormat {
    Table,
    Json,
}

/// The `--fail-on` threshold: a failing check or a change at this severity or worse
/// ends the command with exit code 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FailOn {
    Critical,
    Warning,
}

impl FailOn {
    /// `Severity` orders most severe first, so "this bad or worse" is `<=`.
    pub fn reached_by(self, severity: Severity) -> bool {
        let threshold = match self {
            FailOn::Critical => Severity::Critical,
            FailOn::Warning => Severity::Warning,
        };
        severity <= threshold
    }
}

/// Exit code for a command that could not do its job.
pub const EXIT_RUNTIME: u8 = 2;

/// Opens `-o FILE` or stdout for a rendered report.
pub fn open_output(path: Option<&PathBuf>) -> Result<Box<dyn Write>, CliError> {
    Ok(match path {
        Some(p) => {
            Box::new(BufWriter::new(File::create(p).map_err(|e| {
                CliError::msg(format!("cannot write {}: {e}", p.display()))
            })?))
        }
        None => Box::new(BufWriter::new(io::stdout().lock())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer whose reader has gone away.
    struct ClosedPipe;

    impl Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }

    #[test]
    fn broken_pipe_is_recognised_through_every_writer() {
        let io_err = CliError::from(ClosedPipe.write_all(b"x").unwrap_err());
        assert!(io_err.is_broken_pipe());
        let json_err = CliError::from(serde_json::to_writer(ClosedPipe, &[1, 2]).unwrap_err());
        assert!(json_err.is_broken_pipe());
        let mut csv = csv::Writer::from_writer(ClosedPipe);
        csv.write_record(["a", "b"]).unwrap();
        let csv_err = CliError::from(csv.flush().map_err(csv::Error::from).unwrap_err());
        assert!(csv_err.is_broken_pipe());

        assert!(!CliError::msg("nope").is_broken_pipe());
        let other = CliError::from(io::Error::from(io::ErrorKind::PermissionDenied));
        assert!(!other.is_broken_pipe());
    }
}
