//! `codoseo bots`: the registry of AI crawlers, fetchers and control tokens CodoSEO knows.

use std::io::Write;

use clap::Args;
use codoseo_geo::registry::{Honours, Purpose, registry, registry_json};

use super::{EXIT_OK, Outcome, PlainFormat};

#[derive(Debug, Args)]
pub struct BotsArgs {
    /// `json` prints the registry file as published at /ai-bots.json
    #[arg(long, value_enum, default_value_t = PlainFormat::Table)]
    format: PlainFormat,
}

pub fn run(args: BotsArgs) -> Outcome {
    let mut w = std::io::stdout().lock();
    match args.format {
        PlainFormat::Json => {
            w.write_all(registry_json().as_bytes())?;
            if !registry_json().ends_with('\n') {
                writeln!(w)?;
            }
        }
        PlainFormat::Table => write_table(&mut w)?,
    }
    w.flush()?;
    Ok(EXIT_OK)
}

pub(super) fn purpose_label(p: Purpose) -> &'static str {
    match p {
        Purpose::Search => "search",
        Purpose::UserFetch => "user fetch",
        Purpose::Agent => "agent",
        Purpose::Training => "training",
        Purpose::Ads => "ads",
    }
}

fn honours_label(h: Honours) -> &'static str {
    match h {
        Honours::Yes => "yes",
        Honours::Partial => "partial",
        Honours::No => "no",
        Honours::Unknown => "unknown",
    }
}

fn write_table(w: &mut impl Write) -> std::io::Result<()> {
    let reg = registry();
    let width = |f: fn(&codoseo_geo::Bot) -> usize| reg.bots.iter().map(f).max().unwrap_or(0);
    let token = width(|b| b.token.chars().count()).max("Token".len());
    let operator = width(|b| b.operator.chars().count()).max("Operator".len());
    writeln!(
        w,
        "{:<token$}  {:<operator$}  {:<10}  Honours robots.txt",
        "Token", "Operator", "Purpose"
    )?;
    for b in &reg.bots {
        writeln!(
            w,
            "{:<token$}  {:<operator$}  {:<10}  {}",
            b.token,
            b.operator,
            purpose_label(b.purpose),
            honours_label(b.honours_robots)
        )?;
    }
    writeln!(
        w,
        "\n{} bots, updated {}, licence {}. More at {}",
        reg.bots.len(),
        reg.updated,
        reg.license,
        reg.homepage
    )
}
