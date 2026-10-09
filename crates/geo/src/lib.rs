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
