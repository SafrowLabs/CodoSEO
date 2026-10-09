//! `codoseo check`: one page, its fields, redirect chain and page issues.

use std::io::Write;

use clap::Args;
use codoseo_checks::{check_page, def};
use codoseo_core::check::CheckId;
use codoseo_core::crawl::{AddressPolicy, CrawlConfig, CrawlLimits, Politeness, USER_AGENT};
use codoseo_core::page::PageRecord;
use codoseo_crawler::crawl::inspect_page;
use url::Url;

use super::output::{clean, severity_label, slug, write_json, write_pairs};
use super::{CliError, EXIT_OK, EXIT_RUNTIME, Outcome, PlainFormat};

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// The page to inspect
    url: Url,
    #[arg(long, value_enum, default_value_t = PlainFormat::Table)]
    format: PlainFormat,
}

pub async fn run(args: CheckArgs) -> Outcome {
    let cfg = CrawlConfig {
        start_url: args.url,
        limits: CrawlLimits::default(),
        politeness: Politeness::default(),
        address_policy: AddressPolicy::AllowPrivate,
        user_agent: USER_AGENT.to_owned(),
        site_signals: true,
    };
    let mut page = inspect_page(&cfg).await?;
    check_page(&mut page);

    let mut w = std::io::stdout().lock();
    match args.format {
        PlainFormat::Json => write_json(&mut w, &page)?,
        PlainFormat::Table => write_table(&mut w, &page).map_err(CliError::from)?,
    }
    w.flush()?;

    // No response at all: the record is printed, but the command could not do its job.
    if let Some(error) = &page.error {
        eprintln!("the page could not be fetched ({})", slug(error));
        return Ok(EXIT_RUNTIME);
    }
    Ok(EXIT_OK)
}

fn or_dash(value: Option<&str>) -> String {
    value.map_or_else(|| "-".to_owned(), str::to_owned)
}

fn write_table(w: &mut impl Write, p: &PageRecord) -> std::io::Result<()> {
    let f = &p.fields;
    let mut pairs = vec![
        ("URL", p.url.to_string()),
        ("Status", p.status.to_string()),
        ("Indexability", slug(&p.indexability)),
        ("Title", or_dash(f.title.as_deref())),
        ("Meta description", or_dash(f.meta_description.as_deref())),
        ("H1", or_dash(f.h1.first().map(String::as_str))),
        ("Canonical", or_dash(f.canonical.as_ref().map(Url::as_str))),
        ("Meta robots", or_dash(f.meta_robots.as_deref())),
        ("X-Robots-Tag", or_dash(f.x_robots_tag.as_deref())),
        ("Word count", f.word_count.to_string()),
        ("Response time", format!("{} ms", p.response_ms)),
        ("Size", format!("{} bytes", p.size_bytes)),
        ("Content type", or_dash(p.content_type.as_deref())),
        (
            "Links out",
            format!(
                "{} internal, {} external",
                p.outlinks_internal, p.outlinks_external
            ),
        ),
    ];
    if let Some(error) = &p.error {
        pairs.insert(2, ("Error", slug(error)));
    }
    write_pairs(w, "", &pairs)?;

    writeln!(w, "\nRedirect chain")?;
    if p.redirect_chain.is_empty() {
        writeln!(w, "  none")?;
    }
    for (status, url) in &p.redirect_chain {
        writeln!(w, "  {status}  {}", clean(url.as_str()))?;
    }
    if let Some(target) = &p.redirect_target {
        writeln!(w, "  → {}", clean(target.as_str()))?;
    }

    let mut issues: Vec<_> = p
        .issues
        .iter()
        .filter_map(CheckId::from_bit)
        .map(def)
        .collect();
    issues.sort_by_key(|d| (d.severity, d.id));
    writeln!(w, "\nPage issues ({})", issues.len())?;
    let width = issues
        .iter()
        .map(|d| d.title.chars().count())
        .max()
        .unwrap_or(0);
    for d in issues {
        writeln!(
            w,
            "  {:<8}  {:<width$}  {}",
            severity_label(d.severity),
            d.title,
            d.id.slug()
        )?;
    }
    Ok(())
}
