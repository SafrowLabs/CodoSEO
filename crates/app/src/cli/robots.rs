//! `codoseo robots`: what robots.txt says to CodoSEObot.

use std::io::Write;

use clap::Args;
use codoseo_core::crawl::AddressPolicy;
use codoseo_crawler::fetch::{Fetcher, FetcherConfig};
use codoseo_crawler::robots::fetch_robots;
use serde::Serialize;
use url::Url;

use super::output::{clean, write_json, write_pairs};
use super::{CliError, EXIT_OK, Outcome, PlainFormat};

#[derive(Debug, Args)]
pub struct RobotsArgs {
    /// Any address on the site
    url: Url,
    /// The path to test (default: the address's own path)
    #[arg(long)]
    path: Option<String>,
    #[arg(long, value_enum, default_value_t = PlainFormat::Table)]
    format: PlainFormat,
}

#[derive(Debug, Serialize)]
struct RobotsReport {
    status: u16,
    path: String,
    allowed: bool,
    crawl_delay_secs: Option<f64>,
    sitemaps: Vec<String>,
}

pub async fn run(args: RobotsArgs) -> Outcome {
    let fetcher = Fetcher::new(FetcherConfig::new(AddressPolicy::AllowPrivate))?;
    let (rules, file) = fetch_robots(&fetcher, &args.url)
        .await
        .map_err(|e| CliError::msg(format!("could not fetch robots.txt: {e}")))?;
    let path = args.path.unwrap_or_else(|| args.url.path().to_owned());
    let report = RobotsReport {
        status: file.status,
        allowed: rules.allowed(&path),
        path,
        crawl_delay_secs: rules.crawl_delay().map(|d| d.as_secs_f64()),
        sitemaps: rules.sitemaps().to_vec(),
    };

    let mut w = std::io::stdout().lock();
    match args.format {
        PlainFormat::Json => write_json(&mut w, &report)?,
        PlainFormat::Table => write_table(&mut w, &report)?,
    }
    w.flush()?;
    Ok(EXIT_OK)
}

fn write_table(w: &mut impl Write, r: &RobotsReport) -> std::io::Result<()> {
    let pairs = [
        ("robots.txt", format!("HTTP {}", r.status)),
        ("Path", r.path.clone()),
        (
            "CodoSEObot",
            if r.allowed { "allowed" } else { "blocked" }.to_owned(),
        ),
        (
            "Crawl-delay",
            r.crawl_delay_secs
                .map_or_else(|| "none".to_owned(), |s| format!("{s} s")),
        ),
        (
            "Sitemaps",
            if r.sitemaps.is_empty() {
                "none".to_owned()
            } else {
                String::new()
            },
        ),
    ];
    write_pairs(w, "", &pairs)?;
    for sitemap in &r.sitemaps {
        writeln!(w, "  {}", clean(sitemap))?;
    }
    Ok(())
}
