//! Argument definitions and dispatch.

mod check;
mod crawl;
mod diff;
mod output;
mod redirects;
mod robots;

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use codoseo_core::check::Severity;

/// What a command did: its exit code, or the reason it could not run.
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
}

pub async fn run(cli: Cli) -> Outcome {
    match cli.command {
        Command::Crawl(args) => crawl::run(args).await,
        Command::Check(args) => check::run(args).await,
        Command::Robots(args) => robots::run(args).await,
        Command::Redirects(args) => redirects::run(args).await,
        Command::Diff(args) => diff::run(args),
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
