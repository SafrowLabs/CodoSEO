//! The CodoSEO crawler. No database: the CLI, local MCP and the worker all use it directly.

pub mod crawl;
pub mod extract;
pub mod fetch;
pub mod frontier;
pub mod guard;
pub mod politeness;
pub mod preflight;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the crawl orchestrator in a later task")
)]
pub(crate) mod record;
pub mod robots;
pub mod scope;
pub mod sitemap;
