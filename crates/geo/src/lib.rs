//! AI-access foundations for CodoSEO: the public registry of AI crawlers and agents, and (in later
//! modules) robots.txt verdicts and page-level answer eligibility per engine.

pub mod declared;
pub mod registry;
pub mod robots;

pub use registry::{Bot, Honours, Purpose, Registry, registry, registry_json};
