//! The CodoSEO check registry and the checks themselves.
//!
//! [`CHECKS`] describes all 44 checks (severity, category, scope, title). Per-page checks
//! live in [`page`] and are run with [`page_issues`].

pub mod page;
mod registry;

pub use page::page_issues;
pub use registry::{CHECKS, Category, CheckDef, Scope, def};
