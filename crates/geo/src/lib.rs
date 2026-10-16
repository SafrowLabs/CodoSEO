//! AI-access foundations for CodoSEO: the public registry of AI crawlers and agents,
//! robots.txt verdicts, page-level answer eligibility per engine, the owner's intent, the access
//! report built from a crawl, the findings drawn from it and the incident lifecycle.

pub mod content;
pub mod declared;
pub mod eligibility;
pub mod findings;
pub mod incident;
pub mod intent;
pub mod ledger;
pub mod llms;
pub mod markdown;
pub mod probe;
pub mod recheck;
pub mod registry;
pub mod remedy;
pub mod report;
pub mod robots;
pub mod score;
pub mod signals;

pub use intent::{Intent, Stance};
pub use registry::{Bot, Honours, Purpose, Registry, registry, registry_json};

use codoseo_core::output::CrawlOutput;

/// The AI access section of a saved audit (`Audit::ai_access`), as `codoseo crawl` and the local
/// MCP server's `audit_site` both write it: the report over the crawl's important pages (no
/// starred pages: there is no site to star them on) and its findings under the default intent
/// (there is nowhere to keep an owner's), as `{ "report": …, "findings": …, "remedies": … }`.
/// `remedies` has one entry per finding ([`remedy::FindingRemedy`]): a local run has no
/// incidents, so the fix hangs off the finding.
pub fn assess_with_defaults(out: &CrawlOutput) -> serde_json::Value {
    let important = report::important_urls(&out.pages, &out.origin, &Default::default());
    let access = report::build_report(out, &important);
    section(out.origin.as_str(), &access)
}

/// `{ "report", "findings", "remedies" }` for an access report under the default intent: the
/// shape of an audit's AI access section.
pub fn section(origin: &str, access: &report::AccessReport) -> serde_json::Value {
    section_with(origin, access, &Intent::default())
}

/// [`section`] under the owner's `intent` (a site policy file's AI rules).
pub fn section_with(
    origin: &str,
    access: &report::AccessReport,
    intent: &Intent,
) -> serde_json::Value {
    let found = findings::findings(access, intent);
    let remedies = remedy::finding_remedies(origin, access, &found);
    serde_json::json!({ "report": access, "findings": found, "remedies": remedies })
}
