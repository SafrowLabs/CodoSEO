//! `codoseo redirects`: every hop from a URL to where it settles.

use std::io::Write;

use clap::Args;
use codoseo_core::crawl::AddressPolicy;
use codoseo_crawler::fetch::{FetchError, Fetcher, FetcherConfig, Hop};
use serde::Serialize;
use url::Url;

use super::output::{clean, write_json};
use super::{EXIT_OK, Outcome, PlainFormat};

#[derive(Debug, Args)]
pub struct RedirectsArgs {
    /// The address to follow
    url: Url,
    #[arg(long, value_enum, default_value_t = PlainFormat::Table)]
    format: PlainFormat,
}

#[derive(Debug, Serialize)]
struct HopReport {
    status: u16,
    url: Url,
}

#[derive(Debug, Serialize)]
struct RedirectReport {
    hops: Vec<HopReport>,
    final_status: Option<u16>,
    final_url: Option<Url>,
    /// `redirect_loop` or `too_many_redirects` when the chain never settled.
    problem: Option<&'static str>,
}

fn hops(chain: Vec<Hop>) -> Vec<HopReport> {
    chain
        .into_iter()
        .map(|h| HopReport {
            status: h.status,
            url: h.url,
        })
        .collect()
}

pub async fn run(args: RedirectsArgs) -> Outcome {
    let fetcher = Fetcher::new(FetcherConfig::new(AddressPolicy::AllowPrivate))?;
    // A chain that never settles is a finding to show, not a failure of the command.
    let report = match fetcher.fetch(&args.url).await {
        Ok(res) => RedirectReport {
            hops: hops(res.chain),
            final_status: Some(res.status),
            final_url: Some(res.final_url),
            problem: None,
        },
        Err(FetchError::RedirectLoop { chain }) => settled_not(chain, "redirect_loop"),
        Err(FetchError::TooManyRedirects { chain }) => settled_not(chain, "too_many_redirects"),
        Err(e) => return Err(e.into()),
    };

    let mut w = std::io::stdout().lock();
    match args.format {
        PlainFormat::Json => write_json(&mut w, &report)?,
        PlainFormat::Table => write_table(&mut w, &report)?,
    }
    w.flush()?;
    Ok(EXIT_OK)
}

fn settled_not(chain: Vec<Hop>, problem: &'static str) -> RedirectReport {
    RedirectReport {
        hops: hops(chain),
        final_status: None,
        final_url: None,
        problem: Some(problem),
    }
}

fn write_table(w: &mut impl Write, r: &RedirectReport) -> std::io::Result<()> {
    for hop in &r.hops {
        writeln!(w, "{}  {}", hop.status, clean(hop.url.as_str()))?;
    }
    if let (Some(status), Some(url)) = (r.final_status, &r.final_url) {
        writeln!(w, "{status}  {}", clean(url.as_str()))?;
    }
    match r.problem {
        Some("redirect_loop") => {
            writeln!(w, "loop  the redirects lead back to a URL already visited")?
        }
        Some(_) => writeln!(w, "stop  too many redirects")?,
        None => {}
    }
    Ok(())
}
