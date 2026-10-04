//! The CodoSEO crawler. No database: the CLI, local MCP and the worker all use it directly.

pub use codoseo_core::output::*;

pub mod crawl;
pub mod extract;
pub mod fetch;
pub mod frontier;
pub mod guard;
pub mod politeness;
pub mod preflight;
pub(crate) mod record;
pub mod robots;
pub mod scope;
pub mod sitemap;
