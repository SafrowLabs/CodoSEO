//! CSV: one row per page.

use std::io::Write;

use codoseo_core::audit::Audit;
use codoseo_core::check::CheckId;
use codoseo_core::page::PageRecord;

use super::{CliError, clean, slug};

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

/// A crawled text cell: control characters dropped, and a leading `'` when a spreadsheet
/// would read the cell as a formula (`=`, `+`, `-`, `@`, tab or CR first).
fn text_cell(text: Option<&str>) -> String {
    let cell = clean(text.unwrap_or_default());
    if cell.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{cell}")
    } else {
        cell
    }
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
        text_cell(p.fields.title.as_deref()),
        text_cell(p.fields.meta_description.as_deref()),
        text_cell(p.fields.h1.first().map(String::as_str)),
        p.fields.word_count.to_string(),
        p.depth.map(|d| d.to_string()).unwrap_or_default(),
        p.inlinks.to_string(),
        p.response_ms.to_string(),
        p.in_sitemap.to_string(),
        issues.join(";"),
    ]
}
