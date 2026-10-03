//! CSV: one row per page.

use std::io::Write;

use codoseo_core::audit::Audit;
use codoseo_core::check::CheckId;
use codoseo_core::page::PageRecord;

use super::{CliError, slug};

pub(super) fn crawl(w: &mut impl Write, audit: &Audit) -> Result<(), CliError> {
    let mut out = csv::Writer::from_writer(&mut *w);
    out.write_record([
        "url",
        "status",
        "indexability",
        "title",
        "meta_description",
        "h1",
        "word_count",
        "depth",
        "inlinks",
        "response_ms",
        "in_sitemap",
        "issues",
    ])?;
    for p in &audit.snapshot.pages {
        out.write_record(csv_row(p))?;
    }
    out.flush()?;
    Ok(())
}

fn csv_row(p: &PageRecord) -> [String; 12] {
    let issues: Vec<&str> = p
        .issues
        .iter()
        .filter_map(CheckId::from_bit)
        .map(CheckId::slug)
        .collect();
    [
        p.url.to_string(),
        p.status.to_string(),
        slug(&p.indexability),
        p.fields.title.clone().unwrap_or_default(),
        p.fields.meta_description.clone().unwrap_or_default(),
        p.fields.h1.first().cloned().unwrap_or_default(),
        p.fields.word_count.to_string(),
        p.depth.map(|d| d.to_string()).unwrap_or_default(),
        p.inlinks.to_string(),
        p.response_ms.to_string(),
        p.in_sitemap.to_string(),
        issues.join(";"),
    ]
}
