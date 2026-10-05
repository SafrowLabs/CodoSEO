//! The cloud side of the MCP surface: the wire types shared by the REST API (`/api/v1`) and the
//! cloud MCP tools, so both return identical JSON. The web crate owns the service behind them.

pub mod types;
