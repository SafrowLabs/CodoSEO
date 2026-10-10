//! `codoseo robots`: what robots.txt says to CodoSEObot.

use std::io::Write;

use clap::Args;
use codoseo_core::crawl::AddressPolicy;
use codoseo_crawler::fetch::{Fetcher, FetcherConfig};
use codoseo_crawler::robots::fetch_robots;
use codoseo_geo::registry::Honours;
use codoseo_geo::report::{BotVerdict, RobotsDeclared, bot_verdicts};
use codoseo_geo::robots::{Pair, RobotsTxt};
use serde::Serialize;
use url::Url;

use super::bots::purpose_label;
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
    /// Each registry bot's verdict for the path; empty when robots.txt could not be read (5xx).
    bots: Vec<BotVerdict>,
    declared: RobotsDeclared,
}

pub async fn run(args: RobotsArgs) -> Outcome {
    let fetcher = Fetcher::new(FetcherConfig::new(AddressPolicy::AllowPrivate))?;
    let (rules, file) = fetch_robots(&fetcher, &args.url)
        .await
        .map_err(|e| CliError::msg(format!("could not fetch robots.txt: {e}")))?;
    let path = args.path.unwrap_or_else(|| args.url.path().to_owned());
    let parsed = RobotsTxt::from_response(Some(file.status), file.body.as_bytes());
    let (bots, declared) = match &parsed {
        Some(txt) => (bot_verdicts(txt, &path), RobotsDeclared::of(txt)),
        None => (Vec::new(), RobotsDeclared::default()),
    };
    let report = RobotsReport {
        status: file.status,
        allowed: rules.allowed(&path),
        path,
        crawl_delay_secs: rules.crawl_delay().map(|d| d.as_secs_f64()),
        sitemaps: rules.sitemaps().to_vec(),
        bots,
        declared,
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
    write_ai_bots(w, r)
}

fn pairs_text(pairs: &[Pair]) -> String {
    pairs
        .iter()
        .map(|p| format!("{}={}", p.key, p.value))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The AI bots section: one row per registry bot, then the declared preferences.
fn write_ai_bots(w: &mut impl Write, r: &RobotsReport) -> std::io::Result<()> {
    writeln!(w, "\nAI bots (path {})", clean(&r.path))?;
    if r.bots.is_empty() {
        return writeln!(
            w,
            "  No verdicts: robots.txt could not be read (HTTP {}), so crawlers are told to stay away for now",
            r.status
        );
    }
    let width = |f: fn(&BotVerdict) -> usize| r.bots.iter().map(f).max().unwrap_or(0);
    let token = width(|b| b.token.chars().count()).max("Token".len());
    let operator = width(|b| b.operator.chars().count()).max("Operator".len());
    writeln!(
        w,
        "  {:<token$}  {:<operator$}  {:<10}  Verdict",
        "Token", "Operator", "Purpose"
    )?;
    for b in &r.bots {
        let verdict = if b.allowed { "Allowed" } else { "Blocked" };
        let by = b.line.map_or_else(String::new, |n| format!("  line {n}"));
        let ignore = if b.honours_robots == Honours::Yes {
            ""
        } else {
            "  may ignore robots.txt"
        };
        writeln!(
            w,
            "  {:<token$}  {:<operator$}  {:<10}  {verdict}{by}{ignore}",
            b.token,
            b.operator,
            purpose_label(b.purpose),
        )?;
    }
    for s in &r.declared.content_signals {
        writeln!(
            w,
            "  Content-Signal (line {}): {}",
            s.line,
            clean(&pairs_text(&s.pairs))
        )?;
    }
    for u in &r.declared.content_usage {
        let scope = u
            .path
            .as_deref()
            .map_or_else(String::new, |p| format!(" {p}"));
        writeln!(
            w,
            "  Content-Usage (line {}):{} {}",
            u.line,
            clean(&scope),
            clean(&pairs_text(&u.pairs))
        )?;
    }
    Ok(())
}
