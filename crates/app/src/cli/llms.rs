//! `codoseo llms-txt`: fetch a site's `/llms.txt` and check it against the llmstxt.org format and
//! Lighthouse's audit. Informational: the file is optional, nothing depends on it and the exit
//! code is 0 whatever it holds.

use std::io::Write;

use clap::{Args, ValueEnum};
use codoseo_core::crawl::{AddressPolicy, CrawlLimits};
use codoseo_crawler::llms::{PAGE_CHECK_DEADLINE, check_llms_txt};
use codoseo_geo::llms::{LlmsTxtReport, assess};
use serde::Serialize;
use url::Url;

use super::output::write_llms;
use super::{CliError, EXIT_OK, Outcome};

/// `--format` for `llms-txt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LlmsFormat {
    Table,
    Json,
    #[value(alias = "md")]
    Markdown,
}

#[derive(Debug, Args)]
pub struct LlmsArgs {
    /// Any page of the site; /llms.txt is read from its origin
    url: Url,
    #[arg(long, value_enum, default_value_t = LlmsFormat::Table)]
    format: LlmsFormat,
}

/// What `codoseo llms-txt` prints.
#[derive(Debug, Serialize)]
pub struct LlmsOutput {
    /// The URL that was fetched.
    pub url: String,
    /// Always true: llms.txt is informational and never scored.
    pub informational: bool,
    pub summary: String,
    #[serde(flatten)]
    pub report: LlmsTxtReport,
}

pub async fn run(args: LlmsArgs) -> Outcome {
    // Like `crawl`, a self-run command may read private and internal addresses.
    let found = check_llms_txt(
        &args.url,
        AddressPolicy::AllowPrivate,
        CrawlLimits::default().llms_link_checks,
        PAGE_CHECK_DEADLINE,
    )
    .await
    .map_err(CliError::msg)?;
    let report = assess(&found.fetch, false);
    let output = LlmsOutput {
        url: found.url,
        informational: true,
        summary: report.summary(),
        report,
    };
    let mut w = std::io::stdout().lock();
    write_llms(&mut w, args.format, &output)?;
    w.flush()?;
    Ok(EXIT_OK)
}
