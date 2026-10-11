//! `codoseo probe`: fetch one page as each AI search and user-triggered bot, with its documented
//! user agent, and compare every response with a CodoSEObot baseline. Synthetic: the requests come
//! from this machine, not from the bots.

use std::io::Write;

use clap::{Args, ValueEnum};
use codoseo_core::crawl::AddressPolicy;
use codoseo_crawler::probe::{DEFAULT_MIN_GAP, PAGE_PROBE_DEADLINE, probe_from_env, probe_page};
use codoseo_geo::Intent;
use codoseo_geo::findings::{Finding, FindingKind, findings};
use codoseo_geo::probe::ProbeReport;
use serde::Serialize;
use url::Url;

use super::output::write_probe;
use super::{CliError, EXIT_OK, Outcome};

/// `--format` for `probe`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProbeFormat {
    Table,
    Json,
    #[value(alias = "md")]
    Markdown,
}

#[derive(Debug, Args)]
pub struct ProbeArgs {
    /// The page to probe
    url: Url,
    /// Only these bots, comma-separated (default: every probeable bot)
    #[arg(long, value_delimiter = ',')]
    bots: Option<Vec<String>>,
    #[arg(long, value_enum, default_value_t = ProbeFormat::Table)]
    format: ProbeFormat,
}

/// What `codoseo probe` prints: the probe report, the probed page and the `fetch_blocked` findings
/// under the default intent. `report.urls[].url` indexes `pages`.
#[derive(Debug, Serialize)]
pub struct ProbeOutput {
    pub url: String,
    #[serde(flatten)]
    pub report: ProbeReport,
    /// The pages `report.urls[].url` points at.
    pub pages: Vec<String>,
    pub findings: Vec<Finding>,
}

pub async fn run(args: ProbeArgs) -> Outcome {
    // Like `crawl`, a self-run command may probe private and internal addresses.
    let probed = probe_page(
        &args.url,
        args.bots.as_deref(),
        AddressPolicy::AllowPrivate,
        &probe_from_env(),
        DEFAULT_MIN_GAP,
        PAGE_PROBE_DEADLINE,
    )
    .await
    .map_err(CliError::msg)?;
    let found = findings(&probed.report, &Intent::default())
        .into_iter()
        .filter(|f| f.kind == FindingKind::FetchBlocked)
        .collect();
    let output = ProbeOutput {
        url: probed.page_url().to_owned(),
        report: probed.probes().clone(),
        pages: probed
            .report
            .important
            .iter()
            .map(|u| u.url.clone())
            .collect(),
        findings: found,
    };
    let mut w = std::io::stdout().lock();
    write_probe(&mut w, args.format, &output)?;
    w.flush()?;
    Ok(EXIT_OK)
}
