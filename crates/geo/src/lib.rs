//! AI-access foundations for CodoSEO: the public registry of AI crawlers and agents,
//! robots.txt verdicts, page-level answer eligibility per engine, the owner's intent, the access
//! report built from a crawl, the findings drawn from it and the incident lifecycle.

pub mod declared;
pub mod eligibility;
pub mod findings;
pub mod incident;
pub mod intent;
pub mod registry;
pub mod report;
pub mod robots;

pub use intent::{Intent, Stance};
pub use registry::{Bot, Honours, Purpose, Registry, registry, registry_json};

use codoseo_core::output::CrawlOutput;

/// The AI access section of a saved audit (`Audit::ai_access`), as `codoseo crawl` and the local
/// MCP server's `audit_site` both write it: the report over the crawl's important pages (no
/// starred pages: there is no site to star them on) and its findings under the default intent
/// (there is nowhere to keep an owner's), as `{ "report": …, "findings": … }`.
pub fn assess_with_defaults(out: &CrawlOutput) -> serde_json::Value {
    let important = report::important_urls(&out.pages, &out.origin, &Default::default());
    let access = report::build_report(out, &important);
    let found = findings::findings(&access, &Intent::default());
    serde_json::json!({ "report": access, "findings": found })
}
