//! Shared types for CodoSEO. No I/O lives here.

pub mod audit;
pub mod change;
pub mod check;
pub mod crawl;
pub mod output;
pub mod page;
pub mod plan;
pub mod report;
pub mod snapshot;
pub mod url;

pub use ::url::Url;
