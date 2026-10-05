//! The cloud side of the MCP surface: the wire types shared by the REST API (`/api/v1`) and the
//! cloud MCP tools, so both return identical JSON, and the cloud MCP server itself. The web
//! crate owns the service behind it and implements [`handler::CloudBackend`].

pub mod handler;
mod keyed;
pub mod types;

pub use handler::{Caller, CloudBackend, CloudMcp};
