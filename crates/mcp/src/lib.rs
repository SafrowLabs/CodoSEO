//! CodoSEO's local MCP server: the `Backend` trait, a JSON audit cache, a backend that
//! runs crawls directly (no database), and the MCP tool surface over `rmcp`.

pub mod backend;
pub mod cache;
pub mod cloud;
pub mod local;
pub mod tools;
pub mod types;

pub use backend::{Backend, BackendError};
pub use local::LocalBackend;
pub use tools::CodoseoMcp;
