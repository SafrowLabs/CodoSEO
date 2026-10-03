//! The CodoSEO crawler. No database: the CLI, local MCP and the worker all use it directly.

pub mod extract;
pub mod fetch;
pub mod guard;
