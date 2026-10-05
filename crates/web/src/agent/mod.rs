//! What agents use to reach CodoSEO in the cloud: API keys, the REST API's service layer
//! (shared with the MCP server, so both give the same JSON and charge the same quota) and the
//! Bearer authentication in front of them.

pub mod auth;
pub mod error;
pub mod keys;
pub mod service;
